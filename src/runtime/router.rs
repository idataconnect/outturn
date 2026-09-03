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

use super::component::{AgentRunner, Message, ProgressSink, RunOptions, ToolActivity, ToolSink};

/// Bounds a runaway guest. Generous enough for a long conversation, finite so
/// a loop cannot occupy the service indefinitely.
const FUEL_PER_TURN: u64 = 50_000_000_000;

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
}

#[derive(Debug, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
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
    /// The guest started a tool, with the model's reason for it.
    Tool { id: String, name: String, reason: String },
    /// Generation finished; the reply is complete.
    Done { content: String },
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

    let conversation: Vec<Message> = request
        .conversation
        .into_iter()
        .map(|m| Message {
            role: m.role,
            content: m.content,
            // Stored history holds no tool calls: a turn's tool round trips
            // live and die inside it, and only the reply is kept.
            tool_calls: Vec::new(),
            tool_call_id: None,
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
                reason: activity.reason.clone(),
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
        fuel: FUEL_PER_TURN,
        timezone: request.timezone,
        reasoning_effort: request.reasoning_effort,
        traffic_type: request
            .traffic_type
            .unwrap_or_else(|| crate::gateway::routing::DEFAULT_TRAFFIC_TYPE.to_string()),
        idle_timeout: crate::http_client::IDLE_TIMEOUT,
    };

    tokio::spawn(async move {
        let outcome = runner
            .run(&module, conversation, request.system_prompt, options)
            .await;

        let _ = match outcome {
            Ok(content) => {
                tracing::info!(chars = content.len(), "agent finished");
                tx.send(ExecuteEvent::Done { content })
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
