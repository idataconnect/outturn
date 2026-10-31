//! HTTP surface of the runtime service.
//!
//! Executes an agent as a WebAssembly component and streams its progress back
//! to the caller. The runtime holds no database: it is given a conversation and
//! returns a reply, so the tier that owns the transcript stays the only writer.

use std::sync::Arc;

pub use outturn_core::protocol::work::{
    ConversationMessage, ConversationPart, ConversationToolCall, ExecuteEvent, ExecuteRequest,
};

use super::component::{
    AbsorbedSink, AgentRunner, CallUsage, ConnectionSink, ProgressSink, ReasoningSink,
    ToolActivity, ToolOutcome, ToolResultSink, ToolSink, UsageSink, WriteSink, WritingSink,
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
    pub storage: Option<Arc<dyn outturn_core::storage::StorageBackend>>,
    /// Whether this pod has room for another turn.
    pub admission: Arc<crate::admission::Admission>,
}

/// The sinks a turn reports through.
///
/// Built together because they all feed one channel, and shared because both
/// ways of reaching a runtime -- being called, and asking -- need the same set.
/// The channel is unbounded: these are called while the guest is blocked, so
/// they cannot wait for a slow reader.
///
/// Named rather than a tuple. It was six positions read back as `sinks.3` and
/// `sinks.4` by a caller that could not say which was which, with a doc comment
/// still calling it three -- so inserting one silently handed every sink after
/// it to the wrong field, and the types were close enough that most of it
/// compiled.
pub struct Sinks {
    pub progress: ProgressSink,
    pub reasoning: ReasoningSink,
    pub writing: WritingSink,
    pub on_tool: ToolSink,
    pub on_tool_result: ToolResultSink,
    pub on_usage: UsageSink,
    pub on_write: WriteSink,
    pub on_absorbed: AbsorbedSink,
    pub on_connection: ConnectionSink,
}

pub fn sinks_for(tx: &tokio::sync::mpsc::UnboundedSender<ExecuteEvent>) -> Sinks {
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

    // No index and no round separator: thinking is not the reply, so nothing
    // concatenates it to stored content and nothing has to line up with what a
    // later round shows.
    let reasoning: ReasoningSink = {
        let tx = tx.clone();
        Arc::new(move |text: &str| {
            let _ = tx.send(ExecuteEvent::Reasoning {
                text: text.to_string(),
            });
        })
    };

    let writing: WritingSink = {
        let tx = tx.clone();
        Arc::new(move |index: u32, name: &str, bytes: u64| {
            let _ = tx.send(ExecuteEvent::Writing {
                index,
                name: name.to_string(),
                bytes,
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
        Arc::new(
            move |path: &str, key: &str, what: crate::component::Stored| {
                let _ = tx.send(match what {
                    crate::component::Stored::Wrote(bytes) => ExecuteEvent::Wrote {
                        path: path.to_string(),
                        key: key.to_string(),
                        bytes: Some(bytes),
                    },
                    crate::component::Stored::Deleted => ExecuteEvent::Deleted {
                        key: key.to_string(),
                    },
                });
            },
        )
    };

    let on_usage: UsageSink = {
        let tx = tx.clone();
        Arc::new(move |call: &CallUsage| {
            let _ = tx.send(ExecuteEvent::Usage {
                round: call.round,
                traffic_type: Some(call.traffic_type.clone()),
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
                partial: call.partial,
                estimated: call.estimated,
            });
        })
    };

    let on_absorbed: AbsorbedSink = {
        let tx = tx.clone();
        Arc::new(move |ids: &[uuid::Uuid]| {
            let _ = tx.send(ExecuteEvent::Absorbed { ids: ids.to_vec() });
        })
    };

    let on_connection: ConnectionSink = {
        let tx = tx.clone();
        Arc::new(move |host: &str, asker: uuid::Uuid| {
            let _ = tx.send(ExecuteEvent::Connection {
                host: host.to_string(),
                asker,
            });
        })
    };

    Sinks {
        on_connection,
        progress,
        reasoning,
        writing,
        on_tool,
        on_tool_result,
        on_usage,
        on_write,
        on_absorbed,
    }
}
