//! Removing files nobody will read again: a session's scratch files once the
//! session has been idle for the window, and everything under a session,
//! agent or workspace that was deleted (migration 0071).
//!
//! Here rather than in a bucket lifecycle rule, which can only count an
//! object's own age: a long conversation lost its uploads mid-conversation
//! because they were older than the window, however recently it was used.

use std::sync::Arc;

use outturn_core::storage::StorageBackend;
use outturn_core::storage::scope::{self, Scope, Space};
use sqlx::PgPool;
use uuid::Uuid;

/// Days a session may sit idle before its files are swept, unless
/// `OUTTURN_SESSION_FILE_TTL_DAYS` says otherwise.
pub const DEFAULT_TTL_DAYS: u32 = 30;

/// Prefixes, and idle sessions, handled per sweep. A sweep runs hourly, so a
/// backlog drains over hours rather than holding one tick for minutes.
const BATCH: i64 = 100;

pub fn ttl_days() -> u32 {
    std::env::var("OUTTURN_SESSION_FILE_TTL_DAYS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(DEFAULT_TTL_DAYS)
}

/// One pass: deleted things first, then idle sessions. Errors are logged and
/// the rest carries on, so one unreadable prefix does not stop the others; a
/// prefix that failed stays recorded and is tried next time.
pub async fn sweep(pool: &PgPool, storage: &Arc<dyn StorageBackend>, ttl_days: u32) {
    match deleted(pool, storage).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(prefixes = n, "removed the files of deleted things"),
        Err(e) => tracing::warn!(error = %e, "could not sweep deleted things' files"),
    }
    match idle(pool, storage, ttl_days).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(sessions = n, ttl_days, "swept idle sessions' files"),
        Err(e) => tracing::warn!(error = %e, "could not sweep idle sessions' files"),
    }
}

async fn deleted(pool: &PgPool, storage: &Arc<dyn StorageBackend>) -> anyhow::Result<u64> {
    let prefixes: Vec<(String,)> =
        sqlx::query_as("select prefix from storage_deletions order by requested_at limit $1")
            .bind(BATCH)
            .fetch_all(pool)
            .await?;
    let mut done = 0;
    for (prefix,) in prefixes {
        if let Err(e) = remove_under(storage, &prefix).await {
            tracing::warn!(prefix, error = %e, "could not remove a deleted thing's files");
            continue;
        }
        sqlx::query("delete from storage_deletions where prefix = $1")
            .bind(&prefix)
            .execute(pool)
            .await?;
        done += 1;
    }
    Ok(done)
}

/// Sessions idle for the window whose files have not been swept since they
/// were last active. A session with work still to come -- a turn queued or
/// parked, a wakeup it asked for -- is not idle, however long ago anybody
/// spoke in it: `sleep` may wait exactly the window, and waking to find its
/// files gone is the bug this replaced.
async fn idle(
    pool: &PgPool,
    storage: &Arc<dyn StorageBackend>,
    ttl_days: u32,
) -> anyhow::Result<u64> {
    let sessions: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select s.id, s.workspace_id, s.agent_id from agent_sessions s \
         where s.last_active_at < now() - make_interval(days => $1) \
           and (s.files_swept_at is null or s.files_swept_at < s.last_active_at) \
           and not exists ( \
               select 1 from jobs j \
               where j.state in ('pending', 'running', 'parked') \
                 and j.payload->>'session_id' = s.id::text) \
         order by s.last_active_at \
         limit $2",
    )
    .bind(i32::try_from(ttl_days).unwrap_or(i32::MAX))
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for (session_id, workspace_id, agent_id) in sessions {
        let prefix = scope::root_for(
            &Space {
                workspace_id,
                agent_id,
                session_id,
            },
            Scope::Session,
        );
        if let Err(e) = remove_under(storage, &prefix).await {
            tracing::warn!(%session_id, error = %e, "could not sweep an idle session's files");
            continue;
        }
        sqlx::query("update agent_sessions set files_swept_at = now() where id = $1")
            .bind(session_id)
            .execute(pool)
            .await?;
        done += 1;
    }
    Ok(done)
}

/// Every object under a prefix, and the text read out of each: what a reader
/// could still be served for a file the listing no longer shows.
async fn remove_under(storage: &Arc<dyn StorageBackend>, prefix: &str) -> anyhow::Result<()> {
    for under in [prefix.to_string(), outturn_core::extract::text_key(prefix)] {
        for object in storage.list(&under).await? {
            match storage.delete(&object.path).await {
                Ok(()) | Err(outturn_core::storage::StorageError::NotFound) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}
