use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::PgPool;
use sqlx::{Executor, Postgres, Row};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
}

/// A claimed job. Dropping this does not release the lease — the reaper
/// reclaims it once `leased_until` passes.
#[derive(Debug, Clone)]
pub struct JobHandle {
    pub job: Job,
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("job not found")]
    NotFound,
    #[error("job store error: {0}")]
    Internal(String),
}

fn internal(e: sqlx::Error) -> JobError {
    JobError::Internal(e.to_string())
}

fn read_job(row: &sqlx::postgres::PgRow) -> Job {
    Job {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        kind: row.get("kind"),
        payload: row.get("payload"),
        attempts: row.get("attempts"),
        max_attempts: row.get("max_attempts"),
    }
}

/// Enqueues work.
///
/// Takes any executor so the enqueue can share the transaction of the change
/// that caused it: commit together or not at all, with no outbox to reconcile.
pub async fn enqueue<'e, E>(
    executor: E,
    tenant_id: Uuid,
    kind: &str,
    payload: serde_json::Value,
    delay: Option<Duration>,
    // Work sharing a key never runs concurrently. None leaves it unconstrained.
    serial_key: Option<&str>,
) -> Result<Uuid, JobError>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let delay_secs = delay.map(|d| d.as_secs_f64()).unwrap_or(0.0);

    sqlx::query(
        "insert into jobs (id, tenant_id, kind, payload, run_after, serial_key) \
         values ($1, $2, $3, $4, now() + make_interval(secs => $5), $6)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(kind)
    .bind(&payload)
    .bind(delay_secs)
    .bind(serial_key)
    .execute(executor)
    .await
    .map_err(internal)?;

    Ok(id)
}

/// Claims up to `limit` runnable jobs.
///
/// SKIP LOCKED lets concurrent workers claim disjoint sets without blocking on
/// each other: each skips rows another worker has locked rather than queueing
/// behind them.
pub async fn claim(
    pool: &PgPool,
    kinds: &[&str],
    limit: i64,
    lease: Duration,
) -> Result<Vec<JobHandle>, JobError> {
    // Serialisation takes two statements, and the second one is the point.
    //
    // Everything below used to be a single statement: pick candidates, take a
    // transaction-scoped advisory lock on the serial key, check that nothing
    // with that key is already running, claim. The advisory lock did serialise
    // correctly. It bought nothing, because under READ COMMITTED one statement
    // sees one snapshot, taken before the statement began -- so a claimer that
    // acquired the lock *after* another had claimed and committed still
    // evaluated `not exists` against a snapshot from before that commit, found
    // nothing running, and claimed a second job for the same key.
    //
    // The candidate rows never had this problem: `for update` re-checks each
    // row against its latest version. A `not exists` subquery gets no such
    // treatment.
    //
    // So the guard moves into a second statement, which under READ COMMITTED
    // takes a fresh snapshot and therefore sees the other claimer's commit.
    // The advisory lock is still needed, and now guards something real: it
    // keeps a second claimer from sitting between these two statements for the
    // same key, so the only claim the guard can miss is one that has not
    // committed -- and that one is still holding the lock.
    let mut tx = pool.begin().await.map_err(internal)?;

    // Rows stay locked for the rest of the transaction, so the update below
    // cannot collide with a concurrent claimer. The ranking handles two jobs
    // for one key inside a single batch, where the advisory lock is held by
    // this same transaction and so would admit both.
    let picked: Vec<Uuid> = sqlx::query_scalar(
        "with candidate as ( \
             select id, serial_key from jobs \
             where state = 'pending' \
               and run_after <= now() \
               and (cardinality($1::text[]) = 0 or kind = any($1)) \
             order by run_after, id \
             for update skip locked \
             limit $2 \
         ), \
         ranked as ( \
             select id, serial_key, \
                    row_number() over (partition by serial_key order by id) as rank \
             from candidate \
         ) \
         select id from ranked \
         where serial_key is null \
            or (rank = 1 and pg_try_advisory_xact_lock(hashtext(serial_key)))",
    )
    .bind(kinds)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await
    .map_err(internal)?;

    if picked.is_empty() {
        // Nothing to do, but the transaction still has to end -- and rolling
        // back releases the advisory locks sooner than dropping it would.
        tx.rollback().await.map_err(internal)?;
        return Ok(Vec::new());
    }

    let rows = sqlx::query(
        "update jobs set \
             state = 'running', \
             attempts = attempts + 1, \
             leased_until = now() + make_interval(secs => $2), \
             updated_at = now() \
         where id = any($1) \
           and ( \
                 jobs.serial_key is null \
                 or not exists ( \
                     select 1 from jobs running \
                     where running.state = 'running' \
                       and running.serial_key = jobs.serial_key \
                 ) \
               ) \
         returning id, tenant_id, kind, payload, attempts, max_attempts",
    )
    .bind(&picked)
    .bind(lease.as_secs_f64())
    .fetch_all(&mut *tx)
    .await
    .map_err(internal)?;

    tx.commit().await.map_err(internal)?;

    Ok(rows.iter().map(|r| JobHandle { job: read_job(r) }).collect())
}

pub async fn complete(pool: &PgPool, id: Uuid) -> Result<(), JobError> {
    let result = sqlx::query(
        "update jobs set state = 'succeeded', leased_until = null, updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    .map_err(internal)?;

    if result.rows_affected() == 0 {
        return Err(JobError::NotFound);
    }
    Ok(())
}

/// Records a failure. Retries with backoff while attempts remain, otherwise
/// parks the job in `failed`.
pub async fn fail(
    pool: &PgPool,
    id: Uuid,
    error: &str,
    backoff: Duration,
) -> Result<(), JobError> {
    let result = sqlx::query(
        "update jobs set \
             state = case when attempts >= max_attempts then 'failed' else 'pending' end, \
             last_error = $2, \
             leased_until = null, \
             run_after = now() + make_interval(secs => $3), \
             updated_at = now() \
         where id = $1",
    )
    .bind(id)
    .bind(error)
    .bind(backoff.as_secs_f64())
    .execute(pool)
    .await
    .map_err(internal)?;

    if result.rows_affected() == 0 {
        return Err(JobError::NotFound);
    }
    Ok(())
}

/// Returns jobs whose lease expired to the pending pool.
///
/// This is what makes a crashed worker recoverable: the claim is a lease, not
/// a transfer of ownership. Attempts are not incremented again here — the
/// claim already counted it.
pub async fn reap_abandoned(pool: &PgPool) -> Result<u64, JobError> {
    let result = sqlx::query(
        "update jobs set \
             state = case when attempts >= max_attempts then 'failed' else 'pending' end, \
             last_error = coalesce(last_error, 'lease expired'), \
             leased_until = null, \
             updated_at = now() \
         where state = 'running' and leased_until < now()",
    )
    .execute(pool)
    .await
    .map_err(internal)?;

    Ok(result.rows_affected())
}

/// Extends the lease on a job this worker is still running.
///
/// A long execution would otherwise outlive its lease and be reaped into the
/// queue while still in flight, producing a second execution of the same work.
/// Returns false if the job is no longer ours -- it was reaped and taken by
/// someone else -- which is the signal to abandon the work rather than finish
/// it and write a duplicate result.
pub async fn extend_lease(
    pool: &PgPool,
    id: Uuid,
    lease: Duration,
) -> Result<bool, JobError> {
    let result = sqlx::query(
        "update jobs set leased_until = now() + make_interval(secs => $2), \
             updated_at = now() \
         where id = $1 and state = 'running'",
    )
    .bind(id)
    .bind(lease.as_secs_f64())
    .execute(pool)
    .await
    .map_err(internal)?;

    Ok(result.rows_affected() == 1)
}
