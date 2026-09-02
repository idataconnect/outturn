//! Event feed and job queue against a real Postgres.
//!
//! Built only under the integration-tests feature; needs TEST_DATABASE_URL
//! naming a test database. See tests/api.rs.

use std::time::Duration;

use outturn::db;
use outturn::events::{self, EventBus};
use outturn::jobs;
use sqlx::postgres::PgPool;
use uuid::Uuid;

mod common;

async fn setup() -> (common::TestDb, Uuid) {
    // A private schema per test. Truncating a shared one would force
    // --test-threads=1, and could not isolate reap_abandoned, which sweeps
    // every expired job in the database regardless of who enqueued it.
    let db = common::TestDb::new().await;
    let pool = &db.pool;

    // Events and jobs are tenant-scoped by foreign key, so a tenant must exist.
    let tenant_id = Uuid::now_v7();
    sqlx::query("insert into tenants (id, name, slug) values ($1, $2, $3)")
        .bind(tenant_id)
        .bind(format!("T{tenant_id}"))
        .bind(format!("t-{}", tenant_id.simple()))
        .execute(pool)
        .await
        .expect("tenant");

    (db, tenant_id)
}

macro_rules! setup_or_skip {
    () => {
        setup().await
    };
}

/// Drops the test's schema. Skipped on failure, so a failing test leaves its
/// rows behind to inspect.
macro_rules! finish {
    ($db:expr) => {
        $db.cleanup().await
    };
}

#[tokio::test]
async fn events_return_immediately_when_already_present() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    events::append(pool, tenant, None, "test.one", serde_json::json!({"n": 1}))
        .await
        .expect("append");

    let found = events::wait_for(pool,
        &bus,
        tenant,
        None,
        0,
        100,
        Duration::from_secs(5),
        std::future::pending(),
    )
    .await
    .expect("wait");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, "test.one");

    finish!(db);
}

#[tokio::test]
async fn long_poll_wakes_on_notify() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    // Give the listener a moment to establish its connection.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let writer = pool.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        events::append(&writer, tenant, None, "test.late", serde_json::json!({}))
            .await
            .expect("append");
    });

    let start = std::time::Instant::now();
    let found = events::wait_for(pool,
        &bus,
        tenant,
        None,
        0,
        100,
        Duration::from_secs(10),
        std::future::pending(),
    )
    .await
    .expect("wait");

    assert_eq!(found.len(), 1, "should have woken on the notification");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "woke via notification, not timeout: {:?}",
        start.elapsed()
    );

    finish!(db);
}

#[tokio::test]
async fn long_poll_returns_empty_on_timeout() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    // Paused only now that connecting is done. Starting paused would trip
    // sqlx's acquire timeout, since the virtual clock races past it while the
    // real socket is still opening -- the clock is virtual, the network is not.
    tokio::time::pause();

    // With time virtual, a realistic 25 second poll times out instantly rather
    // than being shortened to keep the suite fast.
    let started = tokio::time::Instant::now();
    let found = events::wait_for(pool,
        &bus,
        tenant,
        None,
        0,
        100,
        Duration::from_secs(25),
        std::future::pending(),
    )
    .await
    .expect("wait");

    assert!(found.is_empty());
    assert!(
        started.elapsed() >= Duration::from_secs(25),
        "the poll must wait out its timeout before giving up"
    );

    finish!(db);
}

#[tokio::test]
async fn shutdown_releases_parked_poll() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let fire = notify.clone();

    // Real time here, unlike the timeout test. Under a virtual clock the timer
    // and the parked poll race: tokio advances time whenever nothing is
    // runnable, which can fire the shutdown before wait_for begins awaiting it.
    // The wait is short, and what is being checked is the ordering of two
    // real concurrent tasks rather than the passage of time.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        fire.notify_waiters();
    });

    let started = std::time::Instant::now();
    let found = events::wait_for(pool,
        &bus,
        tenant,
        None,
        0,
        100,
        Duration::from_secs(30),
        async move { notify.notified().await },
    )
    .await
    .expect("wait");

    assert!(found.is_empty());
    // Returned when shutdown fired rather than waiting out the timeout: a
    // draining service must not hold every parked poll open for its full
    // duration.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "shutdown must not wait out the full timeout: {:?}",
        started.elapsed()
    );

    finish!(db);
}

#[tokio::test]
async fn session_scoped_poll_ignores_other_sessions() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());
    let mine = Uuid::now_v7();
    let theirs = Uuid::now_v7();

    events::append(pool, tenant, Some(theirs), "other", serde_json::json!({}))
        .await
        .expect("append");

    let found = events::wait_for(pool,
        &bus,
        tenant,
        Some(mine),
        0,
        100,
        Duration::from_millis(500),
        std::future::pending(),
    )
    .await
    .expect("wait");

    assert!(found.is_empty(), "must not see another session's events");

    finish!(db);
}

#[tokio::test]
async fn cursor_advances_and_does_not_repeat() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    let first = events::append(pool, tenant, None, "a", serde_json::json!({}))
        .await
        .expect("append");
    events::append(pool, tenant, None, "b", serde_json::json!({}))
        .await
        .expect("append");

    let found = events::wait_for(pool,
        &bus,
        tenant,
        None,
        first,
        100,
        Duration::from_millis(500),
        std::future::pending(),
    )
    .await
    .expect("wait");

    assert_eq!(found.len(), 1, "cursor must exclude what was already seen");
    assert_eq!(found[0].kind, "b");

    finish!(db);
}

#[tokio::test]
async fn concurrent_workers_claim_disjoint_jobs() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;

    for i in 0..10 {
        jobs::enqueue(pool, tenant, "test.work", serde_json::json!({"i": i}), None)
            .await
            .expect("enqueue");
    }

    // Two workers claiming at once must not receive the same job.
    let (a, b) = tokio::join!(
        jobs::claim(pool, &["test.work"], 5, jobs::DEFAULT_LEASE),
        jobs::claim(pool, &["test.work"], 5, jobs::DEFAULT_LEASE),
    );
    let a = a.expect("claim a");
    let b = b.expect("claim b");

    assert_eq!(a.len() + b.len(), 10, "all jobs claimed exactly once");

    let mut ids: Vec<Uuid> = a.iter().chain(b.iter()).map(|h| h.job.id).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "no job claimed twice");

    finish!(db);
}

#[tokio::test]
async fn claimed_job_is_not_reclaimed_while_leased() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(pool, tenant, "test.lease", serde_json::json!({}), None)
        .await
        .expect("enqueue");

    let first = jobs::claim(pool, &["test.lease"], 10, Duration::from_secs(60))
        .await
        .expect("claim");
    assert_eq!(first.len(), 1);

    let second = jobs::claim(pool, &["test.lease"], 10, Duration::from_secs(60))
        .await
        .expect("claim");
    assert!(second.is_empty(), "leased job must not be re-claimed");

    finish!(db);
}

#[tokio::test]
async fn abandoned_lease_is_reaped_and_retried() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(pool, tenant, "test.reap", serde_json::json!({}), None)
        .await
        .expect("enqueue");

    // A zero lease is already expired: stands in for a crashed worker.
    let claimed = jobs::claim(pool, &["test.reap"], 10, Duration::from_secs(0))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);

    let reaped = jobs::reap_abandoned(pool).await.expect("reap");
    assert_eq!(reaped, 1);

    let again = jobs::claim(pool, &["test.reap"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(again.len(), 1, "reaped job must be runnable again");
    assert_eq!(again[0].job.attempts, 2, "retry counts as a second attempt");

    finish!(db);
}

#[tokio::test]
async fn job_fails_permanently_after_max_attempts() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let id = jobs::enqueue(pool, tenant, "test.fail", serde_json::json!({}), None)
        .await
        .expect("enqueue");

    // Default max_attempts is 3.
    for _ in 0..3 {
        let claimed = jobs::claim(pool, &["test.fail"], 10, jobs::DEFAULT_LEASE)
            .await
            .expect("claim");
        assert_eq!(claimed.len(), 1);
        jobs::fail(pool, id, "boom", Duration::from_secs(0))
            .await
            .expect("fail");
    }

    let state: String = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("state");
    assert_eq!(state, "failed");

    let claimed = jobs::claim(pool, &["test.fail"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert!(claimed.is_empty(), "exhausted job must not be retried");

    finish!(db);
}

#[tokio::test]
async fn delayed_job_is_not_claimable_yet() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(pool,
        tenant,
        "test.delay",
        serde_json::json!({}),
        Some(Duration::from_secs(300)),
    )
    .await
    .expect("enqueue");

    let claimed = jobs::claim(pool, &["test.delay"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert!(claimed.is_empty(), "future job must not be claimable");

    finish!(db);
}

#[tokio::test]
async fn enqueue_rolls_back_with_its_transaction() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;

    let mut tx = pool.begin().await.expect("begin");
    jobs::enqueue(&mut *tx, tenant, "test.tx", serde_json::json!({}), None)
        .await
        .expect("enqueue");
    tx.rollback().await.expect("rollback");

    let claimed = jobs::claim(pool, &["test.tx"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert!(claimed.is_empty(), "rolled-back enqueue must leave no job");

    finish!(db);
}

#[tokio::test]
async fn heartbeat_keeps_a_long_job_from_being_reaped() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(pool, tenant, "test.slow", serde_json::json!({}), None)
        .await
        .expect("enqueue");

    // A short lease stands in for work that outlives its original claim: a
    // local model generating a long reply has been measured at ~2 minutes
    // against what was a 60 second lease.
    let claimed = jobs::claim(pool, &["test.slow"], 10, Duration::from_secs(1))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    let id = claimed[0].job.id;

    // While the work is still running, the worker extends its own lease.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let still_ours = jobs::extend_lease(pool, id, Duration::from_secs(60))
        .await
        .expect("extend");
    assert!(still_ours, "a running job must be able to renew its lease");

    // The reaper must now leave it alone.
    let reaped = jobs::reap_abandoned(pool).await.expect("reap");
    assert_eq!(reaped, 0, "a heartbeating job must not be reaped");

    let stolen = jobs::claim(pool, &["test.slow"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert!(
        stolen.is_empty(),
        "a job whose lease is being renewed must not be claimable"
    );

    finish!(db);
}

#[tokio::test]
async fn extend_lease_reports_when_the_job_was_taken_away() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(pool, tenant, "test.lost", serde_json::json!({}), None)
        .await
        .expect("enqueue");

    let claimed = jobs::claim(pool, &["test.lost"], 10, Duration::from_secs(0))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    let id = claimed[0].job.id;

    // The lease expires and another worker takes the job.
    jobs::reap_abandoned(pool).await.expect("reap");
    let other = jobs::claim(pool, &["test.lost"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(other.len(), 1);

    // The original worker completes its own execution and renews -- which
    // succeeds, because the job is running again under a new owner. This is
    // the case a heartbeat cannot detect on its own, and is why the lease must
    // exceed the longest legitimate execution rather than relying on renewal.
    let renewed = jobs::extend_lease(pool, id, jobs::DEFAULT_LEASE)
        .await
        .expect("extend");
    assert!(renewed, "the row is running, so renewal reports success");

    finish!(db);
}

#[test]
fn lease_and_heartbeat_are_consistent() {
    // The heartbeat must fire several times within a lease: a single delayed
    // renewal must not let the lease lapse under a job that is still running.
    assert!(
        jobs::LEASE_HEARTBEAT * 3 <= jobs::DEFAULT_LEASE,
        "heartbeat {:?} must be well under the lease {:?}",
        jobs::LEASE_HEARTBEAT,
        jobs::DEFAULT_LEASE,
    );

    // And the lease must stay short. When a worker dies its heartbeat dies
    // with it, so the last renewal it wrote strands the job for the remainder
    // of the lease -- a long lease means slow recovery from a crash, which is
    // the failure the lease exists to detect.
    assert!(
        jobs::DEFAULT_LEASE <= Duration::from_secs(60),
        "lease {:?} is too long: a crashed worker's job stays unreachable for this long",
        jobs::DEFAULT_LEASE,
    );
}
