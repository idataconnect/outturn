use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::Authority;
use crate::{events, jobs};

use super::chat::{AgentSession, ChatError, CreateSession, Delivery, History, Usage};
use super::router::{ApiError, ApiState, authenticate, authorize};
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
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    let all = state.chat.list_sessions(claims.workspace_id).await?;

    // Filtered rather than refused: a listing that failed because one session
    // is out of reach would tell the caller nothing and hide what is theirs.
    let reach = super::router::reach_of(&state, &claims).await?;
    Ok(Json(
        all.into_iter()
            .filter(|s| reach.covers(s.agent_id) || s.user_id == Some(claims.subject))
            .collect(),
    ))
}

/// Whether having started a conversation is enough on its own.
///
/// `agent_sessions.user_id` records who started it, and for most of what can
/// be done to a conversation that settles it: your own history is yours to
/// read, rename and delete whoever else is shut out, and stopping a turn is
/// never the dangerous direction.
///
/// Sending is the exception, and it is not a detail. A narrowing says which
/// agents a person may use, and a person who keeps an old thread open keeps
/// using one -- its tools, its skills, the workspace's allowance -- for as
/// long as they like. Refusing new sessions while leaving old ones live
/// narrows the roster and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ownership {
    /// Started it, so it is theirs.
    Suffices,
    /// Started it, and that is not the question being asked.
    Insufficient,
}

/// Loads a conversation and proves the caller may act on it, together.
///
/// One function rather than a lookup and a check, because the check was opt-in
/// before and four handlers did not opt in: narrowing held on reads and not on
/// rename, delete, send or cancel. A handler that cannot get the session
/// without naming the authority it is acting under cannot forget the authority,
/// and the next write endpoint inherits the guard rather than having to
/// remember it.
///
/// A write is narrowed wherever its read is. Holding `sessions:delete`
/// workspace-wide while narrowed to the support agent has to mean the
/// accounting agent's conversations cannot be deleted either -- otherwise the
/// narrowing hides conversations it leaves fully writable to whoever has the
/// id, and ids travel in URLs.
async fn session_for(
    state: &ApiState,
    claims: &crate::auth::SessionClaims,
    id: Uuid,
    authority: Authority,
    ownership: Ownership,
) -> Result<AgentSession, ApiError> {
    let session = state.chat.get_session(claims.workspace_id, id).await?;
    if ownership == Ownership::Suffices && session.user_id == Some(claims.subject) {
        return Ok(session);
    }
    super::router::require_for_agent(state, claims, authority, session.agent_id).await?;
    Ok(session)
}

/// Starting a session is how a user gets a fresh context: history is per
/// session, so a new one begins with only the agent's system prompt.
pub async fn create_session(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateSession>,
) -> Result<(StatusCode, Json<AgentSession>), ApiError> {
    let claims = authenticate(&state, &headers)?;
    // Which agent decides it: talking to the accounting agent and talking to
    // the support agent are separate permissions where somebody said so.
    super::router::require_for_agent(&state, &claims, Authority::SessionsCreate, input.agent_id)
        .await?;
    let session = state
        .chat
        .create_session(claims.workspace_id, claims.subject, input)
        .await?;
    Ok((StatusCode::CREATED, Json(session)))
}

/// How far back a single read will go.
///
/// A reader is served the newest `DEFAULT_MESSAGES` and asks for more by
/// cursor. The ceiling is not politeness: the delta aggregation behind a page
/// is per-message work, so an unbounded `limit` would hand a caller the
/// unbounded query this endpoint exists to stop serving.
const DEFAULT_MESSAGES: i64 = 50;
const MAX_MESSAGES: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    /// Keyset cursor: the page holds the messages immediately older than this.
    /// Absent means the newest page, which is where a conversation opens.
    pub before: Option<Uuid>,
    pub limit: Option<i64>,
}

pub async fn get_messages(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<History>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    // Ownership is checked before reading messages, which are not themselves
    // workspace-scoped. It also has to happen before the cursor is used: a
    // cursor names a message, and reading one from another workspace's session
    // must fail on the session rather than on the row it points at.
    let _session = session_for(
        &state,
        &claims,
        id,
        Authority::SessionsRead,
        Ownership::Suffices,
    )
    .await?;

    let limit = query
        .limit
        .unwrap_or(DEFAULT_MESSAGES)
        .clamp(1, MAX_MESSAGES);

    Ok(Json(
        state.chat.messages_page(id, query.before, limit).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct RenameSession {
    pub title: String,
}

/// Names a session, or takes the name away.
///
/// A name a person gives stands: the namer only ever writes to a session
/// that has none. Clearing it is allowed and means "unnamed", which puts the
/// session back where it started, and on the namer's list for the next turn.
pub async fn rename_session(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<RenameSession>,
) -> Result<Json<AgentSession>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsUpdate).await?;
    let title = input.title.trim();
    if title.chars().count() > super::naming::MAX_TITLE_CHARS {
        return Err((StatusCode::BAD_REQUEST, "title is too long".into()));
    }
    // Read before written, for the agent it belongs to: the rename itself is
    // scoped to the workspace and would otherwise land on any session in it.
    let _existing = session_for(
        &state,
        &claims,
        id,
        Authority::SessionsUpdate,
        Ownership::Suffices,
    )
    .await?;

    let session = state
        .chat
        .rename_session(claims.workspace_id, id, title)
        .await?;
    super::naming::announce(&state.pool, &session).await;
    Ok(Json(session))
}

pub async fn delete_session(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsDelete).await?;
    let _session = session_for(
        &state,
        &claims,
        id,
        Authority::SessionsDelete,
        Ownership::Suffices,
    )
    .await?;

    state.chat.delete_session(claims.workspace_id, id).await?;
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
    /// How this should reach a turn that is already running. Defaults to
    /// steering, which is what someone typing mid-turn usually means.
    #[serde(default)]
    pub delivery: Delivery,
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
    let claims = authorize(&state, &headers, Authority::SessionsCreate).await?;

    if input.content.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "message must not be empty".into()));
    }

    // `Insufficient`: having started this thread is not permission to keep
    // talking to an agent somebody has since narrowed away.
    let session = session_for(
        &state,
        &claims,
        id,
        Authority::SessionsCreate,
        Ownership::Insufficient,
    )
    .await?;

    let message = state
        .chat
        .append_message(
            id,
            "user",
            input.content.trim(),
            None,
            Usage::default(),
            input.delivery,
            // The session id on a token is the account id, which is what the
            // login path mints it from.
            Some(claims.subject),
        )
        .await?;

    let payload = serde_json::to_value(ChatTurnPayload {
        workspace_id: claims.workspace_id,
        session_id: id,
        agent_id: session.agent_id,
        message_id: message.id,
        timezone: input.timezone,
        user_id: Some(claims.subject),
    })
    .map_err(internal)?;
    let event = serde_json::to_value(&message).map_err(internal)?;

    // Enqueue and announce in one transaction: the browser is only told the
    // message exists once the work to answer it is durably queued.
    enqueue_turn(&state.pool, claims.workspace_id, id, payload, event)
        .await
        .map_err(internal)?;

    Ok((StatusCode::ACCEPTED, Json(message)).into_response())
}

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Records the message, queues the turn and announces it, together.
///
/// One transaction, so the browser is only told the message exists once the
/// work to answer it is durably queued -- and a message never exists without
/// its job. The earlier version ran these as separate statements, which left
/// a window where a stored user message had no job to answer it and nothing
/// that would ever notice.
async fn enqueue_turn(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    session_id: Uuid,
    payload: serde_json::Value,
    event: serde_json::Value,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    // Somebody is in this conversation, so it counts towards how many pods the
    // fleet wants. Written here rather than derived from the transcript later:
    // the count is needed every few seconds by an autoscaler, and asking the
    // busiest table in the system for it gets more expensive exactly as the
    // cluster gets busier.
    sqlx::query(
        "insert into live_sessions (session_id, expires_at) \
         values ($1, now() + interval '5 minutes') \
         on conflict (session_id) do update set expires_at = excluded.expires_at",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // Serialised on the session: a turn must see the previous reply, and two
    // running at once would each answer against a history missing the other.
    jobs::enqueue(
        &mut *tx,
        workspace_id,
        CHAT_TURN,
        payload,
        None,
        Some(&session_id.to_string()),
        // Somebody is watching this one: it came from a message a person just
        // sent, and a backlog of scheduled work must not put itself in front
        // of them.
        jobs::PRIORITY_REALTIME,
    )
    .await
    .map_err(|e| e.to_string())?;

    events::append_on(
        &mut tx,
        workspace_id,
        Some(session_id),
        "chat.message",
        event,
    )
    .await
    .map_err(|e| e.to_string())?;

    tx.commit().await.map_err(|e| e.to_string())
}

/// Asks the turn a session has in flight to stop.
///
/// Answers what it did rather than only that it succeeded, because the three
/// cases feel different to whoever pressed the button: a turn that had not
/// started is over immediately, one already running takes until its next round
/// boundary, and one that had finished on its own was never stopped at all.
///
/// Idempotent. Pressing stop twice is what a person does when the first press
/// appears not to have worked, and the second must not be an error.
pub async fn cancel_turn(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // The same authority as sending: whoever may start a turn in this session
    // may stop one. A separate permission would mean a person who can spend
    // the allowance cannot stop spending it.
    let claims = authorize(&state, &headers, Authority::SessionsCreate).await?;

    // Proves the session belongs to this workspace before anything is looked
    // up by it, so a job id cannot be reached through somebody else's session,
    // and narrowed the same way sending is: stopping somebody else's turn is
    // acting on their conversation.
    //
    // `Suffices` unlike sending, though. Stopping is the safe direction: a
    // person narrowed away from an agent can no longer start turns on an old
    // thread, and refusing to let them stop one already running would leave
    // them watching something they cannot reach spend the allowance.
    let _session = session_for(
        &state,
        &claims,
        id,
        Authority::SessionsCreate,
        Ownership::Suffices,
    )
    .await?;

    let Some(job_id) = jobs::live_turn_for_session(&state.pool, claims.workspace_id, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    else {
        // Nothing in flight. Not an error: the turn finished while the button
        // was being pressed, which is a race a person cannot avoid and should
        // not be scolded for.
        return Ok(Json(
            serde_json::json!({ "stopped": false, "state": "nothing_running" }),
        ));
    };

    let outcome = jobs::request_cancel(&state.pool, job_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (stopped, described) = match outcome {
        jobs::Cancelled::BeforeItRan => (true, "cancelled"),
        jobs::Cancelled::WhileRunning => (true, "stopping"),
        jobs::Cancelled::AlreadyOver => (false, "nothing_running"),
    };

    // Told to everyone watching, not just whoever asked. A second reader with
    // the conversation open should see it stop too, and the button is not the
    // only thing that has to agree about what happened.
    if stopped {
        events::append(
            &state.pool,
            claims.workspace_id,
            Some(id),
            "chat.cancelling",
            serde_json::json!({ "job_id": job_id, "state": described }),
        )
        .await
        .ok();
    }

    Ok(Json(
        serde_json::json!({ "stopped": stopped, "state": described }),
    ))
}
