//! A request refused for want of an approval, kept so that what a person
//! approves is what is sent.
//!
//! See `docs/approvals.md`, *The approval carries the call*. The runtime relays
//! the whole refused request, the API keeps it beside the queue item, and once
//! somebody says yes the next turn's runtime sends those bytes through the
//! gateway before the guest runs. The model is handed the answer rather than an
//! instruction to make the call again, so it never has to reproduce it.
//!
//! What is here is what the tiers say to one another about one: the request as
//! it travels, what became of it, and the one sentence a model reads about it --
//! written once, since the runtime writes it into the turn that sent the
//! request and the API writes it into every turn after.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The largest request body that is held. A call larger than this is refused
/// rather than held, as the gateway bounds what it reads back: the same figure,
/// so a body the platform would carry one way it also carries the other.
pub const BODY_LIMIT: usize = 256 * 1024;

/// How many times a held request that never left is tried before a person is
/// asked about it instead.
pub const MAX_ATTEMPTS: i32 = 3;

/// An approved request, as a turn is handed it to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldCall {
    pub id: Uuid,
    pub method: String,
    pub url: String,
    /// The headers the guest set, with any idempotency key the host added.
    /// Never a credential: the gateway attaches those, and refuses a guest's.
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// The skill operation it was, so the send is claimed against the record
    /// as the guest's own call would have been.
    pub operation: Option<String>,
    /// The tool result in this turn's conversation that the refusal is, where
    /// the API found one. The host replaces it with what came back.
    pub tool_call_id: Option<String>,
    /// Who said yes, as the model is told it.
    pub approved_by: String,
}

/// What became of a held request a runtime tried to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// The recipient answered, whatever it said. Spent.
    Answered {
        status: u16,
        #[serde(default)]
        headers: Vec<(String, String)>,
        body: String,
        #[serde(default)]
        truncated: bool,
    },
    /// The connection was never made, so nothing can have landed.
    NeverSent { message: String },
    /// It went, or may have, and no answer came back.
    NoAnswer { message: String },
    /// Not sent, because something on this side would not: the gateway
    /// refused it, or it could not be prepared. Sending it again would be
    /// refused the same way, so a person is asked.
    Refused { message: String },
}

/// What a runtime says just before it sends a held request: that it is going.
///
/// Said first, so a pod lost between sending and reporting leaves the request
/// marked as sent rather than as still to send, and the turn's retry does not
/// send it a second time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sending {
    pub job_id: Uuid,
    pub held_id: Uuid,
}

/// Why a request marked as going has no outcome: the turn sending it was lost
/// before it could say.
pub const LOST_IN_FLIGHT: &str = "the turn sending it was lost before it could say what came back";

/// What a runtime reports about one held request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub job_id: Uuid,
    pub held_id: Uuid,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// What the model reads in place of the refusal, once the held request has an
/// outcome a turn should know.
///
/// Answered, it is who approved it and what came back, in the shape a fetch's
/// own result has. Otherwise it is who approved it and why it did not go -- the
/// model must not be left believing the act happened, nor be told to try it.
pub fn result_text(method: &str, url: &str, approved_by: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Answered {
            status,
            body,
            truncated,
            ..
        } => format!(
            "Approved by {approved_by} and sent:\n{}",
            serde_json::json!({
                "method": method,
                "url": url,
                "status": status,
                "body": body,
                "truncated": truncated,
            })
        ),
        Outcome::NeverSent { message } | Outcome::Refused { message } => format!(
            "Approved by {approved_by}, but it could not be sent: {message}. Somebody has been \
             asked what to do about it; do not make this call yourself."
        ),
        Outcome::NoAnswer { message } => format!(
            "Approved by {approved_by} and sent, but no answer came back: {message}. Whether it \
             happened is not known, and somebody has been asked to check. Do not make this call \
             again."
        ),
    }
}

/// What the model reads while an approved request is still waiting for a turn
/// to send it -- one that never left, between tries.
pub fn pending_text(approved_by: &str) -> String {
    format!(
        "Approved by {approved_by}. It could not be sent yet and will be tried again; do not \
         make this call yourself."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_travels_flat_beside_what_it_is_about() {
        let report = Report {
            job_id: Uuid::nil(),
            held_id: Uuid::nil(),
            outcome: Outcome::NeverSent {
                message: "connection refused".into(),
            },
        };
        let wire = serde_json::to_value(&report).unwrap();
        assert_eq!(wire["outcome"], "never_sent");
        assert_eq!(wire["message"], "connection refused");
        let back: Report = serde_json::from_value(wire).unwrap();
        assert_eq!(back.outcome, report.outcome);
    }

    #[test]
    fn an_answer_names_who_approved_it_and_carries_what_came_back() {
        let text = result_text(
            "POST",
            "https://pay.example.com/charges",
            "Dana Reyes",
            &Outcome::Answered {
                status: 402,
                headers: vec![],
                body: "card declined".into(),
                truncated: false,
            },
        );
        assert!(
            text.starts_with("Approved by Dana Reyes and sent:"),
            "{text}"
        );
        assert!(
            text.contains("card declined") && text.contains("402"),
            "{text}"
        );
    }

    #[test]
    fn an_unknown_outcome_never_reads_as_done_or_as_an_instruction_to_retry() {
        let text = result_text(
            "POST",
            "https://pay.example.com/charges",
            "Dana Reyes",
            &Outcome::NoAnswer {
                message: "the connection reset".into(),
            },
        );
        assert!(text.contains("not known"), "{text}");
        assert!(text.contains("Do not make this call again"), "{text}");
    }
}
