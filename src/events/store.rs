use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::PgPool;
use sqlx::Row;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use super::notify::{CHANNEL, EventBus};

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: Uuid,
    pub tenant_id: Uuid,
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
    tenant_id: Uuid,
    session_id: Option<Uuid>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<Uuid, EventError> {
    let mut conn = pool.acquire().await.map_err(internal)?;
    append_on(&mut conn, tenant_id, session_id, kind, payload).await
}

/// Appends an event on a connection the caller holds.
///
/// For callers inside a transaction, so the event commits with the change
/// that caused it and the NOTIFY fires only if that transaction commits.
pub async fn append_on(
    conn: &mut sqlx::PgConnection,
    tenant_id: Uuid,
    session_id: Option<Uuid>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<Uuid, EventError> {
    let id = Uuid::now_v7();
    let row = sqlx::query(
        "insert into events (id, tenant_id, session_id, kind, payload) \
         values ($1, $2, $3, $4, $5) returning id",
    )
    .bind(id)
    .bind(tenant_id)
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
        "tenant_id": tenant_id,
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

/// Events newer than `after` for a tenant, optionally narrowed to one session.
///
/// `after` is the last event id the caller saw. `Uuid::nil()` sorts below every
/// UUIDv7, so it reads as "from the beginning".
pub async fn since(
    pool: &PgPool,
    tenant_id: Uuid,
    session_id: Option<Uuid>,
    after: Uuid,
    limit: i64,
) -> Result<Vec<Event>, EventError> {
    let rows = match session_id {
        Some(sid) => {
            sqlx::query(
                "select id, tenant_id, session_id, kind, payload from events \
                 where tenant_id = $1 and session_id = $2 and id > $3 \
                 order by id limit $4",
            )
            .bind(tenant_id)
            .bind(sid)
            .bind(after)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query(
                "select id, tenant_id, session_id, kind, payload from events \
                 where tenant_id = $1 and id > $2 order by id limit $3",
            )
            .bind(tenant_id)
            .bind(after)
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
            tenant_id: r.get("tenant_id"),
            session_id: r.get("session_id"),
            kind: r.get("kind"),
            payload: r.get("payload"),
        })
        .collect())
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
    tenant_id: Uuid,
    session_id: Option<Uuid>,
    after: Uuid,
    limit: i64,
    timeout: Duration,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<Vec<Event>, EventError> {
    let mut rx = bus.subscribe();

    let existing = since(pool, tenant_id, session_id, after, limit).await?;
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
                    if hint.tenant_id != tenant_id {
                        continue;
                    }
                    // A session-scoped poll ignores other sessions; a
                    // tenant-wide poll takes everything.
                    if let Some(want) = session_id
                        && hint.session_id != Some(want)
                    {
                        continue;
                    }
                    let events = since(pool, tenant_id, session_id, after, limit).await?;
                    if !events.is_empty() {
                        return Ok(events);
                    }
                }
                // Slow consumer: hints were dropped, so re-query rather than
                // trusting the stream.
                Err(RecvError::Lagged(_)) => {
                    let events = since(pool, tenant_id, session_id, after, limit).await?;
                    if !events.is_empty() {
                        return Ok(events);
                    }
                }
                Err(RecvError::Closed) => return Ok(Vec::new()),
            },
        }
    }
}
