//! HTTP surface of the runtime service.
//!
//! Executes an agent as a WebAssembly component and streams its progress back
//! to the caller. The runtime holds no database: it is given a conversation and
//! returns a reply, so the tier that owns the transcript stays the only writer.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::component::{
    AgentRunner, ProgressSink, ToolActivity, ToolOutcome, ToolResultSink,
    ToolSink,
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
    /// Object storage, shared by every tenant and partitioned by prefix. The
    /// host resolves which part a turn may touch; the guest never learns.
    pub storage: Option<Arc<dyn crate::runtime::storage::StorageBackend>>,
    /// Whether this pod has room for another turn.
    pub admission: Arc<crate::runtime::admission::Admission>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ExecuteRequest {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub conversation: Vec<ConversationMessage>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    /// IANA zone of the user this turn belongs to, e.g. "Europe/London".
    /// Absent means the guest's clock answers in UTC.
    #[serde(default)]
    pub timezone: Option<String>,
    /// From the agent's policy. "none" disables thinking where supported.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
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
    /// Hosts this tenant's agents may reach. Sent with the turn because the
    /// runtime holds no database; absent means the agent reaches nothing.
    #[serde(default)]
    pub egress: Vec<crate::runtime::egress::EgressRule>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
    /// On an assistant message, the tools it asked for on that turn.
    #[serde(default)]
    pub tool_calls: Vec<ConversationToolCall>,
    /// On a tool message, which call it answers.
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
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
) -> (ProgressSink, ToolSink, ToolResultSink) {
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

    (progress, on_tool, on_tool_result)
}

