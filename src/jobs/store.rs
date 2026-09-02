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
) -> Result<Uuid, JobError>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let delay_secs = delay.map(|d| d.as_secs_f64()).unwrap_or(0.0);

    sqlx::query(
        "insert into jobs (id, tenant_id, kind, payload, run_after) \
         values ($1, $2, $3, $4, now() + make_interval(secs => $5))",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(kind)
    .bind(&payload)
    .bind(delay_secs)
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
    let rows = sqlx::query(
        "with claimed as ( \
             select id from jobs \
             where state = 'pending' \
               and run_after <= now() \
               and (cardinality($1::text[]) = 0 or kind = any($1)) \
             order by run_after, id \
             for update skip locked \
             limit $2 \
         ) \
         update jobs set \
             state = 'running', \
             attempts = attempts + 1, \
             leased_until = now() + make_interval(secs => $3), \
             updated_at = now() \
         where id in (select id from claimed) \
         returning id, tenant_id, kind, payload, attempts, max_attempts",
    )
    .bind(kinds)
    .bind(limit)
    .bind(lease.as_secs_f64())
    .fetch_all(pool)
    .await
    .map_err(internal)?;

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
