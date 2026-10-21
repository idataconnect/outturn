//! A message an inbound mail service delivered to an email trigger's path:
//! taken apart, and held to the trigger's sender policy. See
//! docs/triggers.md, "Email".
//!
//! Anybody can send mail to an address, so a message is not merely untrusted
//! but unauthenticated in a way a signed webhook is not. Two checks stand
//! between it and a turn: the sender is one the trigger allows, and the
//! receiving server authenticated that sender's domain -- DMARC passed, or SPF
//! or DKIM passed for the domain in `From:`, as its own
//! `Authentication-Results` says.

use mail_parser::MessageParser;

/// What a turn is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    /// The sender's address, lower case.
    pub from: String,
    pub subject: String,
    /// The sender's id for the message, or a stand-in where it gave none.
    pub id: String,
    pub text: String,
    /// Whether the receiving server authenticated the domain in `from`.
    pub authenticated: bool,
}

/// Why a message started no turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    NotMail,
    NoSender,
    Unauthenticated(String),
    NotAllowed(String),
}

impl Refused {
    pub fn why(&self) -> String {
        match self {
            Refused::NotMail => "the body was not a mail message".into(),
            Refused::NoSender => "the message names no sender".into(),
            Refused::Unauthenticated(from) => format!(
                "{from}'s mail was not authenticated by the receiving server (no DMARC pass, nor an SPF or DKIM pass for its domain)"
            ),
            Refused::NotAllowed(from) => format!("{from} is not a sender this trigger allows"),
        }
    }
}

/// The most of a body an agent is given. Mail quotes the thread below every
/// reply, and the newest part is what was sent.
const MAX_TEXT: usize = 32 * 1024;

/// Parses a raw message, RFC 822 as the service delivered it.
pub fn parse(raw: &[u8]) -> Result<Email, Refused> {
    let message = MessageParser::default()
        .parse(raw)
        .ok_or(Refused::NotMail)?;
    let from = message
        .from()
        .and_then(|a| a.first())
        .and_then(|a| a.address())
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| a.contains('@'))
        .ok_or(Refused::NoSender)?;
    let subject = message.subject().unwrap_or("").trim().to_string();
    let text = match message.body_text(0) {
        Some(text) => text.into_owned(),
        None => message
            .body_html(0)
            .map(|html| strip_html(&html))
            .unwrap_or_default(),
    };
    let text: String = text.trim().chars().take(MAX_TEXT).collect();
    let id = match message.message_id() {
        Some(id) if !id.trim().is_empty() => id.trim().to_string(),
        // Missing or reused ids are not rare; the date, sender and subject
        // together stand in, so the same message delivered twice is one.
        _ => {
            use sha2::{Digest, Sha256};
            let date = message.date().map(|d| d.to_rfc3339()).unwrap_or_default();
            let digest = Sha256::digest(format!("{date}\n{from}\n{subject}"));
            format!("sha256:{}", hex::encode(&digest[..16]))
        }
    };
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
    let authenticated = authenticated(raw, domain);
    Ok(Email {
        from,
        subject,
        id,
        text,
        authenticated,
    })
}

/// Whether the topmost `Authentication-Results` header authenticated
/// `domain`, the one in `From:`.
///
/// The topmost only: the receiving server prepends its own, and anything
/// further down arrived with the message -- which a sender can write
/// whatever it likes into.
///
/// A pass counts only for the domain the allowlist is then held to. SPF and
/// DKIM vouch for whichever domain they checked, and a sender passing SPF for
/// its own domain can still write anybody's address into `From:`. So it is
/// `dmarc=pass`, which is that alignment already checked, or `dkim=pass` with
/// `header.d` aligned, or `spf=pass` with `smtp.mailfrom` aligned.
fn authenticated(raw: &[u8], domain: &str) -> bool {
    let head = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(end) => &raw[..end],
        None => match raw.windows(2).position(|w| w == b"\n\n") {
            Some(end) => &raw[..end],
            None => raw,
        },
    };
    let head = String::from_utf8_lossy(head);
    let mut value = String::new();
    let mut inside = false;
    for line in head.lines() {
        if inside {
            if line.starts_with(' ') || line.starts_with('\t') {
                value.push(' ');
                value.push_str(line.trim());
                continue;
            }
            break;
        }
        if let Some((name, rest)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("authentication-results")
        {
            value.push_str(rest.trim());
            inside = true;
        }
    }
    let value = strip_comments(&value.to_ascii_lowercase());
    let domain = domain.to_ascii_lowercase();
    // The first part is the server's own name; each after it is one result,
    // `method=result` and then `ptype.property=value` pairs.
    value.split(';').skip(1).any(|result| {
        let mut tokens = result.split_whitespace();
        let Some((method, verdict)) = tokens.next().and_then(|t| t.split_once('=')) else {
            return false;
        };
        if verdict != "pass" {
            return false;
        }
        let property = |name: &str| {
            result
                .split_whitespace()
                .find_map(|t| t.strip_prefix(name)?.strip_prefix('='))
        };
        match method {
            "dmarc" => true,
            "dkim" => property("header.d").is_some_and(|d| aligned(d, &domain)),
            "spf" => property("smtp.mailfrom")
                .map(|m| m.rsplit_once('@').map(|(_, d)| d).unwrap_or(m))
                .is_some_and(|d| aligned(d, &domain)),
            _ => false,
        }
    })
}

/// Whether a domain that passed vouches for the one in `From:`: the same, or a
/// parent of it, relaxed alignment's way. Without the public suffix list a
/// parent must be at least two labels, so a pass for a bare suffix vouches for
/// nothing.
fn aligned(passed: &str, from: &str) -> bool {
    let passed = passed.trim_end_matches('.');
    !passed.is_empty()
        && (passed == from
            || (passed.contains('.')
                && from
                    .strip_suffix(passed)
                    .is_some_and(|rest| rest.ends_with('.'))))
}

/// The header with its parenthesized comments blanked, since a comment may
/// say anything, `spf=pass` included.
fn strip_comments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;
    for c in value.chars() {
        match c {
            '(' => {
                depth += 1;
                out.push(' ');
            }
            ')' => {
                depth = depth.saturating_sub(1);
                out.push(' ');
            }
            _ if depth == 0 => out.push(c),
            _ => out.push(' '),
        }
    }
    out
}

/// Holds a message to a trigger's policy.
pub fn admit(
    email: &Email,
    senders: &[String],
    require_authenticated: bool,
) -> Result<(), Refused> {
    if require_authenticated && !email.authenticated {
        return Err(Refused::Unauthenticated(email.from.clone()));
    }
    let domain = email.from.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
    let allowed = senders.iter().any(|s| {
        let s = s.trim().to_ascii_lowercase();
        s == "*" || s == email.from || s.strip_prefix('@').is_some_and(|d| d == domain)
    });
    if allowed {
        Ok(())
    } else {
        Err(Refused::NotAllowed(email.from.clone()))
    }
}

/// What the agent is given: who, about what, and what they said.
pub fn prompt_text(email: &Email) -> String {
    format!(
        "From: {}\nSubject: {}\n\n{}",
        email.from, email.subject, email.text
    )
}

/// HTML as the text a person would read, crudely: tags out, entities for the
/// common few. Only for a message with no plain part.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(results: &str) -> Vec<u8> {
        format!(
            "{results}From: Jo Okafor <Jo@Example.com>\r\nTo: desk@example.com\r\n\
             Subject: Garden room\r\nMessage-ID: <abc@example.com>\r\n\
             Content-Type: text/plain\r\n\r\nIs it free on the 24th?\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn a_message_is_taken_apart() {
        let email = parse(&message(
            "Authentication-Results: mx.example.net;\r\n spf=pass smtp.mailfrom=example.com\r\n",
        ))
        .unwrap();
        assert_eq!(email.from, "jo@example.com");
        assert_eq!(email.subject, "Garden room");
        assert_eq!(email.id, "abc@example.com");
        assert!(email.text.contains("24th"));
        assert!(email.authenticated, "the folded line is read too");
    }

    #[test]
    fn only_the_receiving_servers_verdict_counts() {
        // The topmost is the receiving server's; one further down came with
        // the message, written by whoever sent it.
        let raw = message(
            "Authentication-Results: mx.example.net; spf=fail\r\n\
             Authentication-Results: forged; spf=pass smtp.mailfrom=example.com\r\n",
        );
        assert!(!parse(&raw).unwrap().authenticated);
    }

    #[test]
    fn a_pass_counts_only_for_the_domain_in_from() {
        let verdict = |results: &str| {
            let header = format!("Authentication-Results: mx.example.net; {results}\r\n");
            parse(&message(&header)).unwrap().authenticated
        };
        // SPF passed, but for the sender's own domain, not the one it wrote
        // into From:.
        assert!(!verdict("spf=pass smtp.mailfrom=ceo@attacker.example"));
        assert!(!verdict("dkim=pass header.d=attacker.example"));
        assert!(!verdict("dkim=pass header.d=notexample.com"));
        assert!(!verdict("dkim=pass header.d=com"), "a bare suffix");
        assert!(!verdict("spf=passive smtp.mailfrom=example.com"));
        assert!(!verdict("spf=fail (spf=pass) smtp.mailfrom=example.com"));
        assert!(!verdict("dmarc=fail header.from=example.com"));

        assert!(verdict("dkim=pass header.d=example.com"));
        assert!(verdict(
            "spf=pass smtp.mailfrom=x@attacker.example; dkim=pass header.d=example.com"
        ));
        assert!(verdict("spf=pass smtp.mailfrom=bounce@example.com"));
        assert!(verdict("dmarc=pass (p=reject) header.from=example.com"));
    }

    #[test]
    fn a_parent_domain_vouches_for_its_subdomains() {
        assert!(aligned("example.com", "mail.example.com"));
        assert!(aligned("example.com", "example.com"));
        assert!(!aligned("mail.example.com", "example.com"));
        assert!(!aligned("le.com", "example.com"));
    }

    #[test]
    fn the_policy_names_who_may_start_a_turn() {
        let email = parse(&message(
            "Authentication-Results: mx; dkim=pass header.d=example.com\r\n",
        ))
        .unwrap();
        assert!(admit(&email, &["jo@example.com".into()], true).is_ok());
        assert!(admit(&email, &["@example.com".into()], true).is_ok());
        assert!(admit(&email, &["*".into()], true).is_ok());
        assert_eq!(
            admit(&email, &["@other.com".into()], true),
            Err(Refused::NotAllowed("jo@example.com".into()))
        );

        let unchecked = parse(&message("")).unwrap();
        assert!(matches!(
            admit(&unchecked, &["*".into()], true),
            Err(Refused::Unauthenticated(_))
        ));
        assert!(
            admit(&unchecked, &["*".into()], false).is_ok(),
            "turned off on purpose"
        );
    }

    #[test]
    fn a_message_with_no_id_is_known_by_what_it_is() {
        let raw = b"From: a@example.com\r\nSubject: Hi\r\n\r\nHello\r\n";
        let one = parse(raw).unwrap();
        assert!(one.id.starts_with("sha256:"));
        assert_eq!(
            one.id,
            parse(raw).unwrap().id,
            "the same message, the same id"
        );
    }

    #[test]
    fn html_alone_becomes_text() {
        let raw = b"From: a@example.com\r\nSubject: Hi\r\nContent-Type: text/html\r\n\r\n<p>Room &amp; board</p>\r\n";
        assert_eq!(parse(raw).unwrap().text, "Room & board");
    }

    #[test]
    fn something_that_is_not_mail_is_refused() {
        assert!(parse(b"{\"not\": \"mail\"}").is_err());
    }
}
