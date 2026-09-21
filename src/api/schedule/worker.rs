//! The loop that fires schedules.
//!
//! Runs in the API tier beside the other background work. It wakes, takes what
//! is due one row at a time, and for each creates a session, stores the
//! prompt, and queues a turn -- then writes when the schedule should fire
//! again.

use chrono::Utc;
use chrono_tz::Tz;
use uuid::Uuid;

use super::{Cron, TICK, advance, postgres};

/// Runs until the process is asked to stop.
///
/// Stopping matters more here than it looks. `take_due` clears `next_run_at`
/// as it takes a row, so a firing interrupted between that and writing its
/// successor leaves a schedule with nowhere to go next -- and a rolling
/// redeploy is an ordinary event rather than a rare one. The signal is checked
/// between firings, never inside one, so a firing that has started completes.
pub async fn run(pool: sqlx::PgPool, shutdown: std::sync::Arc<tokio::sync::Notify>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = shutdown.notified() => {
                tracing::info!("schedules: stopping, no new firings will be taken");
                return;
            }
        }

        // Drained rather than one-per-tick: several schedules can come due in
        // the same minute, and making them wait thirty seconds each would turn
        // a 9am report into a 9:02 one for no reason. Bounded so a backlog
        // cannot hold the loop for ever.
        //
        // Not interruptible partway. A firing that has taken its row has
        // already cleared `next_run_at`, so abandoning it there is the one
        // outcome worth avoiding -- the drain is at most 64 firings and each
        // is two short statements.
        for _ in 0..64 {
            match postgres::take_due(&pool, Utc::now()).await {
                Ok(Some((schedule, owed))) => fire(&pool, schedule, owed).await,
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %e, "could not read due schedules");
                    break;
                }
            }
        }
    }
}

/// One firing: a session, a message nobody sent, and a queued turn.
async fn fire(pool: &sqlx::PgPool, schedule: super::Schedule, owed: chrono::DateTime<Utc>) {
    let now = Utc::now();

    // Reparsed at firing time rather than trusted from when it was saved. The
    // row is the only durable thing, and a schedule written by an older
    // version -- or by hand -- may not parse at all. Recording that is better
    // than panicking in a background loop nobody is watching.
    let parsed = Cron::parse(&schedule.expression);
    let zone: Result<Tz, _> = schedule.timezone.parse();

    let (cron, tz) = match (parsed, zone) {
        (Ok(c), Ok(z)) => (c, z),
        (Err(e), _) => return stop(pool, &schedule, &format!("expression: {e}")).await,
        (_, Err(_)) => {
            return stop(
                pool,
                &schedule,
                &format!("unknown timezone {}", schedule.timezone),
            )
            .await;
        }
    };

    // Where it should go next, and how much was missed while nothing was
    // running. Computed before the turn is queued so a failure to queue still
    // leaves the schedule pointing forward rather than stalled.
    //
    // `owed` rather than `now`: the search starts from the firing this one is
    // for, so everything between that and now counts as missed. Passing `now`
    // for both -- which an earlier version did -- makes the first candidate
    // strictly later than `now` by construction, so nothing is ever counted
    // and every firing looks punctual.
    let (next, skipped) = advance(&cron, tz, owed, now);

    match start_turn(pool, &schedule).await {
        Ok(session_id) => {
            tracing::info!(
                schedule_id = %schedule.id,
                session_id = %session_id,
                skipped,
                "a schedule fired"
            );
            let status = if skipped > 0 { "skipped" } else { "ok" };
            record(pool, schedule.id, status, None, next, skipped).await;
        }
        Err(e) => {
            tracing::error!(schedule_id = %schedule.id, error = %e, "a schedule could not fire");
            record(pool, schedule.id, "failed", Some(&e), next, skipped).await;
        }
    }
}

/// A schedule that cannot be understood stops rather than retrying.
///
/// `next_run_at` stays null, so nothing tries again until somebody edits it --
/// which is right, because an expression that does not parse will not parse in
/// thirty seconds either, and a loop retrying it writes a log line every tick
/// for ever.
async fn stop(pool: &sqlx::PgPool, schedule: &super::Schedule, why: &str) {
    tracing::error!(schedule_id = %schedule.id, why, "a schedule was stopped as unusable");
    record(pool, schedule.id, "failed", Some(why), None, 0).await;
}

async fn record(
    pool: &sqlx::PgPool,
    id: Uuid,
    status: &str,
    error: Option<&str>,
    next: Option<chrono::DateTime<Utc>>,
    skipped: i32,
) {
    if let Err(e) = postgres::record_run(pool, id, status, error, next, skipped).await {
        // The turn is already queued at this point, so the work happens
        // whatever this says. What is lost is the schedule's own account of
        // itself -- and, if `next` was not written, its next firing. Loud
        // because a schedule that silently stops is the failure this design
        // is most worried about.
        tracing::error!(schedule_id = %id, error = %e, "could not record a firing");
    }
}

/// One firing's turn, through the shared trigger path.
async fn start_turn(pool: &sqlx::PgPool, schedule: &super::Schedule) -> Result<Uuid, String> {
    crate::api::trigger::start(
        pool,
        crate::api::trigger::Started {
            workspace_id: schedule.workspace_id,
            agent_id: schedule.agent_id,
            title: schedule.name.clone(),
            prompt: schedule.prompt.clone(),
            account: schedule.account.clone(),
            // The schedule's zone, so the agent's clock reads the way whoever
            // set it up expects rather than the way the pod's does.
            timezone: Some(schedule.timezone.clone()),
            source: crate::api::trigger::Source::Schedule(schedule.id),
            metadata: serde_json::json!({
                "schedule_id": schedule.id,
                "schedule_name": schedule.name,
            }),
        },
    )
    .await
}
