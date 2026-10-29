//! What each workspace holds, counted per object as it is written and
//! deleted, for the storage quota. See `docs/storage-quota.md`.
//!
//! In core because the tiers that write with a database -- the API's files and
//! the extraction it runs -- both record here. The runtime holds no database:
//! it reports each write and delete, and the API records those.

use sqlx::PgExecutor;
use uuid::Uuid;

use super::scope::Scope;
use crate::extract;

/// The workspace an object counts against, from its key: a scope's prefix,
/// or text extracted from one. Skill versions are not counted -- they are
/// content-addressed, and a package's size is bounded where it is published.
pub fn workspace_of(key: &str) -> Option<Uuid> {
    let key = key.strip_prefix("extracted/").unwrap_or(key);
    Scope::ALL.iter().find_map(|s| {
        let rest = key.strip_prefix(s.bucket_prefix())?;
        let (id, _) = rest.split_once('/')?;
        id.parse().ok()
    })
}

/// Records an object at its size, replacing what was recorded for its key.
/// A key no workspace owns is not recorded.
pub async fn record<'e>(
    executor: impl PgExecutor<'e>,
    key: &str,
    bytes: u64,
) -> Result<(), sqlx::Error> {
    let Some(workspace_id) = workspace_of(key) else {
        return Ok(());
    };
    sqlx::query(
        "insert into stored_objects (key, workspace_id, bytes) values ($1, $2, $3) \
         on conflict (key) do update set bytes = excluded.bytes, written_at = now()",
    )
    .bind(key)
    .bind(workspace_id)
    .bind(i64::try_from(bytes).unwrap_or(i64::MAX))
    .execute(executor)
    .await?;
    Ok(())
}

/// Forgets an object, and the text extracted from it, which goes with it.
pub async fn forget<'e>(executor: impl PgExecutor<'e>, key: &str) -> Result<(), sqlx::Error> {
    forget_keys(
        executor,
        &[
            key.to_string(),
            extract::text_key(key),
            extract::failed_key(key),
        ],
    )
    .await
}

/// Forgets objects by key, and notes when, so a listing already under way
/// does not count them again (`forgotten_since`).
pub async fn forget_keys<'e>(
    executor: impl PgExecutor<'e>,
    keys: &[String],
) -> Result<(), sqlx::Error> {
    let owners: Vec<Option<Uuid>> = keys.iter().map(|k| workspace_of(k)).collect();
    sqlx::query(
        "with gone as (delete from stored_objects where key = any($1)) \
         insert into stored_forgets (key, workspace_id) \
         select k, w from unnest($1::text[], $2::uuid[]) as t(k, w) where w is not null \
         on conflict (key) do update set forgotten_at = now()",
    )
    .bind(keys)
    .bind(&owners)
    .execute(executor)
    .await?;
    Ok(())
}

/// Forgets every object under a prefix, as a sweep removes them.
pub async fn forget_under<'e>(
    executor: impl PgExecutor<'e>,
    prefix: &str,
) -> Result<(), sqlx::Error> {
    // A prefix is a key's start; `like` would read its `_` and `%` as
    // patterns, so it is compared as text.
    sqlx::query(
        "with gone as (delete from stored_objects where left(key, length($1)) = $1 \
                       returning key, workspace_id) \
         insert into stored_forgets (key, workspace_id) select key, workspace_id from gone \
         on conflict (key) do update set forgotten_at = now()",
    )
    .bind(prefix)
    .execute(executor)
    .await?;
    Ok(())
}

/// What a workspace may still write, in bytes: `None` for no limit, or for a
/// workspace whose holdings have not been counted against the bucket yet.
pub async fn remaining<'e>(
    executor: impl PgExecutor<'e>,
    workspace_id: Uuid,
    quota: u64,
) -> Result<Option<u64>, sqlx::Error> {
    if quota == 0 {
        return Ok(None);
    }
    let row: Option<(i64, bool)> = sqlx::query_as(
        "select stored_bytes, stored_counted_at is not null from workspaces where id = $1",
    )
    .bind(workspace_id)
    .fetch_optional(executor)
    .await?;
    Ok(match row {
        Some((held, true)) => Some(quota.saturating_sub(u64::try_from(held).unwrap_or(0))),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_counts_against_the_workspace_in_its_key() {
        let ws = Uuid::now_v7();
        let agent = Uuid::now_v7();
        for key in [
            format!("workspaces/{ws}/reference/pricing.csv"),
            format!("agents/{ws}/{agent}/notes.md"),
            format!("sessions/{ws}/{agent}/{agent}/upload.pdf"),
            format!("extracted/sessions/{ws}/{agent}/{agent}/upload.pdf"),
        ] {
            assert_eq!(workspace_of(&key), Some(ws), "{key}");
        }
        assert_eq!(workspace_of(&format!("skills/{ws}/abc123")), None);
        // What a runtime may report is the same answer, less extracted text.
        let own = format!("sessions/{ws}/{agent}/{agent}/upload.pdf");
        assert!(extract::belongs_to(&own, ws));
        assert!(!extract::belongs_to(&own, Uuid::now_v7()));
        assert!(!extract::belongs_to(&extract::text_key(&own), ws));
        assert_eq!(workspace_of("workspaces/not-a-uuid/x"), None);
    }
}
