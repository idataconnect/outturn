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

use super::chat::{AgentSession, ChatError, CreateSession, Delivery, History, Recent, Usage};
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

/// Where the recent list continues: an opaque cursor rather than an id, since
/// the list is ordered by last activity and an id alone does not say where
/// that is.
#[derive(Debug, Deserialize)]
pub struct RecentQuery {
    pub after: Option<String>,
    pub limit: Option<i64>,
    /// One agent's conversations only: an agent's page shows its few most
    /// recent, and asking for those is a probe of
    /// `agent_sessions_agent_recent_idx` where filtering the workspace's whole
    /// list in the browser was a read of every session it had.
    pub agent: Option<Uuid>,
}

/// A page of the recent list. The same shape as `Page`, with a string cursor.
#[derive(Debug, serde::Serialize)]
pub struct RecentPage {
    pub items: Vec<AgentSession>,
    pub next: Option<String>,
}

pub async fn list_sessions(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<RecentQuery>,
) -> Result<Json<RecentPage>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let after = match query.after.as_deref() {
        None => None,
        Some(cursor) => Some(
            Recent::decode(cursor)
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "not a cursor".to_string()))?,
        ),
    };

    // Push the reach filter into SQL so pagination returns a full page.
    let reach = super::router::reach_of(&state, &claims).await?;
    let agent_ids: Option<Vec<Uuid>> = if reach.is_narrowed() {
        Some(reach.agents().iter().copied().collect())
    } else {
        None
    };

    let mut items = state
        .chat
        .list_sessions(
            claims.workspace_id,
            agent_ids.as_deref(),
            claims.subject,
            query.agent,
            after,
            limit + 1,
        )
        .await?;

    // Read with one extra row, as `Page::from_rows` does, so `next` is set
    // only when there is another page.
    let next = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|s| Recent::of(s).encode())
    } else {
        None
    };
    Ok(Json(RecentPage { items, next }))
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
pub(super) enum Ownership {
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
pub(super) async fn session_for(
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
    // After the authority, so a caller who may not start conversations learns
    // nothing about an agent's configuration from being refused.
    let agent = state
        .agents
        .get(claims.workspace_id, input.agent_id)
        .await?;
    if !agent.takes_conversations() {
        return Err((StatusCode::CONFLICT, "this agent is disabled".into()));
    }
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

    // Serialized on the session: a turn must see the previous reply, and two
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
/// Runs a failed turn again.
///
/// The message is already stored -- what failed is the attempt at answering
/// it -- so this requeues that job rather than writing a new one. The
/// alternative, which is what the button did before this existed, was to put
/// the text back in the composer: the failed message stayed in the
/// transcript, so sending produced a second copy and the agent was asked the
/// same thing twice.
pub async fn retry_turn(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((id, message_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Sending, because that is what this is: the same prompt asked again, at
    // the same cost to the same allowance.
    let claims = authorize(&state, &headers, Authority::SessionsCreate).await?;

    // `Insufficient` rather than `Suffices`, unlike cancelling. Stopping is
    // the safe direction; this starts a turn. Having begun the thread is not
    // the question being asked -- somebody narrowed away from an agent must
    // not reach it again through a conversation they opened before.
    let _session = session_for(
        &state,
        &claims,
        id,
        Authority::SessionsCreate,
        Ownership::Insufficient,
    )
    .await?;

    let Some(job_id) = jobs::turn_for_message(&state.pool, claims.workspace_id, message_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    else {
        return Err((StatusCode::NOT_FOUND, "no turn for that message".into()));
    };

    let outcome = jobs::requeue_failed(&state.pool, job_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (queued, described) = match outcome {
        jobs::Requeued::Queued => (true, "queued"),
        // Not an error: two people pressing the button, or one pressing it
        // twice, is a race nobody can avoid and should not be scolded for.
        jobs::Requeued::NotFailed => (false, "not_failed"),
        jobs::Requeued::Unknown => (false, "no_turn"),
    };

    // Told to everyone watching, like a cancel is. A second reader with the
    // conversation open should see it start again rather than go on showing a
    // failure that is no longer true.
    if queued {
        events::append(
            &state.pool,
            claims.workspace_id,
            Some(id),
            "chat.requeued",
            serde_json::json!({ "job_id": job_id, "message_id": message_id }),
        )
        .await
        .ok();
    }

    Ok(Json(
        serde_json::json!({ "queued": queued, "state": described }),
    ))
}

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

/// One agent's activity, for the dashboard's "right now" panel.
#[derive(Debug, serde::Serialize)]
pub struct AgentActivity {
    pub agent_id: Uuid,
    pub running: i64,
    pub queued: i64,
    pub waiting: i64,
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The most recently active session with a live turn, to open from the
    /// panel. None when nothing is in flight.
    pub live_session_id: Option<Uuid>,
}

/// Which of the workspace's agents are working, waiting or idle.
///
/// Sized by agents and turns in flight, never by sessions: the live turns come
/// from `jobs_live_turn_workspace_idx`, which holds only what is in flight, and
/// each agent's last activity is one probe of `agent_sessions_agent_recent_idx`.
/// The panel asks every few seconds for as long as it is open, so a query that
/// grew with history would be a cost that grew with it.
///
/// Narrowed readers see the agents they were scoped to and nothing else. The
/// session list also shows them their own conversations with other agents, but
/// counting those here would need an index of its own for a corner of a corner.
pub async fn agent_activity(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<AgentActivity>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::SessionsRead).await?;
    let reach = super::router::reach_of(&state, &claims).await?;
    let agent_ids: Option<Vec<Uuid>> = reach
        .is_narrowed()
        .then(|| reach.agents().iter().copied().collect());

    // The state list and the kind are `jobs_live_turn_workspace_idx`'s
    // predicate word for word, which is the only way the planner will use it;
    // a state added there has to be added here, and in `live_turn` (0025).
    let rows = sqlx::query(
        "with live as ( \
             select s.agent_id, s.id as session_id, s.last_active_at, j.state \
               from jobs j \
               join agent_sessions s on s.id = (j.payload->>'session_id')::uuid \
              where j.workspace_id = $1 \
                and j.kind = 'chat.turn' and j.state in ('pending', 'running', 'parked') \
         ) \
         select a.id as agent_id, \
                (select count(*) from live where live.agent_id = a.id and state = 'running') as running, \
                (select count(*) from live where live.agent_id = a.id and state = 'pending') as queued, \
                (select count(*) from live where live.agent_id = a.id and state = 'parked') as waiting, \
                (select s.last_active_at from agent_sessions s where s.agent_id = a.id \
                  order by s.last_active_at desc limit 1) as last_active_at, \
                (select session_id from live where live.agent_id = a.id \
                  order by last_active_at desc limit 1) as live_session_id \
           from agents a \
          where a.workspace_id = $1 and a.enabled \
            and ($2::uuid[] is null or a.id = any($2))",
    )
    .bind(claims.workspace_id)
    .bind(agent_ids)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;

    use sqlx::Row;
    Ok(Json(
        rows.iter()
            .map(|r| AgentActivity {
                agent_id: r.get("agent_id"),
                running: r.get("running"),
                queued: r.get("queued"),
                waiting: r.get("waiting"),
                last_active_at: r.get("last_active_at"),
                live_session_id: r.get("live_session_id"),
            })
            .collect(),
    ))
}
