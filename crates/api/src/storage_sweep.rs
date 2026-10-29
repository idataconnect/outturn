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
    match recount(pool, storage).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(workspaces = n, "counted what workspaces hold"),
        Err(e) => tracing::warn!(error = %e, "could not count what workspaces hold"),
    }
}

/// How often what a workspace holds is listed from the bucket, to correct
/// what was counted as it was written.
const RECOUNT_DAYS: i32 = 7;

/// Workspaces listed per sweep: a listing is many requests to the store, and
/// the sweep is hourly, so the week's round is spread across it.
const RECOUNT_BATCH: i64 = 10;

/// Replaces each due workspace's count with what the bucket holds: what the
/// runtime never reported -- a pod that died mid-turn, a runtime too old to
/// say -- and the first count of a workspace, until which no quota applies.
/// See docs/storage-quota.md, *Reconciliation*.
async fn recount(pool: &PgPool, storage: &Arc<dyn StorageBackend>) -> anyhow::Result<u64> {
    let due: Vec<Uuid> = sqlx::query_scalar(
        "select id from workspaces \
         where stored_counted_at is null \
            or stored_counted_at < now() - make_interval(days => $1) \
         order by stored_counted_at nulls first limit $2",
    )
    .bind(RECOUNT_DAYS)
    .bind(RECOUNT_BATCH)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for workspace_id in due {
        if let Err(e) = recount_one(pool, storage, workspace_id).await {
            tracing::warn!(%workspace_id, error = %e, "could not count a workspace's files");
            continue;
        }
        done += 1;
    }
    Ok(done)
}

async fn recount_one(
    pool: &PgPool,
    storage: &Arc<dyn StorageBackend>,
    workspace_id: Uuid,
) -> anyhow::Result<()> {
    // Rows written after the listing began are the writes it may have missed,
    // so they are kept rather than replaced.
    let began: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select now()").fetch_one(pool).await?;
    let mut keys = Vec::new();
    let mut sizes = Vec::new();
    for s in Scope::ALL {
        let root = format!("{}{workspace_id}/", s.bucket_prefix());
        for under in [root.clone(), outturn_core::extract::text_key(&root)] {
            for object in storage.list(&under).await? {
                if !object.is_dir {
                    keys.push(object.path);
                    sizes.push(i64::try_from(object.size).unwrap_or(i64::MAX));
                }
            }
        }
    }

    let mut tx = pool.begin().await?;
    let before: i64 = sqlx::query_scalar("select stored_bytes from workspaces where id = $1")
        .bind(workspace_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("delete from stored_objects where workspace_id = $1 and written_at < $2")
        .bind(workspace_id)
        .bind(began)
        .execute(&mut *tx)
        .await?;
    // Neither a key written since the listing began, which is kept as it
    // is, nor one deleted since, which the listing may still have seen.
    sqlx::query(
        "insert into stored_objects (key, workspace_id, bytes, written_at) \
         select k, $1, b, $4 from unnest($2::text[], $3::bigint[]) as t(k, b) \
         where not exists (select 1 from stored_forgets f \
                           where f.key = t.k and f.forgotten_at >= $4) \
         on conflict (key) do nothing",
    )
    .bind(workspace_id)
    .bind(&keys)
    .bind(&sizes)
    .bind(began)
    .execute(&mut *tx)
    .await?;
    let after: i64 = sqlx::query_scalar(
        "update workspaces set stored_counted_at = now() where id = $1 returning stored_bytes",
    )
    .bind(workspace_id)
    .fetch_one(&mut *tx)
    .await?;
    // A forget matters only while a listing that began before it is under
    // way, and none takes a day.
    sqlx::query("delete from stored_forgets where forgotten_at < now() - interval '1 day'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if before != after {
        // A pattern here -- the same workspace corrected upward week after
        // week -- is writes going unreported, which is worth a look.
        tracing::info!(%workspace_id, before, after, "corrected what a workspace holds");
    }
    Ok(())
}

async fn deleted(pool: &PgPool, storage: &Arc<dyn StorageBackend>) -> anyhow::Result<u64> {
    let prefixes: Vec<(String,)> =
        sqlx::query_as("select prefix from storage_deletions order by requested_at limit $1")
            .bind(BATCH)
            .fetch_all(pool)
            .await?;
    let mut done = 0;
    for (prefix,) in prefixes {
        if let Err(e) = remove_under(pool, storage, &prefix).await {
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
        if let Err(e) = remove_under(pool, storage, &prefix).await {
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
///
/// Uncounted from the storage quota once each is gone, so a workspace is
/// never held over for what a sweep removed.
async fn remove_under(
    pool: &PgPool,
    storage: &Arc<dyn StorageBackend>,
    prefix: &str,
) -> anyhow::Result<()> {
    for under in [prefix.to_string(), outturn_core::extract::text_key(prefix)] {
        let mut gone = Vec::new();
        for object in storage.list(&under).await? {
            match storage.delete(&object.path).await {
                Ok(()) | Err(outturn_core::storage::StorageError::NotFound) => {}
                Err(e) => return Err(e.into()),
            }
            gone.push(object.path);
        }
        // What was counted under it, and what was removed whether counted or
        // not: a listing under way may have seen either.
        outturn_core::storage::stored::forget_under(pool, &under).await?;
        outturn_core::storage::stored::forget_keys(pool, &gone).await?;
    }
    Ok(())
}
