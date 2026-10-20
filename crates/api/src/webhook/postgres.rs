//! Reading and writing triggers, and the one statement that admits a delivery.
//!
//! A webhook is a `triggers` row of kind `webhook`: its path, scheme and
//! secret are in `config`. Read back under the names a `Trigger` has always
//! had, so nothing above this file knows the table changed.

use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use super::{Trigger, TriggerInput};

/// The columns a webhook `Trigger` is read from, by the names it gives them.
macro_rules! columns {
    () => {
        "id, workspace_id, agent_id, kind, name, config->>'path' as path, \
         coalesce(config->'senders', '[]'::jsonb) as senders, \
         coalesce((config->>'authenticated')::bool, true) as authenticated, \
         config->>'scheme' as scheme, config->>'secret' as secret, prompt, enabled, \
         account, owner_id, coalesce(max_per_hour, 60) as max_per_hour, last_at, \
         last_status, last_error, refused, created_at"
    };
}

fn row(r: &PgRow) -> Trigger {
    Trigger {
        id: r.get("id"),
        workspace_id: r.get("workspace_id"),
        agent_id: r.get("agent_id"),
        kind: r.get("kind"),
        senders: serde_json::from_value(r.get("senders")).unwrap_or_default(),
        authenticated: r.get("authenticated"),
        name: r.get("name"),
        path: r.get("path"),
        scheme: r.get("scheme"),
        secret: r.get("secret"),
        prompt: r.get("prompt"),
        enabled: r.get("enabled"),
        account: r.get("account"),
        owner_id: r.get("owner_id"),
        max_per_hour: r.get("max_per_hour"),
        last_at: r.get("last_at"),
        last_status: r.get("last_status"),
        last_error: r.get("last_error"),
        refused: r.get("refused"),
        created_at: r.get("created_at"),
    }
}

pub async fn list(
    pool: &PgPool,
    workspace_id: Uuid,
    agent_id: Option<Uuid>,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Trigger>, sqlx::Error> {
    let rows = sqlx::query(concat!(
        "select ",
        columns!(),
        " from triggers \
          where kind in ('webhook', 'email') and workspace_id = $1 and ($2::uuid is null or agent_id = $2) \
            and ($3::uuid is null or id > $3) \
          order by id limit $4"
    ))
    .bind(workspace_id)
    .bind(agent_id)
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(row).collect())
}

pub async fn get(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<Option<Trigger>, sqlx::Error> {
    let found = sqlx::query(concat!(
        "select ",
        columns!(),
        " from triggers where kind in ('webhook', 'email') and workspace_id = $1 and id = $2"
    ))
    .bind(workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(found.as_ref().map(row))
}

pub async fn create(
    pool: &PgPool,
    workspace_id: Uuid,
    owner_id: Option<Uuid>,
    path: &str,
    secret: &str,
    input: &TriggerInput,
) -> Result<Trigger, sqlx::Error> {
    let r = sqlx::query(concat!(
        "insert into triggers \
             (id, workspace_id, agent_id, kind, name, config, prompt, \
              enabled, account, owner_id, max_per_hour, created_by) \
         values ($1, $2, $3, $13, $4, \
                 jsonb_build_object('path', $5::text, 'scheme', $6::text, 'secret', $7::text, \
                                    'senders', $14::jsonb, 'authenticated', $15::bool), \
                 $8, $9, $10, $11, $12, $11) \
         returning ",
        columns!()
    ))
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(input.agent_id)
    .bind(input.name.trim())
    .bind(path)
    .bind(input.scheme.trim())
    .bind(secret)
    .bind(input.prompt.trim())
    .bind(input.enabled)
    .bind(input.account.as_deref())
    .bind(owner_id)
    .bind(input.max_per_hour)
    .bind(&input.kind)
    .bind(serde_json::json!(input.senders))
    .bind(input.authenticated)
    .fetch_one(pool)
    .await?;
    Ok(row(&r))
}

/// Replaces what a person may change.
///
/// Not the path and not the secret: rotating either is a deliberate act with
/// consequences for whoever is sending, so it belongs behind its own call
/// rather than happening because somebody edited a name.
pub async fn update(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
    input: &TriggerInput,
) -> Result<Option<Trigger>, sqlx::Error> {
    let found = sqlx::query(concat!(
        "update triggers set \
             name = $3, prompt = $4, \
             config = config || jsonb_build_object('scheme', $5::text, \
                 'senders', $9::jsonb, 'authenticated', $10::bool), \
             enabled = $6, account = $7, max_per_hour = $8, updated_at = now() \
         where kind in ('webhook', 'email') and workspace_id = $1 and id = $2 \
         returning ",
        columns!()
    ))
    .bind(workspace_id)
    .bind(id)
    .bind(input.name.trim())
    .bind(input.prompt.trim())
    .bind(input.scheme.trim())
    .bind(input.enabled)
    .bind(input.account.as_deref())
    .bind(input.max_per_hour)
    .bind(serde_json::json!(input.senders))
    .bind(input.authenticated)
    .fetch_optional(pool)
    .await?;
    Ok(found.as_ref().map(row))
}

pub async fn delete(pool: &PgPool, workspace_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "delete from triggers where kind in ('webhook', 'email') and workspace_id = $1 and id = $2",
    )
    .bind(workspace_id)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The trigger at a path, whoever is asking.
///
/// Workspace-blind, because the caller has no workspace: this is reached by
/// whoever holds the URL. The path is the only thing that selects a row, and
/// everything that decides whether the delivery is honored happens after.
pub async fn by_path(pool: &PgPool, path: &str) -> Result<Option<Trigger>, sqlx::Error> {
    let found = sqlx::query(concat!(
        "select ",
        columns!(),
        " from triggers where kind in ('webhook', 'email') and config->>'path' = $1"
    ))
    .bind(path)
    .fetch_optional(pool)
    .await?;
    Ok(found.as_ref().map(row))
}

/// Records that a delivery was turned away by the ceiling.
///
/// Counted separately from the window, and never reset: a hook quietly
/// dropping half its traffic looks exactly like a sender that stopped sending,
/// and the difference is this number.
///
/// `counts` is false for a refusal that is not the ceiling's -- a replay,
/// which the sender should see and fix but which says nothing about the rate.
/// The column means one thing, so an operator watching it climb knows to look
/// at the rate rather than at a duplicate.
pub async fn record_refusal(
    pool: &PgPool,
    id: Uuid,
    reason: &str,
    counts: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update triggers set \
             refused = refused + case when $3 then 1 else 0 end, \
             last_at = now(), last_status = 'refused', \
             last_error = $2, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .bind(reason)
    .bind(counts)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records what a delivery produced.
pub async fn record_delivery(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update triggers set \
             last_at = now(), last_status = $2, last_error = $3, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records a delivery as seen, and says whether it is the first time.
///
/// The insert *is* the check: a primary key on (trigger, digest) means a
/// second presentation of the same credential cannot be stored, and
/// `on conflict do nothing` turns that into an answer rather than an error.
/// One statement, one index probe, no read-then-write race -- two deliveries
/// arriving together cannot both be told they are first.
///
/// Returns `true` when this delivery has not been seen. A `false` is a replay
/// and must be refused.
pub async fn remember(
    pool: &PgPool,
    trigger_id: Uuid,
    digest: &[u8],
    expires_at: chrono::DateTime<chrono::Utc>,
    claimed_by: Uuid,
) -> Result<bool, sqlx::Error> {
    let stored = sqlx::query(
        "insert into webhook_deliveries (trigger_id, digest, expires_at, claimed_by) \
         values ($1, $2, $3, $4) \
         on conflict (trigger_id, digest) do nothing",
    )
    .bind(trigger_id)
    .bind(digest)
    .bind(expires_at)
    .bind(claimed_by)
    .execute(pool)
    .await?;
    Ok(stored.rows_affected() > 0)
}

/// Releases a delivery that was remembered but never became a turn.
///
/// `remember` runs before the ceiling and before the turn is started, because
/// a replay must not spend either. The cost of that ordering is this: a
/// delivery refused by the ceiling, or one whose turn failed to start, has
/// already claimed its slot. Left there, the retry that a 429 or a 500 invites
/// is refused as a replay and the event is lost -- a worse outcome than the
/// duplicate turn the record exists to prevent, because it is silent.
///
/// So the record is released on every path that does not start a turn. It is
/// keyed on the same pair the insert used, so releasing one delivery cannot
/// touch another's.
pub async fn forget(
    pool: &PgPool,
    trigger_id: Uuid,
    digest: &[u8],
    claimed_by: Uuid,
) -> Result<(), sqlx::Error> {
    // `claimed_by` is what keeps this to the caller's own row. Without it two
    // identical deliveries racing would have the loser answered 409 -- "already
    // accepted" -- and the winner then delete the row on its way to a 429,
    // leaving the event refused, unrecorded and never run.
    sqlx::query(
        "delete from webhook_deliveries \
         where trigger_id = $1 and digest = $2 and claimed_by = $3",
    )
    .bind(trigger_id)
    .bind(digest)
    .bind(claimed_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Forgets deliveries that can no longer be replayed.
///
/// Swept opportunistically from the worker's tick rather than by a job of its
/// own, beside `live_sessions`: the table is small by construction -- it holds
/// at most a few minutes of traffic -- and a dedicated loop for something that
/// bounded is a loop to keep alive for no reason.
pub async fn forget_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let gone = sqlx::query("delete from webhook_deliveries where expires_at < now()")
        .execute(pool)
        .await?;
    Ok(gone.rows_affected())
}
