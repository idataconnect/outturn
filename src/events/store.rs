use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::PgPool;
use sqlx::{Executor, Postgres, Row};
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use super::notify::{CHANNEL, EventBus};

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub seq: i64,
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
///
/// Takes any executor so callers can append inside the transaction that caused
/// the event; the NOTIFY then fires only if that transaction commits.
pub async fn append<'e, E>(
    executor: E,
    tenant_id: Uuid,
    session_id: Option<Uuid>,
    kind: &str,
    payload: serde_json::Value,
) -> Result<i64, EventError>
where
    E: Executor<'e, Database = Postgres> + Copy,
{
    let row = sqlx::query(
        "insert into events (tenant_id, session_id, kind, payload) \
         values ($1, $2, $3, $4) returning seq",
    )
    .bind(tenant_id)
    .bind(session_id)
    .bind(kind)
    .bind(&payload)
    .fetch_one(executor)
    .await
    .map_err(internal)?;

    let seq: i64 = row.get("seq");

    // pg_notify rather than NOTIFY: the channel payload is parameterised, and
    // it is transactional either way.
    let hint = serde_json::json!({
        "tenant_id": tenant_id,
        "session_id": session_id,
    });
    sqlx::query("select pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(hint.to_string())
        .execute(executor)
        .await
        .map_err(internal)?;

    Ok(seq)
}

/// Events newer than `after` for a tenant, optionally narrowed to one session.
pub async fn since(
    pool: &PgPool,
    tenant_id: Uuid,
    session_id: Option<Uuid>,
    after: i64,
    limit: i64,
) -> Result<Vec<Event>, EventError> {
    let rows = match session_id {
        Some(sid) => {
            sqlx::query(
                "select seq, tenant_id, session_id, kind, payload from events \
                 where tenant_id = $1 and session_id = $2 and seq > $3 \
                 order by seq limit $4",
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
                "select seq, tenant_id, session_id, kind, payload from events \
                 where tenant_id = $1 and seq > $2 order by seq limit $3",
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
            seq: r.get("seq"),
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
    after: i64,
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
