//! Reading and writing schedules, and the one statement that fires them.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use super::{Schedule, ScheduleInput};

fn row(r: &PgRow) -> Schedule {
    Schedule {
        id: r.get("id"),
        workspace_id: r.get("workspace_id"),
        agent_id: r.get("agent_id"),
        name: r.get("name"),
        prompt: r.get("prompt"),
        expression: r.get("expression"),
        timezone: r.get("timezone"),
        enabled: r.get("enabled"),
        owner_id: r.get("owner_id"),
        next_run_at: r.get("next_run_at"),
        last_run_at: r.get("last_run_at"),
        last_status: r.get("last_status"),
        last_error: r.get("last_error"),
        skipped: r.get("skipped"),
        created_at: r.get("created_at"),
    }
}

pub async fn list(
    pool: &PgPool,
    workspace_id: Uuid,
    agent_id: Option<Uuid>,
) -> Result<Vec<Schedule>, sqlx::Error> {
    let rows = sqlx::query(
        "select id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at \
         from schedules \
         where workspace_id = $1 and ($2::uuid is null or agent_id = $2) \
         order by id",
    )
        .bind(workspace_id)
        .bind(agent_id)
        .fetch_all(pool)
        .await?;
    Ok(rows.iter().map(row).collect())
}

pub async fn get(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<Option<Schedule>, sqlx::Error> {
    let found = sqlx::query(
        "select id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at \
         from schedules where workspace_id = $1 and id = $2",
    )
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
    input: &ScheduleInput,
    next_run_at: Option<DateTime<Utc>>,
) -> Result<Schedule, sqlx::Error> {
    let r = sqlx::query(
        "insert into schedules \
             (id, workspace_id, agent_id, name, prompt, expression, timezone, \
              enabled, owner_id, next_run_at, created_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9) \
         returning id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at",
    )
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(input.agent_id)
        .bind(input.name.trim())
        .bind(input.prompt.trim())
        .bind(input.expression.trim())
        .bind(input.timezone.trim())
        .bind(input.enabled)
        .bind(owner_id)
        .bind(next_run_at)
        .fetch_one(pool)
        .await?;
    Ok(row(&r))
}

/// Replaces what a person may change, and recomputes when it next fires.
///
/// `next_run_at` is passed in rather than kept, because an edit to the
/// expression or the zone makes the stored one wrong -- and leaving it would
/// fire the old schedule once more before the new one took effect.
pub async fn update(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
    input: &ScheduleInput,
    next_run_at: Option<DateTime<Utc>>,
) -> Result<Option<Schedule>, sqlx::Error> {
    let found = sqlx::query(
        "update schedules set \
             name = $3, prompt = $4, expression = $5, timezone = $6, \
             enabled = $7, next_run_at = $8, updated_at = now() \
         where workspace_id = $1 and id = $2 \
         returning id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at",
    )
        .bind(workspace_id)
        .bind(id)
        .bind(input.name.trim())
        .bind(input.prompt.trim())
        .bind(input.expression.trim())
        .bind(input.timezone.trim())
        .bind(input.enabled)
        .bind(next_run_at)
        .fetch_optional(pool)
        .await?;
    Ok(found.as_ref().map(row))
}

pub async fn delete(pool: &PgPool, workspace_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("delete from schedules where workspace_id = $1 and id = $2")
        .bind(workspace_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Takes one schedule that is due, and clears its `next_run_at` in the same
/// statement.
///
/// `for update skip locked` and the immediate clear are what make this safe to
/// run in more than one API pod. Two loops ticking at once would otherwise
/// both see the same due row and both fire it, and a duplicate turn is a turn
/// that may act on the world twice.
///
/// The row comes back with `next_run_at` already null, so a crash between here
/// and writing the successor leaves a schedule that stops rather than one that
/// fires twice. Stopping is visible in the list; a double firing is not.
pub async fn take_due(pool: &PgPool, now: DateTime<Utc>) -> Result<Option<Schedule>, sqlx::Error> {
    let found = sqlx::query(
        "update schedules set next_run_at = null, updated_at = now() \
         where id = ( \
             select id from schedules \
             where enabled and next_run_at is not null and next_run_at <= $1 \
             order by next_run_at \
             for update skip locked \
             limit 1 \
         ) \
         returning id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at",
    )
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(found.as_ref().map(row))
}

/// Records what a firing did and when it should happen again.
pub async fn record_run(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    error: Option<&str>,
    next_run_at: Option<DateTime<Utc>>,
    skipped: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update schedules set \
             last_run_at = now(), last_status = $2, last_error = $3, \
             next_run_at = $4, skipped = skipped + $5, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(error)
    .bind(next_run_at)
    .bind(skipped)
    .execute(pool)
    .await?;
    Ok(())
}
