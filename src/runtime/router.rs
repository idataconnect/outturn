//! HTTP surface of the runtime service.
//!
//! Executes an agent as a WebAssembly component and streams its progress back
//! to the caller. The runtime holds no database: it is given a conversation and
//! returns a reply, so the tier that owns the transcript stays the only writer.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::component::{
    AgentRunner, CallUsage, ProgressSink, ToolActivity, ToolOutcome, ToolResultSink,
    ToolSink, UsageSink, WriteSink,
};

/// Bounds a runaway guest. Generous enough for a long conversation, finite so
/// a loop cannot occupy the service indefinitely.
pub const FUEL_PER_TURN: u64 = 50_000_000_000;

/// Model calls permitted in one turn when nothing says otherwise.
///
/// High on purpose. An agent working towards a goal legitimately reads,
/// decides and reads again many times, and a cap tuned for a single-tool agent
/// would end that work partway. This is a runaway guard, not a budget -- what
/// costs money is tokens, and that bound belongs with the accounting.
pub const DEFAULT_MAX_TOOL_ROUNDS: u32 = 100;

pub struct RuntimeState {
    pub gateway_url: String,
    pub runner: Arc<AgentRunner>,
    /// The component every agent currently runs.
    pub agent_module: Arc<Vec<u8>>,
    /// Object storage, shared by every workspace and partitioned by prefix. The
    /// host resolves which part a turn may touch; the guest never learns.
    pub storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
    /// Whether this pod has room for another turn.
    pub admission: Arc<crate::runtime::admission::Admission>,
}

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
    pub egress: Vec<crate::runtime::egress::EgressRule>,
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
    /// One model call finished, and this is what it cost. Emitted per call
    /// rather than summed at the end, so a turn that fails after three calls
    /// still bills for three, and a turn that fell back mid-way names both
    /// providers.
    Usage {
        /// Which call within the turn, from zero.
        round: u32,
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
        /// Why a hold ended this turn, where one did. Absent otherwise, and
        /// absent from an older runtime that does not send it -- which reads
        /// as "a person stopped it", the same thing it read as before.
        #[serde(default)]
        held: Option<String>,
    },
    /// The turn failed.
    Failed { message: String },
}

/// The three sinks a turn reports progress through.
///
/// Built together because they all feed one channel, and shared because both
/// ways of reaching a runtime -- being called, and asking -- need the same
/// three. The channel is unbounded: these are called while the guest is
/// blocked, so they cannot wait for a slow reader.
pub fn sinks_for(
    tx: &tokio::sync::mpsc::UnboundedSender<ExecuteEvent>,
) -> (ProgressSink, ToolSink, ToolResultSink, UsageSink, WriteSink) {
    let progress: ProgressSink = {
        let tx = tx.clone();
        let index = std::sync::atomic::AtomicI64::new(0);
        Arc::new(move |text: &str| {
            let idx = index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.send(ExecuteEvent::Delta {
                idx,
                text: text.to_string(),
            });
        })
    };

    let on_tool: ToolSink = {
        let tx = tx.clone();
        Arc::new(move |activity: &ToolActivity| {
            let _ = tx.send(ExecuteEvent::Tool {
                id: activity.id.clone(),
                name: activity.name.clone(),
                action: activity.action.clone(),
                arguments: activity.arguments.clone(),
            });
        })
    };

    let on_tool_result: ToolResultSink = {
        let tx = tx.clone();
        Arc::new(move |outcome: &ToolOutcome| {
            let _ = tx.send(ExecuteEvent::ToolResult {
                id: outcome.id.clone(),
                details: outcome.details.clone(),
                content: outcome.content.clone(),
                is_error: outcome.is_error,
            });
        })
    };

    let on_write: WriteSink = {
        let tx = tx.clone();
        Arc::new(move |path: &str, key: &str| {
            let _ = tx.send(ExecuteEvent::Wrote {
                path: path.to_string(),
                key: key.to_string(),
            });
        })
    };

    let on_usage: UsageSink = {
        let tx = tx.clone();
        Arc::new(move |call: &CallUsage| {
            let _ = tx.send(ExecuteEvent::Usage {
                round: call.round,
                endpoint: call.endpoint.clone(),
                model: call.model.clone(),
                paid_by: call.paid_by.clone(),
                prompt_tokens: call.usage.prompt_tokens,
                completion_tokens: call.usage.completion_tokens,
                cache_read_tokens: call.usage.cache_read_tokens,
                cache_write_tokens: call.usage.cache_write_tokens,
                reasoning_tokens: call.usage.reasoning_tokens,
                provider_usage: call.provider_usage.clone(),
                service_tier: call.service_tier.clone(),
            });
        })
    };

    (progress, on_tool, on_tool_result, on_usage, on_write)
}

