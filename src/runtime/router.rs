//! HTTP surface of the runtime service.
//!
//! Executes an agent as a WebAssembly component and streams its progress back
//! to the caller. The runtime holds no database: it is given a conversation and
//! returns a reply, so the tier that owns the transcript stays the only writer.

use std::sync::Arc;

use axum::body::Body;
use futures::StreamExt;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{self, Authority, Role, TokenMinter, TokenValidator};

use super::component::{
    AgentRunner, Message, ProgressSink, RunOptions, ToolActivity, ToolOutcome, ToolResultSink,
    ToolSink,
};

/// Bounds a runaway guest. Generous enough for a long conversation, finite so
/// a loop cannot occupy the service indefinitely.
const FUEL_PER_TURN: u64 = 50_000_000_000;

/// Model calls permitted in one turn when nothing says otherwise.
///
/// High on purpose. An agent working towards a goal legitimately reads,
/// decides and reads again many times, and a cap tuned for a single-tool agent
/// would end that work partway. This is a runaway guard, not a budget -- what
/// costs money is tokens, and that bound belongs with the accounting.
const DEFAULT_MAX_TOOL_ROUNDS: u32 = 100;

pub struct RuntimeState {
    pub auth: TokenValidator,
    /// Mints the token the guest's model calls travel with. Minted here rather
    /// than forwarded from the caller, so a guest's reach is bounded by what
    /// the runtime grants rather than by whatever the API happened to hold.
    pub minter: TokenMinter,
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

#[derive(Debug, Deserialize)]
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
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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

type ApiError = (StatusCode, String);

pub async fn execute(
    State(state): State<Arc<RuntimeState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ExecuteRequest>,
) -> Result<Response, ApiError> {
    let token = auth::extract_bearer(&headers)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;
    let claims = state
        .auth
        .validate(token)
        .map_err(|e| (StatusCode::UNAUTHORIZED, e.to_string()))?;

    claims
        .require(Authority::GatewayInvoke)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;

    // The tenant on the token wins over the one in the body, so a caller
    // cannot run work against a tenant it holds no token for.
    if claims.tenant_id != request.tenant_id {
        return Err((
            StatusCode::FORBIDDEN,
            "tenant does not match the token".into(),
        ));
    }

    // Before any work is done for this turn, and before a response body is
    // opened: a refusal has to be a status the caller can retry, not a stream
    // that dies partway.
    let permit = match state.admission.try_admit() {
        Ok(permit) => permit,
        Err(refusal) => {
            tracing::info!(
                session_id = %request.session_id,
                in_flight = state.admission.in_flight(),
                reason = %refusal,
                "refused a turn"
            );
            // 503 rather than 429: nothing about the caller is the problem,
            // and the same request to another pod would be served. Retry-After
            // keeps a rejected caller from returning immediately, which would
            // spend the pod's remaining headroom on saying no.
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                [(axum::http::header::RETRY_AFTER, "1")],
                refusal.to_string(),
            )
                .into_response());
        }
    };

    let conversation: Vec<Message> = request
        .conversation
        .into_iter()
        .map(|m| Message {
            role: m.role,
            content: m.content,
            // A turn's tool round trips are part of the conversation, not
            // scaffolding inside it. An agent that is shown only the prose it
            // wrote afterwards cannot tell what it looked up from what it
            // decided, and will look things up again to find out.
            tool_calls: m
                .tool_calls
                .into_iter()
                .map(|c| crate::runtime::component::ToolCall {
                    id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                })
                .collect(),
            tool_call_id: m.tool_call_id,
        })
        .collect();

    let gateway_token = state
        .minter
        .mint(request.session_id, request.tenant_id, &[Role::Operator])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Deltas are forwarded down the response as the guest produces them. An
    // unbounded channel because the sink is synchronous -- it is called while
    // the guest is blocked and cannot wait for a slow reader.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ExecuteEvent>();

    let sink: ProgressSink = {
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

    let tool_sink: ToolSink = {
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

    let tool_result_sink: ToolResultSink = {
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

    tracing::info!(
        session_id = %request.session_id,
        tenant_id = %request.tenant_id,
        messages = conversation.len(),
        "executing agent"
    );

    let runner = Arc::clone(&state.runner);
    let module = Arc::clone(&state.agent_module);
    let options = RunOptions {
        session_id: request.session_id,
        gateway_url: state.gateway_url.clone(),
        gateway_token,
        default_model: request
            .model
            .unwrap_or_else(|| std::env::var("OUTTURN_DEFAULT_MODEL").unwrap_or_else(|_| "llama3.1".into())),
        progress: Some(sink),
        on_tool: Some(tool_sink),
        on_tool_result: Some(tool_result_sink),
        fuel: FUEL_PER_TURN,
        timezone: request.timezone,
        reasoning_effort: request.reasoning_effort,
        traffic_type: request
            .traffic_type
            .unwrap_or_else(|| crate::gateway::routing::DEFAULT_TRAFFIC_TYPE.to_string()),
        max_tool_rounds: match request.max_tool_rounds {
            // Negative and zero both mean unbounded, so a caller does not have
            // to know which spelling this end prefers.
            Some(n) if n <= 0 => 0,
            Some(n) => u32::try_from(n).unwrap_or(u32::MAX),
            None => DEFAULT_MAX_TOOL_ROUNDS,
        },
        reply_id: request.reply_id,
        storage: state.storage.clone(),
        // Taken from the token rather than the body, so a caller cannot ask
        // for another tenant's objects by saying it is one.
        tenant_id: claims.tenant_id,
        idle_timeout: crate::http_client::IDLE_TIMEOUT,
    };

    tokio::spawn(async move {
        // Held until the turn ends, however it ends. The slot is what this
        // pod is carrying, not what it agreed to carry.
        let _permit = permit;
        let outcome = runner
            .run(&module, conversation, request.system_prompt, options)
            .await;

        let _ = match outcome {
            Ok((content, cost)) => {
                tracing::info!(
                    chars = content.len(),
                    prompt_tokens = cost.prompt_tokens,
                    completion_tokens = cost.completion_tokens,
                    provider = cost.provider.as_deref().unwrap_or("unknown"),
                    "agent finished"
                );
                tx.send(ExecuteEvent::Done {
                    content,
                    prompt_tokens: cost.prompt_tokens,
                    completion_tokens: cost.completion_tokens,
                    cache_read_tokens: cost.cache_read_tokens,
                    cache_write_tokens: cost.cache_write_tokens,
                    reasoning_tokens: cost.reasoning_tokens,
                    provider: cost.provider,
                })
            }
            Err(e) => {
                tracing::error!(error = %e, "agent failed");
                tx.send(ExecuteEvent::Failed {
                    message: e.to_string(),
                })
            }
        };
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(|event| {
        serde_json::to_string(&event)
            .map(|mut line| {
                line.push('\n');
                axum::body::Bytes::from(line)
            })
            .map_err(std::io::Error::other)
    });

    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response())
}

pub fn routes(state: Arc<RuntimeState>) -> Router {
    Router::new()
        .route("/v1/execute", post(execute))
        .with_state(state)
}
