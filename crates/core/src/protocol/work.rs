//! What the API and a runtime say to each other about a turn: the turn handed
//! out, and the events a runtime posts back while it runs. Both tiers build
//! and read these, so they live below both.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Header a runtime quotes its lease in.
pub const LEASE_HEADER: &str = "x-outturn-lease";

#[derive(Debug, Deserialize, Serialize)]
pub struct ExecuteRequest {
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    /// Whose files the agent/ scope is. Nil for a turn nobody attributed to
    /// an agent, which should not happen and resolves to a space nothing
    /// else uses.
    #[serde(default)]
    pub agent_id: Uuid,
    /// Storage scopes this turn may write to. Absent means session only.
    #[serde(default)]
    pub write_scopes: Vec<String>,
    /// Storage scopes this turn may read. Absent means session only -- but a
    /// job queued before reads were gated carries none, and the puller reads
    /// that as the scopes it would have had.
    #[serde(default)]
    pub read_scopes: Vec<String>,
    /// The files of the skills this turn was bound to, which the agent reads
    /// as `skill/<slug>/<path>` whatever its scopes: they are part of what it
    /// was told.
    #[serde(default)]
    pub skill_files: Vec<crate::storage::scope::SkillObject>,
    pub conversation: Vec<ConversationMessage>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    /// IANA zone of the user this turn belongs to, e.g. "Europe/London".
    /// Absent means the guest's clock answers in UTC.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Resolved from the settings cascade. "none" disables thinking where
    /// supported.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Resolved from the settings cascade. Absent leaves it to the provider.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// What earlier replies in the conversation were signed with, so a model
    /// reading its own past tool calls gets its reasoning back with them.
    /// Empty from an older API, which replays replies without it.
    #[serde(default)]
    pub signed: Vec<SignedReply>,
    /// What this turn is for. The gateway resolves it to a route.
    #[serde(default)]
    pub traffic_type: Option<String>,
    /// Model calls permitted in this turn. Zero or negative disables the
    /// limit, for work that legitimately runs long.
    #[serde(default)]
    pub max_tool_rounds: Option<i64>,
    /// The reply this turn is writing, so a message absorbed mid-turn can
    /// name what took it.
    pub reply_id: Uuid,
    /// Hosts this workspace's agents may reach. Sent with the turn because the
    /// runtime holds no database; absent means the agent reaches nothing.
    #[serde(default)]
    pub egress: Vec<crate::egress::rule::EgressRule>,
    /// What the API committed to for `egress`, minted into the turn token
    /// alongside it. Carried here so whatever mints the token reads it off the
    /// same request it is signing for, rather than recomputing it from a rule
    /// list it might have obtained separately.
    ///
    /// The runtime itself never checks anything against this, and must not be
    /// given a reason to: it is the tier running workspace code, so a check it
    /// performed on data it was handed would prove nothing. The gateway reads
    /// the committed root out of the signed token instead.
    pub egress_commitment: crate::egress::commit::Hash,
    /// What this tier committed to over the turn's gates -- the requests it may
    /// not make without somebody's word. Travels beside the egress commitment
    /// and is signed into the same token; see `egress::gate`.
    pub gate_commitment: crate::egress::commit::Hash,
    /// The gates themselves, for the gateway to check against that commitment.
    ///
    /// Sent rather than proven one at a time, because a request has to be shown
    /// to match *none* of them and a Merkle proof cannot show absence.
    pub gates: crate::egress::gate::Gates,
    /// Tools to offer from the first round, by name: a template agent's,
    /// which its operator chose. Empty -- an agent made by hand, or a request
    /// from an older API -- defers every tool to the guest's loader.
    #[serde(default)]
    pub eager_tools: Vec<String>,
    /// The bound skills' operations, offered to the model as tools. Empty from
    /// an older API, which leaves operations to be called through `fetch_url`
    /// as before. See `docs/skill-operations.md`.
    #[serde(default)]
    pub skill_operations: Vec<SkillOperation>,
}

/// A request a bound skill's file declares, as a tool the guest offers.
///
/// Parsed and checked by the API at publish; the runtime hands it to the guest
/// unchanged.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SkillOperation {
    /// The tool's name, unique among the turn's operations.
    pub name: String,
    pub description: String,
    /// The operation's file, as the agent reads it: `skill/<slug>/<path>`.
    pub file: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub body: Vec<String>,
    #[serde(default)]
    pub query: Vec<String>,
    /// The arguments as JSON Schema, standing alone: what the tool offers,
    /// and what a call is checked against before anything is sent.
    #[serde(default)]
    pub schema: String,
    /// The body or query fields whose values are what this write is, from
    /// which the host derives an idempotency key. Empty for an operation that
    /// may mean to repeat. See `docs/idempotency.md`.
    #[serde(default)]
    pub identity: Vec<String>,
    /// The header the recipient takes that key in, if it does.
    #[serde(default)]
    pub key_header: Option<String>,
    /// Or the JSON body field.
    #[serde(default)]
    pub key_field: Option<String>,
}

/// What the record of declared writes holds for a key, asked before a write
/// goes out. See `docs/idempotency.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Claimed {
    /// Never seen, now claimed: send it.
    New,
    /// Sent and answered; this is the answer.
    Completed {
        response: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
    },
    /// Sent, and the answer never seen. Not sent again.
    Attempted { at: chrono::DateTime<chrono::Utc> },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationPart {
    Text { text: String },
    Call { call: ConversationToolCall },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConversationMessage {
    pub role: String,
    /// The message in the order it was produced: prose and the calls that sat
    /// between it. A user's is a single text part.
    #[serde(default)]
    pub parts: Vec<ConversationPart>,
    /// On a tool message, which call it answers.
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConversationToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

/// One line of the response stream.
///
/// Newline-delimited JSON rather than Server-Sent Events: the caller is the
/// API, not a browser, and the deltas it receives are written to the event feed
/// rather than forwarded as-is.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecuteEvent {
    /// A fragment of the reply, in order.
    Delta { idx: i64, text: String },
    /// A fragment of the model's thinking, where it produces it.
    ///
    /// Carries no index, because it is not part of the reply and nothing
    /// reassembles it into stored content -- the browser shows it while the turn
    /// runs and the transcript keeps it beside the reply rather than in it.
    Reasoning { text: String },
    /// The model began writing a call to `name`, the `index`th of its round.
    /// Only the name: the arguments are not whole until the round ends, and
    /// the call is reported as `Tool` when the guest starts it.
    Writing {
        index: u32,
        name: String,
        /// Bytes of arguments written so far. Absent from an older runtime,
        /// which only ever said a call had begun.
        #[serde(default)]
        bytes: u64,
    },
    /// The guest wrote an object. Reported so the tier with the database can
    /// treat it exactly as it treats an upload -- a document landing is a
    /// document to extract, whoever put it there. The runtime cannot enqueue
    /// that itself: it holds no database, on purpose.
    Wrote { path: String, key: String },
    /// The guest started a tool, with the model's own label for what it is
    /// doing.
    Tool {
        id: String,
        name: String,
        action: String,
        /// What the model asked with. Stored so a later turn can be shown the
        /// call rather than only the prose that followed it.
        #[serde(default)]
        arguments: String,
    },
    /// A tool finished. Carries what the reader may look at, which the model
    /// was never sent, and separately what the model was given.
    ToolResult {
        id: String,
        details: String,
        /// The result as the model received it, already truncated. Kept so a
        /// later turn replays what was actually said rather than a fuller
        /// version of it.
        #[serde(default)]
        content: String,
        is_error: bool,
    },
    /// The guest took messages the user sent mid-turn, at a round boundary.
    /// Reported where it happened, so the reply can record the point its
    /// course changed and the reader can see the message there rather than
    /// below a reply that answered it.
    Absorbed { ids: Vec<uuid::Uuid> },
    /// One model call finished, and this is what it cost. Emitted per call
    /// rather than summed at the end, so a turn that fails after three calls
    /// still bills for three, and a turn that fell back mid-way names both
    /// providers.
    Usage {
        /// Which call within the turn, from zero.
        round: u32,
        /// The class of traffic the call went out as: the turn's own for a
        /// round of the conversation, `vision` for a look at an image.
        /// Absent from an older runtime, which made only rounds.
        #[serde(default)]
        traffic_type: Option<String>,
        endpoint: String,
        model: String,
        /// Whose credential paid, as the gateway said.
        paid_by: String,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
        reasoning_tokens: u32,
        /// The provider's usage object verbatim, for re-pricing later.
        #[serde(default)]
        provider_usage: Option<serde_json::Value>,
        #[serde(default)]
        service_tier: Option<String>,
        /// The call was cut short -- a stop, a hold, a dropped upstream --
        /// so whatever was reported is for a partial answer. False from an
        /// older runtime, which could not tell.
        #[serde(default)]
        partial: bool,
        /// The counts are the runtime's arithmetic, not a provider's figure:
        /// the stream was cut before the provider said what it cost, on a
        /// protocol that says so only at the end. Marked so a bill never
        /// takes an estimate for a measurement.
        #[serde(default)]
        estimated: bool,
    },
    /// Generation finished; the reply is complete, and this is what it cost.
    Done {
        content: String,
        prompt_tokens: u32,
        completion_tokens: u32,
        #[serde(default)]
        cache_read_tokens: u32,
        #[serde(default)]
        cache_write_tokens: u32,
        #[serde(default)]
        reasoning_tokens: u32,
        #[serde(default)]
        provider: Option<String>,
        /// The model that served the turn's last round, as the gateway
        /// reported it. Absent from an older runtime, which stores nothing.
        #[serde(default)]
        model: Option<String>,
        /// Why a hold ended this turn, where one did. Absent otherwise, and
        /// absent from an older runtime that does not send it -- which reads
        /// as "a person stopped it", the same thing it read as before.
        #[serde(default)]
        held: Option<String>,
        /// Whether what stopped it is something somebody will answer.
        ///
        /// A hold that ends a turn ordinarily *latches* the session: a spend cap
        /// or an operator's stop is meant to need a person to lift it, and a
        /// session that answered the next message as though nothing happened
        /// would be a kill switch that silently did not take. An approval is the
        /// opposite -- answering it is what resumes the turn, and latching would
        /// leave the conversation stopped after the yes.
        ///
        /// False from an older runtime, which is the latching behavior it had.
        #[serde(default)]
        awaiting_approval: bool,
        /// What this turn's replies that called a tool were signed with, for
        /// the turns after it. Empty from an older runtime.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        signed: Vec<SignedReply>,
    },
    /// The turn failed.
    Failed {
        message: String,
        /// Why a hold cut it, where one did.
        ///
        /// A failure and a stop both end a turn without a reply, and they are
        /// not the same: a failure is retried, and a stop that is retried runs
        /// the work the stop existed to prevent as soon as the hold is lifted.
        #[serde(default)]
        held: Option<String>,
        /// Whether a retry would only fail the same way, spending again on
        /// every model call made before it did.
        ///
        /// False from an older runtime, which is the retrying it had.
        #[serde(default)]
        terminal: bool,
        /// Set when the model's provider declined to answer, which is not a
        /// failure of anything and is shown as what it is. Absent otherwise,
        /// and from an older runtime.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal: Option<Refusal>,
    },
}

/// A provider declining to answer, as its safety checks decided.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Refusal {
    /// The provider's own sentence about why.
    pub reason: String,
    /// What it filed the refusal under, where it said: `reasoning_extraction`,
    /// `cyber`, or a filter's name.
    #[serde(default)]
    pub category: Option<String>,
    /// The model that declined, as the gateway named it.
    #[serde(default)]
    pub model: Option<String>,
}

/// The part of a reply that only the model that wrote it can read: its signed
/// thinking blocks, and the signatures a provider put on its calls. Never shown
/// to the agent -- it means nothing to one, and an agent that could edit it
/// could only break it -- but kept with the reply, so the reasoning goes back
/// to the model when the conversation does (docs/thinking.md).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct SignedReply {
    /// The reply's first call, which is how the reply is recognized when it
    /// comes back: a reply that signed anything called a tool.
    pub first_call: String,
    /// The model that served it, as the gateway named it.
    pub model: String,
    /// The thinking blocks, exactly as the provider streamed them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking: Vec<serde_json::Value>,
    /// Signatures on calls, by call id.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub calls: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod refusal_wire {
    use super::*;

    /// What a runtime posts is what the API reads, refusal and all.
    #[test]
    fn a_refusal_survives_the_wire() {
        let sent = ExecuteEvent::Failed {
            message: "declined".into(),
            held: None,
            terminal: true,
            refusal: Some(Refusal {
                reason: "declined".into(),
                category: Some("reasoning_extraction".into()),
                model: Some("m".into()),
            }),
        };
        let line = serde_json::to_string(&sent).unwrap();
        let ExecuteEvent::Failed { refusal, .. } = serde_json::from_str(&line).unwrap() else {
            panic!("{line}")
        };
        assert_eq!(
            refusal.unwrap().category.as_deref(),
            Some("reasoning_extraction"),
            "{line}"
        );
    }
}
