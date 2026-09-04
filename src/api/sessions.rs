use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::Authority;
use crate::{events, jobs};

use super::chat::{AgentSession, ChatError, CreateSession, History, Usage};
use super::router::{ApiError, ApiState, authorize};
use super::worker::{CHAT_TURN, ChatTurnPayload};

impl From<ChatError> for ApiError {
    fn from(e: ChatError) -> Self {
        let status = match e {
            // A stuck session is a server-side fault, not a bad request.
            ChatError::Abandoned(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ChatError::NotFound => StatusCode::NOT_FOUND,
            ChatError::Invalid(_) => StatusCode::BAD_REQUEST,
            ChatError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

pub async fn list_sessions(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<AgentSession>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead)?;
    Ok(Json(state.chat.list_sessions(claims.tenant_id).await?))
}

/// Starting a session is how a user gets a fresh context: history is per
/// session, so a new one begins with only the agent's system prompt.
pub async fn create_session(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateSession>,
) -> Result<(StatusCode, Json<AgentSession>), ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsCreate)?;
    let session = state
        .chat
        .create_session(claims.tenant_id, claims.session_id, input)
        .await?;
    Ok((StatusCode::CREATED, Json(session)))
}

pub async fn get_messages(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<History>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead)?;
    // Ownership is checked before reading messages, which are not themselves
    // tenant-scoped.
    state.chat.get_session(claims.tenant_id, id).await?;
    Ok(Json(state.chat.messages(id).await?))
}

pub async fn delete_session(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsDelete)?;
    state.chat.delete_session(claims.tenant_id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct SendMessage {
    pub content: String,
    /// The sender's IANA timezone, e.g. "Europe/London". Sent per message
    /// rather than held on the account, so the agent answers in the zone the
    /// user is in now. Absent means the agent's clock reads UTC.
    #[serde(default)]
    pub timezone: Option<String>,
}

/// Records the user's message and queues the turn.
///
/// Returns as soon as the message is stored rather than waiting on the model:
/// the reply arrives over the event feed, so a slow provider cannot tie up the
/// request, and the same path will carry streaming and approval prompts later.
pub async fn send_message(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<SendMessage>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let claims = authorize(&state, &headers, Authority::SessionsCreate)?;

    if input.content.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "message must not be empty".into()));
    }

    let session = state.chat.get_session(claims.tenant_id, id).await?;

    let message = state
        .chat
        .append_message(id, "user", input.content.trim(), None, Usage::default())
        .await?;

    let payload = serde_json::to_value(ChatTurnPayload {
        tenant_id: claims.tenant_id,
        session_id: id,
        agent_id: session.agent_id,
        message_id: message.id,
        timezone: input.timezone,
    })
    .map_err(internal)?;
    let event = serde_json::to_value(&message).map_err(internal)?;

    // Enqueue and announce in one transaction: the browser is only told the
    // message exists once the work to answer it is durably queued.
    enqueue_turn(&state.pool, claims.tenant_id, id, payload, event)
        .await
        .map_err(internal)?;

    Ok((StatusCode::ACCEPTED, Json(message)).into_response())
}

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Queues the turn, then announces the message.
///
/// These are two statements rather than one transaction: sqlx's transaction
/// guard is not Send across awaits, which disqualifies the calling axum
/// handler. Ordering them queue-then-announce means the worst case is a queued
/// turn the browser has not been told about, which the next poll picks up --
/// rather than an announced message with no work queued to answer it.
async fn enqueue_turn(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    session_id: Uuid,
    payload: serde_json::Value,
    event: serde_json::Value,
) -> Result<(), String> {
    // Serialised on the session: a turn must see the previous reply, and two
    // running at once would each answer against a history missing the other.
    jobs::enqueue(
        pool,
        tenant_id,
        CHAT_TURN,
        payload,
        None,
        Some(&session_id.to_string()),
    )
        .await
        .map_err(|e| e.to_string())?;

    events::append(pool, tenant_id, Some(session_id), "chat.message", event)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}
