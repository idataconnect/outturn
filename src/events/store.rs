use std::time::Duration;

use serde::Serialize;
use sqlx::Row;
use sqlx::postgres::PgPool;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use super::notify::{CHANNEL, EventBus};

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub session_id: Option<Uuid>,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum EventError {
    #[error("event store error: {0}")]
    Internal(String),
}

fn internal(e: sqlx::Error) -> EventError {
    EventError::Internal(e.to_string())
}

/// Appends an event and announces it.
pub async fn append(
    pool: &PgPool,
    workspace_id: Uuid,
    session_id: Option<Uuid>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<Uuid, EventError> {
    let mut conn = pool.acquire().await.map_err(internal)?;
    append_on(&mut conn, workspace_id, session_id, kind, payload).await
}

/// Appends an event on a connection the caller holds.
///
/// For callers inside a transaction, so the event commits with the change
/// that caused it and the NOTIFY fires only if that transaction commits.
pub async fn append_on(
    conn: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    session_id: Option<Uuid>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<Uuid, EventError> {
    let id = Uuid::now_v7();
    let row = sqlx::query(
        "insert into events (id, workspace_id, session_id, kind, payload) \
         values ($1, $2, $3, $4, $5) returning id",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(session_id)
    .bind(kind)
    .bind(&payload)
    .fetch_one(&mut *conn)
    .await
    .map_err(internal)?;

    let id: Uuid = row.get("id");

    // pg_notify rather than NOTIFY: the channel payload is parameterised, and
    // it is transactional either way.
    let hint = serde_json::json!({
        "workspace_id": workspace_id,
        "session_id": session_id,
    });
    sqlx::query("select pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(hint.to_string())
        .execute(&mut *conn)
        .await
        .map_err(internal)?;

    Ok(id)
}

/// Which conversations a narrowed caller may be shown.
///
/// Absent means "everything in the workspace", which is what somebody who has
/// not been narrowed gets. Present, it is the same rule `list_sessions` filters
/// on: the agents the caller was scoped to, plus their own conversations
/// whichever agent those are with.
///
/// Applied in the query rather than to the result, because filtering afterwards
/// would let a page of events that are all invisible come back empty and read
/// as "nothing yet" -- a long poll that returns instantly and forever.
#[derive(Debug, Clone)]
pub struct Visible {
    agents: Vec<Uuid>,
    subject: Uuid,
}

impl Visible {
    /// What this caller may be shown, or `None` when that is everything.
    ///
    /// The only way to build one, because the two halves of the rule have to
    /// travel together. `Reach` holds its agents privately and reads an empty
    /// set as "nobody narrowed anything" -- the opposite of what an empty
    /// `any($1)` means in the query below, which is "matches nothing". Built by
    /// hand from `reach.agents()`, an unnarrowed caller becomes a filter that
    /// hides every session, the long poll returns empty forever, and the reader
    /// watches a conversation that has stopped arriving.
    pub fn of(reach: &crate::api::scope::Reach, subject: Uuid) -> Option<Self> {
        reach.is_narrowed().then(|| Self {
            agents: reach.agents().iter().copied().collect(),
            subject,
        })
    }
}

/// Events newer than `after` for a workspace, optionally narrowed to one session.
///
/// `after` is the last event id the caller saw. `Uuid::nil()` sorts below every
/// UUIDv7, so it reads as "from the beginning".
pub async fn since(
    pool: &PgPool,
    workspace_id: Uuid,
    session_id: Option<Uuid>,
    after: Uuid,
    limit: i64,
    visible: Option<&Visible>,
) -> Result<Vec<Event>, EventError> {
    // Two statements, split on the session filter, because `events_session_id_idx`
    // is partial -- `on (session_id, id) where session_id is not null` -- and
    // `($n is null or e.session_id = $n)` is not sargable against it. Written as
    // one statement, a session-scoped poll stopped using that index and filtered
    // every workspace event since the cursor instead, on the hot path that
    // re-runs per streamed delta per connected reader.
    //
    // The scope filter stays a null-guard inside each: it is not what an index
    // is chosen on, and spelling out four statements is how two of them drift.
    //
    // A session-less event is workspace-level by construction -- nothing about
    // one conversation -- so narrowing does not hide it.
    // The scope clause is written out in both rather than shared: `sqlx::query`
    // takes a literal by design here, so a shared `const` composed with
    // `format!` is a dynamic SQL string and refused. The duplication is real
    // and the two must be changed together -- which is what the test named
    // after this behaviour is for, since a comment cannot enforce it.
    let rows = match session_id {
        Some(sid) => {
            sqlx::query(
                "select e.id, e.workspace_id, e.session_id, e.kind, e.payload from events e \
                 where e.workspace_id = $1 and e.session_id = $5 and e.id > $2 \
                   and ($3::uuid[] is null or e.session_id is null or exists ( \
                         select 1 from agent_sessions s \
                         where s.id = e.session_id \
                           and (s.agent_id = any($3) or s.user_id = $4))) \
                 order by e.id limit $6",
            )
            .bind(workspace_id)
            .bind(after)
            .bind(visible.map(|v| v.agents.clone()))
            .bind(visible.map(|v| v.subject))
            .bind(sid)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query(
                "select e.id, e.workspace_id, e.session_id, e.kind, e.payload from events e \
                 where e.workspace_id = $1 and e.id > $2 \
                   and ($3::uuid[] is null or e.session_id is null or exists ( \
                         select 1 from agent_sessions s \
                         where s.id = e.session_id \
                           and (s.agent_id = any($3) or s.user_id = $4))) \
                 order by e.id limit $5",
            )
            .bind(workspace_id)
            .bind(after)
            .bind(visible.map(|v| v.agents.clone()))
            .bind(visible.map(|v| v.subject))
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
    .map_err(internal)?;

    Ok(rows
        .iter()
        .map(|r| Event {
            id: r.get("id"),
            workspace_id: r.get("workspace_id"),
            session_id: r.get("session_id"),
            kind: r.get("kind"),
            payload: r.get("payload"),
        })
        .collect())
}

/// The highest event id a poll would have looked at, visible or not.
///
/// A narrowed caller's cursor is taken from the events they were handed, so a
/// window holding nothing they may see leaves it where it was -- and the next
/// poll rescans the same span, and the one after that a longer one. A busy
/// agent they cannot see turns an idle reader into a scan of the day's events,
/// repeated on every notification. The watermark is what lets the cursor move
/// over events that were filtered out, which is the only way it can move when
/// filtering is the ordinary case.
///
/// Bounded by the same `limit` as the read: what it reports is the end of the
/// window that was examined, never the end of the table, so nothing that
/// arrives later is skipped.
pub async fn watermark(
    pool: &PgPool,
    workspace_id: Uuid,
    session_id: Option<Uuid>,
    after: Uuid,
    limit: i64,
) -> Result<Option<Uuid>, EventError> {
    let high: Option<Uuid> = match session_id {
        // `order by ... desc limit 1` rather than `max(id)`: Postgres has no
        // max() over uuid. The inner window is what bounds it, so this reads
        // the last row of the span that would have been examined.
        Some(sid) => sqlx::query_scalar(
            "select w.id from (select e.id from events e \
             where e.workspace_id = $1 and e.session_id = $3 and e.id > $2 \
             order by e.id limit $4) w order by w.id desc limit 1",
        )
        .bind(workspace_id)
        .bind(after)
        .bind(sid)
        .bind(limit)
        .fetch_optional(pool)
        .await
        .map_err(internal)?
        .flatten(),
        None => sqlx::query_scalar(
            "select w.id from (select e.id from events e \
             where e.workspace_id = $1 and e.id > $2 order by e.id limit $3) w \
             order by w.id desc limit 1",
        )
        .bind(workspace_id)
        .bind(after)
        .bind(limit)
        .fetch_optional(pool)
        .await
        .map_err(internal)?
        .flatten(),
    };
    Ok(high)
}

/// Long poll: returns immediately when there is anything newer than `after`,
/// otherwise parks until a matching event arrives, `timeout` elapses, or
/// `shutdown` fires. An empty result means "nothing yet, ask again".
///
/// Subscribing happens before the first query, so an event landing between the
/// two is caught by the wait rather than missed.
pub async fn wait_for(
    pool: &PgPool,
    bus: &EventBus,
    workspace_id: Uuid,
    session_id: Option<Uuid>,
    after: Uuid,
    limit: i64,
    timeout: Duration,
    shutdown: impl std::future::Future<Output = ()>,
    visible: Option<&Visible>,
) -> Result<Vec<Event>, EventError> {
    let mut rx = bus.subscribe();

    let existing = since(pool, workspace_id, session_id, after, limit, visible).await?;
    if !existing.is_empty() {
        return Ok(existing);
    }

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut deadline => return Ok(Vec::new()),
            _ = &mut shutdown => return Ok(Vec::new()),
            received = rx.recv() => match received {
                Ok(hint) => {
                    if hint.workspace_id != workspace_id {
                        continue;
                    }
                    // A session-scoped poll ignores other sessions; a
                    // workspace-wide poll takes everything.
                    if let Some(want) = session_id
                        && hint.session_id != Some(want)
                    {
                        continue;
                    }
                    let events = since(pool, workspace_id, session_id, after, limit, visible).await?;
                    if !events.is_empty() {
                        return Ok(events);
                    }
                }
                // Slow consumer: hints were dropped, so re-query rather than
                // trusting the stream.
                Err(RecvError::Lagged(_)) => {
                    let events = since(pool, workspace_id, session_id, after, limit, visible).await?;
                    if !events.is_empty() {
                        return Ok(events);
                    }
                }
                Err(RecvError::Closed) => return Ok(Vec::new()),
            },
        }
    }
}
