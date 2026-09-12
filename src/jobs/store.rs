use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::PgPool;
use sqlx::{Executor, Postgres, Row};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct Job {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub max_attempts: i32,
    /// Identifies this claim. A heartbeat quotes it back, so a renewal from a
    /// claim that was reaped cannot extend whichever claim replaced it.
    pub lease_token: Option<Uuid>,
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
    // Absent where a job is read rather than claimed: `get` does not need the
    // token, and reading one it has no business with would invite quoting it.
    Job {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        kind: row.get("kind"),
        payload: row.get("payload"),
        attempts: row.get("attempts"),
        max_attempts: row.get("max_attempts"),
        lease_token: row.try_get("lease_token").ok(),
    }
}

/// Enqueues work.
///
/// Takes any executor so the enqueue can share the transaction of the change
/// that caused it: commit together or not at all, with no outbox to reconcile.
pub async fn enqueue<'e, E>(
    executor: E,
    workspace_id: Uuid,
    kind: &str,
    payload: serde_json::Value,
    delay: Option<Duration>,
    // Work sharing a key never runs concurrently. None leaves it unconstrained.
    serial_key: Option<&str>,
    // What is waiting on this. See PRIORITY_REALTIME and PRIORITY_BACKGROUND:
    // the caller knows whether a person is watching, and nothing downstream
    // can work it out afterwards.
    priority: i32,
) -> Result<Uuid, JobError>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let delay_secs = delay.map(|d| d.as_secs_f64()).unwrap_or(0.0);

    sqlx::query(
        "insert into jobs (id, workspace_id, kind, payload, run_after, serial_key, priority) \
         values ($1, $2, $3, $4, now() + make_interval(secs => $5), $6, $7)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(kind)
    .bind(&payload)
    .bind(delay_secs)
    .bind(serial_key)
    .bind(priority)
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
    //
    // Keys already running are excluded *here*, before the limit is applied,
    // and not only in the guard below. The guard alone is not enough: with a
    // limit of one, a queued turn for a session whose previous turn is still
    // running is the top candidate, the guard rejects it, and nothing else is
    // ever looked at -- so one busy session stalls every claim in the fleet
    // until its turn ends. This check is against a possibly stale snapshot,
    // which is why the guard stays; it only has to be right often enough that
    // the limit is not spent on work that cannot start.
    let picked: Vec<Uuid> = sqlx::query_scalar(
        "with candidate as ( \
             select id, serial_key from jobs \
             where state = 'pending' \
               and run_after <= now() \
               and (cardinality($1::text[]) = 0 or kind = any($1)) \
               and (serial_key is null or not exists ( \
                   select 1 from jobs running \
                   where running.state = 'running' \
                     and running.serial_key = jobs.serial_key)) \
             order by priority, run_after, id \
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
             lease_token = gen_random_uuid(), \
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
         returning id, workspace_id, kind, payload, attempts, max_attempts, lease_token",
    )
    .bind(&picked)
    .bind(lease.as_secs_f64())
    .fetch_all(&mut *tx)
    .await
    .map_err(internal)?;

    tx.commit().await.map_err(internal)?;

    Ok(rows.iter().map(|r| JobHandle { job: read_job(r) }).collect())
}

/// Closes a job out as done.
///
/// `token` is the lease the caller holds. Matched so a runtime whose lease
/// lapsed -- and whose turn has since been handed to another pod -- cannot
/// finish the job out from under the pod now running it. Passing None skips
/// the check, for callers closing a job they know nobody else can hold.
pub async fn complete(pool: &PgPool, id: Uuid, token: Option<Uuid>) -> Result<(), JobError> {
    let result = sqlx::query(
        "update jobs set state = 'succeeded', leased_until = null, updated_at = now() \
         where id = $1 and state = 'running' and ($2::uuid is null or lease_token = $2)",
    )
    .bind(id)
    .bind(token)
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
///
/// `token` as for `complete`: a stale holder must not fail a job somebody
/// else is now running.
pub async fn fail(
    pool: &PgPool,
    id: Uuid,
    error: &str,
    backoff: Duration,
    token: Option<Uuid>,
) -> Result<(), JobError> {
    let result = sqlx::query(
        "update jobs set \
             state = case when attempts >= max_attempts then 'failed' else 'pending' end, \
             last_error = $2, \
             leased_until = null, \
             run_after = now() + make_interval(secs => $3), \
             updated_at = now() \
         where id = $1 and state = 'running' and ($4::uuid is null or lease_token = $4)",
    )
    .bind(id)
    .bind(error)
    .bind(backoff.as_secs_f64())
    .bind(token)
    .execute(pool)
    .await
    .map_err(internal)?;

    if result.rows_affected() == 0 {
        return Err(JobError::NotFound);
    }
    Ok(())
}

/// Reads one job.
///
/// For the tier recording a turn's result, which knows the job by id and needs
/// what it was for. Deliberately not scoped by state: a job whose lease
/// lapsed while it ran still has to be recognisable when its results arrive.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Job, JobError> {
    let row = sqlx::query(
        "select id, workspace_id, kind, payload, attempts, max_attempts, lease_token \
         from jobs where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(internal)?
    .ok_or(JobError::NotFound)?;

    Ok(read_job(&row))
}

/// Whether a job is out with something that claimed it.
///
/// The claim is what authorises reporting a turn: a job that is pending was
/// never handed out, and one that has finished was reported already. Checking
/// the state is what stops a job id alone from being enough to write into a
/// transcript.
pub async fn is_running(pool: &PgPool, id: Uuid) -> Result<bool, JobError> {
    let state: Option<String> = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(internal)?;
    Ok(state.as_deref() == Some("running"))
}

/// The turn a session currently has in flight, if any.
///
/// Found by the session rather than by the job id because that is what a
/// reader has: somebody pressing stop knows which conversation they are
/// watching, not which row in a queue is answering it.
///
/// Only pending and running qualify. A finished turn is not stoppable, and
/// picking the newest keeps a session that has queued several from stopping
/// an older one by accident.
pub async fn live_turn_for_session(
    pool: &PgPool,
    workspace_id: Uuid,
    session_id: Uuid,
) -> Result<Option<Uuid>, JobError> {
    sqlx::query_scalar(
        "select id from jobs \
          where kind = 'chat.turn' \
            and workspace_id = $1 \
            and (payload->>'session_id')::uuid = $2 \
            and state in ('pending', 'running') \
          order by id desc limit 1",
    )
    .bind(workspace_id)
    .bind(session_id)
    .fetch_optional(pool)
    .await
    .map_err(internal)
}

/// What asking for a job to stop achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancelled {
    /// It had not started. Nothing is running, so it is finished here and now.
    BeforeItRan,
    /// It is running. The request is recorded and whoever is running it will
    /// find out at its next round boundary.
    WhileRunning,
    /// It had already finished, one way or another. Nothing to stop, and
    /// nothing about that is an error.
    AlreadyOver,
}

/// Asks for a job to stop.
///
/// A pending job is cancelled outright: no runtime has it, so there is nobody
/// to tell and nothing in flight to unwind. A running one gets the request
/// recorded against it instead -- the work is happening in another process,
/// possibly on another machine, and the only honest thing this can do is
/// write down that somebody asked.
///
/// Recorded rather than signalled because a signal needs a listener that
/// exists right now. A row survives the pod running the turn being lost, so a
/// runtime that takes the turn over afterwards learns about the cancel the
/// same way the first one would have.
pub async fn request_cancel(pool: &PgPool, id: Uuid) -> Result<Cancelled, JobError> {
    let state: Option<String> = sqlx::query_scalar(
        "update jobs \
            set cancel_requested_at = coalesce(cancel_requested_at, now()), \
                state = case when state = 'pending' then 'cancelled' else state end, \
                leased_until = case when state = 'pending' then null else leased_until end, \
                updated_at = now() \
          where id = $1 and state in ('pending', 'running') \
      returning state",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;

    Ok(match state.as_deref() {
        Some("cancelled") => Cancelled::BeforeItRan,
        Some("running") => Cancelled::WhileRunning,
        // Nothing matched the update, so it had already finished -- or never
        // existed, which from here is the same absence of work to stop.
        _ => Cancelled::AlreadyOver,
    })
}

/// Whether somebody has asked for this job to stop.
///
/// Asked at a round boundary by whoever is running the turn. Cheap on purpose:
/// it is one indexed lookup per round, not per token, and a round is the only
/// place stopping is safe anyway.
pub async fn cancel_requested(pool: &PgPool, id: Uuid) -> Result<bool, JobError> {
    let asked: Option<bool> =
        sqlx::query_scalar("select cancel_requested_at is not null from jobs where id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(internal)?;
    Ok(asked == Some(true))
}

/// Marks a running job stopped, at the request of whoever asked.
///
/// Terminal, and deliberately not `fail`: a failure is retried while attempts
/// remain, and retrying a turn somebody stopped would be precisely the
/// opposite of what they asked for.
pub async fn mark_cancelled(pool: &PgPool, id: Uuid, token: Option<Uuid>) -> Result<(), JobError> {
    let result = sqlx::query(
        "update jobs set state = 'cancelled', leased_until = null, updated_at = now() \
         where id = $1 and state = 'running' and ($2::uuid is null or lease_token = $2)",
    )
    .bind(id)
    .bind(token)
    .execute(pool)
    .await
    .map_err(internal)?;

    if result.rows_affected() == 0 {
        return Err(JobError::NotFound);
    }
    Ok(())
}

/// Whether `token` is the lease currently held on a running job.
///
/// The ticket a runtime presents when it reports or hands back a turn. A job
/// id alone is not enough: after a lease lapses the same id is out with
/// another pod, and the first pod's report would otherwise be written over
/// the second's.
pub async fn holds_lease(pool: &PgPool, id: Uuid, token: Uuid) -> Result<bool, JobError> {
    let held: Option<bool> = sqlx::query_scalar(
        "select lease_token = $2 from jobs where id = $1 and state = 'running'",
    )
    .bind(id)
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;
    Ok(held == Some(true))
}

/// What became of a job that was handed back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Released {
    /// Back in the queue, waiting for somewhere with room.
    Queued,
    /// Handed back too many times; treated as failed so someone is told.
    GaveUp,
}

/// Returns a job to the queue without holding it against the job.
///
/// For work that was claimed and then found to have nowhere to run: a runtime
/// at capacity, a pod shutting down. Nothing was attempted, so the attempt the
/// claim counted is given back -- otherwise a cluster that is merely busy
/// would burn through a job's retries without ever having run it once, and the
/// user would be told their turn failed because the cluster was popular.
///
/// Releases are counted separately, and past `MAX_RELEASES` the job fails.
/// Giving back the attempt every time and counting nothing would leave a
/// permanently full cluster claiming and releasing the same work forever, with
/// no terminal state and nobody told -- a session showing an indicator that
/// resolves on no timescale at all.
///
/// `token` as for `complete`: only the holder of the current lease may hand
/// the job back, or a pod whose lease lapsed would return work that another
/// pod is in the middle of.
pub async fn release(
    pool: &PgPool,
    id: Uuid,
    delay: Duration,
    max_releases: i32,
    token: Option<Uuid>,
) -> Result<Released, JobError> {
    let state: Option<String> = sqlx::query_scalar(
        "update jobs set \
             state = case when releases + 1 >= $3 then 'failed' else 'pending' end, \
             releases = releases + 1, \
             attempts = greatest(attempts - 1, 0), \
             last_error = case when releases + 1 >= $3 \
                               then 'no runtime had room' else last_error end, \
             leased_until = null, \
             run_after = now() + make_interval(secs => $2), \
             updated_at = now() \
         where id = $1 and state = 'running' and ($4::uuid is null or lease_token = $4) \
         returning state",
    )
    .bind(id)
    .bind(delay.as_secs_f64())
    .bind(max_releases)
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(internal)?;

    match state.as_deref() {
        Some("failed") => Ok(Released::GaveUp),
        Some(_) => Ok(Released::Queued),
        None => Err(JobError::NotFound),
    }
}

/// Returns jobs whose lease expired to the pending pool.
///
/// This is what makes a crashed worker recoverable: the claim is a lease, not
/// a transfer of ownership. Attempts are not incremented again here — the
/// claim already counted it.
///
/// Returns how many were reaped, and the jobs that were parked as failed
/// rather than retried -- those have nobody left to tell the user, so the
/// caller has to.
pub async fn reap_abandoned(pool: &PgPool) -> Result<(u64, Vec<Job>), JobError> {
    let rows = sqlx::query(
        "update jobs set \
             state = case when attempts >= max_attempts then 'failed' else 'pending' end, \
             last_error = coalesce(last_error, 'lease expired'), \
             leased_until = null, \
             updated_at = now() \
         where state = 'running' and leased_until < now() \
         returning id, workspace_id, kind, payload, attempts, max_attempts, state",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?;

    let gave_up = rows
        .iter()
        .filter(|r| r.get::<String, _>("state") == "failed")
        .map(read_job)
        .collect();
    Ok((rows.len() as u64, gave_up))
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
    token: Uuid,
) -> Result<bool, JobError> {
    // Matched on the token rather than on state or time. A turn whose lease
    // lapsed has been handed back and may already be running somewhere else;
    // renewing on state alone extends whichever claim is current while telling
    // the previous holder it still owns the job, and both go on to stream the
    // same turn into the same reply. Timing cannot separate them either, since
    // the new holder's lease is fresh.
    let result = sqlx::query(
        "update jobs set leased_until = now() + make_interval(secs => $2), \
             updated_at = now() \
         where id = $1 and state = 'running' and lease_token = $3",
    )
    .bind(id)
    .bind(lease.as_secs_f64())
    .bind(token)
    .execute(pool)
    .await
    .map_err(internal)?;

    Ok(result.rows_affected() == 1)
}
