//! Turns that start because something outside sent one.
//!
//! The half of [`docs/triggers.md`] a clock does not own. A delivery arrives at
//! an unguessable path, is authenticated by the scheme its trigger declared,
//! is counted against that trigger's ceiling, and becomes a turn.
//!
//! Every step of that is a refusal waiting to happen, which is the shape of
//! the module: this is the only endpoint here reachable by somebody who was
//! never given a credential by this platform.

pub mod postgres;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How much of a delivery is read before it is refused.
///
/// Bodies are read into memory to be verified -- a signature covers the whole
/// body, so there is no verifying a stream of it -- which makes an unbounded
/// body an unbounded allocation at an unauthenticated endpoint. A megabyte is
/// generous for an event payload and small enough that a thousand of them
/// concurrently is still not interesting.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// How far out a delivery's timestamp may be before it is refused as a replay.
///
/// Five minutes each way, which is enough for ordinary clock drift between two
/// machines nobody is synchronising deliberately, and short enough that a
/// captured request is not useful for long.
pub const TIMESTAMP_TOLERANCE_SECS: i64 = 300;

pub const SIGNATURE_HEADER: &str = "x-outturn-signature";
pub const TIMESTAMP_HEADER: &str = "x-outturn-timestamp";
pub const TOKEN_HEADER: &str = "x-outturn-token";

/// What the body is substituted into.
pub const BODY_PLACEHOLDER: &str = "{{body}}";

#[derive(Debug, Clone, Serialize)]
pub struct Trigger {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub name: String,
    pub path: String,
    pub scheme: String,
    /// Never serialised. The struct is returned to a browser and this is the
    /// one field on it that is a credential rather than a description of one.
    #[serde(skip)]
    pub secret: String,
    pub prompt: String,
    pub enabled: bool,
    pub account: Option<String>,
    pub owner_id: Option<Uuid>,
    pub max_per_hour: i32,
    pub last_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub refused: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TriggerInput {
    pub agent_id: Uuid,
    pub name: String,
    pub prompt: String,
    #[serde(default = "hmac_scheme")]
    pub scheme: String,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default = "default_ceiling")]
    pub max_per_hour: i32,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn hmac_scheme() -> String {
    "hmac".to_string()
}

fn default_ceiling() -> i32 {
    60
}

fn yes() -> bool {
    true
}

/// Why a delivery was not accepted.
///
/// Separate variants because the operator needs to tell them apart -- a sender
/// whose signature is wrong is misconfigured, one being refused by the ceiling
/// is working too hard -- while the caller is told much less, for the reason
/// `status` explains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NoSuchTrigger,
    Disabled,
    BadSignature,
    StaleTimestamp,
    MissingCredential,
    TooLarge,
    RateLimited,
}

impl Refusal {
    /// What the sender is told.
    ///
    /// A bad signature and an unknown path are both 404, deliberately: telling
    /// an unauthenticated caller that a path exists but their credential is
    /// wrong turns the endpoint into an oracle for which paths are real. The
    /// operator learns the difference from the trigger's own row and the log,
    /// which is where somebody who is allowed to know can look.
    pub fn status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Refusal::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            // 429 is said plainly rather than hidden as a 404. A sender being
            // throttled needs to know to slow down, and at this point it has
            // already proved it holds the credential.
            Refusal::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::NOT_FOUND,
        }
    }

    /// What the operator's log says, which is the whole truth.
    pub fn reason(&self) -> &'static str {
        match self {
            Refusal::NoSuchTrigger => "no trigger at that path",
            Refusal::Disabled => "the trigger is turned off",
            Refusal::BadSignature => "the signature did not match",
            Refusal::StaleTimestamp => "the timestamp was outside the accepted window",
            Refusal::MissingCredential => "no credential was presented",
            Refusal::TooLarge => "the body was larger than the limit",
            Refusal::RateLimited => "the trigger's hourly ceiling was reached",
        }
    }
}

/// Whether a delivery is who it says it is.
///
/// Takes the body as bytes rather than as a parsed value on purpose: a
/// signature covers what was sent, and two JSON documents that mean the same
/// thing have different bytes, so verifying a reserialisation verifies
/// something the sender never sent.
pub fn verify(
    trigger: &Trigger,
    body: &[u8],
    signature: Option<&str>,
    timestamp: Option<&str>,
    token: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), Refusal> {
    match trigger.scheme.as_str() {
        "shared_secret" => {
            let Some(token) = token else {
                return Err(Refusal::MissingCredential);
            };
            if constant_time_eq(token.as_bytes(), trigger.secret.as_bytes()) {
                Ok(())
            } else {
                Err(Refusal::BadSignature)
            }
        }
        _ => {
            let (Some(signature), Some(timestamp)) = (signature, timestamp) else {
                return Err(Refusal::MissingCredential);
            };

            // The timestamp is checked before the digest, because a stale
            // request is refused whether or not it was signed correctly -- and
            // it is *inside* the signed material, so a caller cannot move it
            // without invalidating the signature.
            let sent: i64 = timestamp.parse().map_err(|_| Refusal::StaleTimestamp)?;
            if (now.timestamp() - sent).abs() > TIMESTAMP_TOLERANCE_SECS {
                return Err(Refusal::StaleTimestamp);
            }

            let mut signed = Vec::with_capacity(timestamp.len() + 1 + body.len());
            signed.extend_from_slice(timestamp.as_bytes());
            signed.push(b'.');
            signed.extend_from_slice(body);

            let expected = format!("sha256={}", hmac_hex(trigger.secret.as_bytes(), &signed));
            if constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
                Ok(())
            } else {
                Err(Refusal::BadSignature)
            }
        }
    }
}

pub fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(message);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compares without leaking where two values first differ.
///
/// A comparison that returns early tells a caller, in timing, how much of a
/// guess was right -- which is enough to find a secret one byte at a time.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The prompt a delivery produces.
///
/// The body goes where the template says, and nowhere else. A template with no
/// placeholder is a trigger whose author wants the same prompt every time,
/// which is unusual but not wrong -- the delivery is then only a signal that
/// something happened.
pub fn prompt_for(template: &str, body: &str) -> String {
    template.replace(BODY_PLACEHOLDER, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(scheme: &str, secret: &str) -> Trigger {
        Trigger {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            agent_id: Uuid::nil(),
            name: "t".into(),
            path: "p".into(),
            scheme: scheme.into(),
            secret: secret.into(),
            prompt: "got: {{body}}".into(),
            enabled: true,
            account: None,
            owner_id: None,
            max_per_hour: 60,
            last_at: None,
            last_status: None,
            last_error: None,
            refused: 0,
            created_at: Utc::now(),
        }
    }

    fn signed_now(secret: &str, body: &[u8], now: DateTime<Utc>) -> (String, String) {
        let ts = now.timestamp().to_string();
        let mut signed = ts.as_bytes().to_vec();
        signed.push(b'.');
        signed.extend_from_slice(body);
        (
            format!("sha256={}", hmac_hex(secret.as_bytes(), &signed)),
            ts,
        )
    }

    #[test]
    fn a_correctly_signed_delivery_is_accepted() {
        let t = trigger("hmac", "shh");
        let now = Utc::now();
        let body = br#"{"event":"booking.created"}"#;
        let (sig, ts) = signed_now("shh", body, now);
        assert!(verify(&t, body, Some(&sig), Some(&ts), None, now).is_ok());
    }

    #[test]
    fn a_body_changed_after_signing_is_refused() {
        // The whole point of signing the body rather than sending a token: a
        // captured request cannot be replayed carrying a different payload.
        let t = trigger("hmac", "shh");
        let now = Utc::now();
        let (sig, ts) = signed_now("shh", br#"{"amount":10}"#, now);
        let tampered = br#"{"amount":99999}"#;
        assert_eq!(
            verify(&t, tampered, Some(&sig), Some(&ts), None, now),
            Err(Refusal::BadSignature)
        );
    }

    #[test]
    fn an_old_delivery_is_refused_however_well_signed() {
        let t = trigger("hmac", "shh");
        let signed_at = Utc::now() - chrono::Duration::hours(2);
        let body = b"{}";
        let (sig, ts) = signed_now("shh", body, signed_at);
        assert_eq!(
            verify(&t, body, Some(&sig), Some(&ts), None, Utc::now()),
            Err(Refusal::StaleTimestamp)
        );
    }

    #[test]
    fn a_delivery_with_no_credential_is_refused_rather_than_let_through() {
        let t = trigger("hmac", "shh");
        assert_eq!(
            verify(&t, b"{}", None, None, None, Utc::now()),
            Err(Refusal::MissingCredential)
        );
        let s = trigger("shared_secret", "shh");
        assert_eq!(
            verify(&s, b"{}", None, None, None, Utc::now()),
            Err(Refusal::MissingCredential)
        );
    }

    #[test]
    fn a_trigger_enforces_only_the_scheme_it_declared() {
        // A signature is not accepted by a shared-secret trigger and a token
        // is not accepted by a signing one. Falling back to whichever the
        // caller offered is how having two schemes turns into having the
        // weaker one.
        let now = Utc::now();
        let body = b"{}";
        let (sig, ts) = signed_now("shh", body, now);

        let shared = trigger("shared_secret", "shh");
        assert_eq!(
            verify(&shared, body, Some(&sig), Some(&ts), None, now),
            Err(Refusal::MissingCredential),
            "a shared-secret trigger read a signature as a credential"
        );

        let signing = trigger("hmac", "shh");
        assert_eq!(
            verify(&signing, body, None, None, Some("shh"), now),
            Err(Refusal::MissingCredential),
            "a signing trigger accepted a bare token"
        );
    }

    #[test]
    fn a_shared_secret_delivery_is_accepted_on_the_token_alone() {
        let t = trigger("shared_secret", "shh");
        assert!(verify(&t, b"{}", None, None, Some("shh"), Utc::now()).is_ok());
        assert_eq!(
            verify(&t, b"{}", None, None, Some("wrong"), Utc::now()),
            Err(Refusal::BadSignature)
        );
    }

    #[test]
    fn an_unknown_path_and_a_bad_signature_look_the_same_from_outside() {
        // Otherwise the endpoint is an oracle for which paths exist.
        assert_eq!(
            Refusal::NoSuchTrigger.status(),
            Refusal::BadSignature.status()
        );
        assert_eq!(
            Refusal::NoSuchTrigger.status(),
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn the_body_goes_where_the_template_says() {
        assert_eq!(
            prompt_for("A booking arrived: {{body}}", r#"{"id":1}"#),
            r#"A booking arrived: {"id":1}"#
        );
        // No placeholder is a trigger that wants the same prompt every time.
        assert_eq!(prompt_for("something happened", "{}"), "something happened");
    }

    #[test]
    fn the_secret_never_serialises() {
        let t = trigger("hmac", "very-secret");
        let json = serde_json::to_string(&t).expect("serialise");
        assert!(
            !json.contains("very-secret"),
            "the secret reached a browser: {json}"
        );
    }
}
