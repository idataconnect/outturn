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
        account: r.get("account"),
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
        "select id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, account, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at \
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
        "select id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, account, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at \
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
              enabled, account, owner_id, next_run_at, created_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $10) \
         returning id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, account, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at",
    )
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(input.agent_id)
        .bind(input.name.trim())
        .bind(input.prompt.trim())
        .bind(input.expression.trim())
        .bind(input.timezone.trim())
        .bind(input.enabled)
        .bind(input.account.as_deref())
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
             enabled = $7, account = $8, next_run_at = $9, updated_at = now() \
         where workspace_id = $1 and id = $2 \
         returning id, workspace_id, agent_id, name, prompt, expression, timezone, enabled, account, owner_id, next_run_at, last_run_at, last_status, last_error, skipped, created_at",
    )
        .bind(workspace_id)
        .bind(id)
        .bind(input.name.trim())
        .bind(input.prompt.trim())
        .bind(input.expression.trim())
        .bind(input.timezone.trim())
        .bind(input.enabled)
        .bind(input.account.as_deref())
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

/// Takes one schedule that is due, with the firing it was owed.
///
/// `for update skip locked` and the immediate clear are what make this safe to
/// run in more than one API pod. Two loops ticking at once would otherwise
/// both see the same due row and both fire it, and a duplicate turn is a turn
/// that may act on the world twice.
///
/// The clear destroys the one thing that says what this firing was *for*, so
/// it comes back separately: `next_run_at` on the returned row is the
/// post-update value and always null. That time is what says how many firings
/// were missed while nothing was running -- an earlier version passed `now` in
/// its place, which made every firing look punctual and the skip count
/// permanently zero.
///
/// A crash between here and writing the successor leaves a schedule that stops
/// rather than one that fires twice.
///
/// `next_run_at` on the returned row is the post-update value and therefore
/// always null, so the time this firing was *for* has to come back separately
/// -- it is what says how many firings were missed while nothing was running.
/// An earlier version passed `now` in its place, which made every firing look
/// punctual and the skip count permanently zero.
pub async fn take_due(
    pool: &PgPool,
    now: DateTime<Utc>,
) -> Result<Option<(Schedule, DateTime<Utc>)>, sqlx::Error> {
    // The due row is chosen in a CTE, which is what makes its `next_run_at`
    // readable at all: RETURNING sees the new row, and Postgres has no OLD to
    // ask. `for update skip locked` in the CTE is also what stops two API pods
    // taking the same firing.
    let found = sqlx::query(
        "with due as ( \
             select id, next_run_at from schedules \
             where enabled and next_run_at is not null and next_run_at <= $1 \
             order by next_run_at \
             for update skip locked \
             limit 1 \
         ), \
         taken as ( \
             update schedules set next_run_at = null, updated_at = now() \
             where id in (select id from due) \
             returning id, workspace_id, agent_id, name, prompt, expression, timezone, \
                       enabled, account, owner_id, next_run_at, last_run_at, last_status, \
                       last_error, skipped, created_at \
         ) \
         select taken.*, due.next_run_at as was_due \
         from taken join due on due.id = taken.id",
    )
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(found.as_ref().map(|r| {
        let owed: Option<DateTime<Utc>> = r.get("was_due");
        // `owed` cannot be null -- the row was selected on `next_run_at is not
        // null` -- but falling back to `now` degrades to "nothing was missed"
        // rather than panicking in a loop nobody is watching.
        (row(r), owed.unwrap_or(now))
    }))
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
