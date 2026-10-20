//! A message an inbound mail service delivered to an email trigger's path:
//! taken apart, and held to the trigger's sender policy. See
//! docs/triggers.md, "Email".
//!
//! Anybody can send mail to an address, so a message is not merely untrusted
//! but unauthenticated in a way a signed webhook is not. Two checks stand
//! between it and a turn: the sender is one the trigger allows, and the
//! receiving server authenticated that sender -- SPF or DKIM passed, as its own
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
    /// Whether the receiving server passed the sender's SPF or DKIM.
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
                "{from}'s mail was not authenticated by the receiving server (no SPF or DKIM pass)"
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
    let authenticated = authenticated(raw);
    Ok(Email {
        from,
        subject,
        id,
        text,
        authenticated,
    })
}

/// Whether the topmost `Authentication-Results` header says SPF or DKIM
/// passed.
///
/// The topmost only: the receiving server prepends its own, and anything
/// further down arrived with the message -- which a sender can write
/// whatever it likes into.
fn authenticated(raw: &[u8]) -> bool {
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
    let value = value.to_ascii_lowercase();
    value.contains("spf=pass") || value.contains("dkim=pass")
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
             Authentication-Results: forged; spf=pass\r\n",
        );
        assert!(!parse(&raw).unwrap().authenticated);
    }

    #[test]
    fn the_policy_names_who_may_start_a_turn() {
        let email = parse(&message("Authentication-Results: mx; dkim=pass\r\n")).unwrap();
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
