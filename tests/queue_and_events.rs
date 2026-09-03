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
        Uuid::nil(),
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
        Uuid::nil(),
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
        Uuid::nil(),
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
        Uuid::nil(),
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
        Uuid::nil(),
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

// -- Transcript / stream boundary ---------------------------------------------

/// Builds a session with one finished reply that was streamed in, so its
/// deltas are still in the event log behind it.
async fn streamed_session(
    pool: &PgPool,
    tenant: Uuid,
) -> (Uuid, std::sync::Arc<dyn outturn::api::chat::ChatStore>) {
    let user_id = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, $2)")
        .bind(user_id)
        .bind("Test")
        .execute(pool)
        .await
        .expect("user");

    let agent_id = Uuid::now_v7();
    sqlx::query("insert into agents (id, tenant_id, name, slug) values ($1, $2, $3, $4)")
        .bind(agent_id)
        .bind(tenant)
        .bind("A")
        .bind(format!("a-{}", agent_id.simple()))
        .execute(pool)
        .await
        .expect("agent");

    let store: std::sync::Arc<dyn outturn::api::chat::ChatStore> =
        std::sync::Arc::new(outturn::api::chat::PostgresChatStore::new(pool.clone()));
    let session = store
        .create_session(
            tenant,
            user_id,
            outturn::api::chat::CreateSession {
                agent_id,
                title: String::new(),
            },
        )
        .await
        .expect("session");

    (session.id, store)
}

/// A reply that was streamed and then completed must read back exactly once.
///
/// The deltas stay in the event log after the turn ends. A client that loaded
/// the transcript and then replayed those deltas would append a message's text
/// to itself -- which is what happened, visibly, in the browser. The cursor
/// returned with the transcript is what forecloses it.
#[tokio::test]
async fn transcript_cursor_excludes_deltas_already_in_content() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, tenant).await;

    let reply = store
        .append_message(session_id, "assistant", "", None, Default::default())
        .await
        .expect("reply");

    let fragments = ["Small ", "task ", "indeed", "."];
    for (idx, text) in fragments.iter().enumerate() {
        events::append(
            pool,
            tenant,
            Some(session_id),
            "chat.delta",
            serde_json::json!({ "message_id": reply.id, "idx": idx, "text": text }),
        )
        .await
        .expect("delta");
    }
    let full = fragments.concat();
    store
        .set_message_content(reply.id, &full, None, serde_json::json!({}))
        .await
        .expect("finalise");

    let history = store.messages(session_id).await.expect("history");
    assert_eq!(history.messages.len(), 1);
    assert_eq!(history.messages[0].content, full);
    assert_eq!(
        history.messages[0].delta_next,
        fragments.len() as i32,
        "content already accounts for every delta"
    );

    // The client polls from the cursor the transcript was read at. Nothing
    // behind it may come back, or the content would be appended to itself.
    let replayed = events::since(pool, tenant, Some(session_id), history.cursor, 100)
        .await
        .expect("since");
    assert!(
        replayed.is_empty(),
        "cursor must exclude the deltas already folded into content, got {} event(s)",
        replayed.len()
    );

    finish!(db);
}

/// Reconnecting mid-turn resumes the stream rather than restarting it.
#[tokio::test]
async fn transcript_mid_stream_returns_partial_content_and_resumes() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, tenant).await;

    // Created empty and streamed into; content is not stored until the turn
    // ends, so the transcript must assemble it from the deltas so far.
    let reply = store
        .append_message(session_id, "assistant", "", None, Default::default())
        .await
        .expect("reply");

    for (idx, text) in ["Half ", "a "].iter().enumerate() {
        events::append(
            pool,
            tenant,
            Some(session_id),
            "chat.delta",
            serde_json::json!({ "message_id": reply.id, "idx": idx, "text": text }),
        )
        .await
        .expect("delta");
    }

    let history = store.messages(session_id).await.expect("history");
    assert_eq!(history.messages[0].content, "Half a ");
    assert_eq!(history.messages[0].delta_next, 2, "next delta is idx 2");

    // The rest of the turn arrives over the feed, and only the rest.
    events::append(
        pool,
        tenant,
        Some(session_id),
        "chat.delta",
        serde_json::json!({ "message_id": reply.id, "idx": 2, "text": "thought." }),
    )
    .await
    .expect("delta");

    let arrived = events::since(pool, tenant, Some(session_id), history.cursor, 100)
        .await
        .expect("since");
    assert_eq!(arrived.len(), 1, "only what the content does not already cover");
    assert_eq!(arrived[0].payload["idx"], 2);

    let resumed = format!(
        "{}{}",
        history.messages[0].content,
        arrived[0].payload["text"].as_str().unwrap()
    );
    assert_eq!(resumed, "Half a thought.");

    finish!(db);
}

/// Messages read back in the order they were appended, ordered by their
/// UUIDv7 keys rather than a separate sequence column.
#[tokio::test]
async fn transcript_orders_by_uuidv7_key() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, tenant).await;

    for n in 0..5 {
        store
            .append_message(session_id, "user", &format!("m{n}"), None, Default::default())
            .await
            .expect("append");
    }

    let history = store.messages(session_id).await.expect("history");
    let contents: Vec<&str> = history.messages.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, ["m0", "m1", "m2", "m3", "m4"]);

    let ids: Vec<Uuid> = history.messages.iter().map(|m| m.id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "key order is transcript order");

    finish!(db);
}

/// A retried turn fills the reply it already created rather than orphaning it.
///
/// This is what a worker killed mid-generation does: the placeholder is
/// already in the transcript, the lease expires, and another worker claims the
/// same job. Creating a second reply would leave the first empty forever,
/// where it is replayed to the model on every later turn -- which is exactly
/// what a redeploy during a turn produced.
#[tokio::test]
async fn a_retried_turn_reuses_its_reply_rather_than_orphaning_it() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, tenant).await;

    let prompt = store
        .append_message(session_id, "user", "hello", None, Default::default())
        .await
        .expect("prompt");

    let first = store
        .claim_placeholder(prompt.id, session_id)
        .await
        .expect("first attempt");
    // The worker dies here: no cleanup runs, the lease expires, another
    // worker claims the same job.
    let second = store
        .claim_placeholder(prompt.id, session_id)
        .await
        .expect("retry");

    assert_eq!(first.id, second.id, "a retry must take back the same reply");

    let history = store.messages(session_id).await.expect("history");
    assert_eq!(
        history.messages.len(),
        2,
        "one prompt and one reply, not two replies: {:?}",
        history
            .messages
            .iter()
            .map(|m| (&m.role, m.content.len()))
            .collect::<Vec<_>>()
    );

    finish!(db);
}

/// Two turns at once in one session get their own replies.
///
/// Ownership hangs off the prompt rather than the session, so a second turn
/// starting while the first is still generating cannot claim the first's
/// reply -- which a "reuse the newest empty message" rule would have done.
#[tokio::test]
async fn concurrent_turns_do_not_claim_each_others_reply() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, tenant).await;

    let first_prompt = store
        .append_message(session_id, "user", "one", None, Default::default())
        .await
        .expect("first prompt");
    // A real turn enqueues its job alongside the message, which is what marks
    // the reply as one somebody is still filling.
    jobs::enqueue(
        pool,
        tenant,
        "chat.turn",
        serde_json::json!({ "message_id": first_prompt.id }),
        None,
    )
    .await
    .expect("first job");
    let first_reply = store
        .claim_placeholder(first_prompt.id, session_id)
        .await
        .expect("first reply");

    // The second message arrives while the first turn is still generating.
    let second_prompt = store
        .append_message(session_id, "user", "two", None, Default::default())
        .await
        .expect("second prompt");
    let second_reply = store
        .claim_placeholder(second_prompt.id, session_id)
        .await
        .expect("second reply");

    assert_ne!(
        first_reply.id, second_reply.id,
        "each turn must fill its own reply"
    );

    finish!(db);
}

/// An empty reply nobody is filling is refused rather than buried.
///
/// Writing past one would leave it in the transcript, where every later turn
/// replays it to the model as an empty assistant message.
#[tokio::test]
async fn an_abandoned_reply_refuses_further_messages() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, tenant).await;

    let prompt = store
        .append_message(session_id, "user", "hello", None, Default::default())
        .await
        .expect("prompt");
    // No job was ever enqueued for this prompt, so nothing is filling the
    // reply -- the state a worker that died without retrying leaves behind.
    store
        .claim_placeholder(prompt.id, session_id)
        .await
        .expect("reply");

    let refused = store
        .append_message(session_id, "user", "anyone there?", None, Default::default())
        .await;

    assert!(
        matches!(refused, Err(outturn::api::chat::ChatError::Abandoned(_))),
        "expected the abandoned reply to be refused, got {refused:?}"
    );

    // Discarding it unwedges the session, which is what a permanently failed
    // turn does.
    store
        .discard_placeholder(prompt.id)
        .await
        .expect("discard");
    store
        .append_message(session_id, "user", "anyone there?", None, Default::default())
        .await
        .expect("the session is writable again");

    finish!(db);
}

// -- Provider circuit breaker --------------------------------------------------

use outturn::gateway::breaker::{self, Verdict};

/// The circuit opens only after repeated failures, not on the first one.
#[tokio::test]
async fn the_circuit_opens_after_repeated_failures() {
    let (db, _tenant) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    // A single failure is often a blip or a bad request, so it must not stop
    // every replica from calling the provider.
    breaker::record_failure(pool, &endpoint, "boom").await;
    assert_eq!(breaker::check(pool, &endpoint).await, Verdict::Allow);

    for _ in 0..4 {
        breaker::record_failure(pool, &endpoint, "boom").await;
    }
    assert_eq!(
        breaker::check(pool, &endpoint).await,
        Verdict::Reject,
        "five consecutive failures should open the circuit"
    );

    finish!(db);
}

/// Exactly one replica probes a recovering provider.
///
/// This is the whole point of putting the breaker in the database: without a
/// shared claim, every pod would probe at once and the outage would be met
/// with a storm rather than a single request.
#[tokio::test]
async fn only_one_replica_claims_the_probe() {
    let (db, _tenant) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    for _ in 0..5 {
        breaker::record_failure(pool, &endpoint, "boom").await;
    }
    // Bring the probe forward rather than waiting out the backoff.
    sqlx::query("update provider_health set probe_after = now() - interval '1 second' where endpoint = $1")
        .bind(&endpoint)
        .execute(pool)
        .await
        .expect("age the circuit");

    // Ten replicas reach the breaker at once.
    let mut checks = Vec::new();
    for _ in 0..10 {
        checks.push(breaker::check(pool, &endpoint));
    }
    let verdicts = futures::future::join_all(checks).await;
    let allowed = verdicts.iter().filter(|v| **v == Verdict::Allow).count();

    assert_eq!(allowed, 1, "exactly one replica may probe, got {allowed}");

    finish!(db);
}

/// A success closes the circuit and clears the history behind it.
#[tokio::test]
async fn a_success_closes_the_circuit() {
    let (db, _tenant) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    for _ in 0..5 {
        breaker::record_failure(pool, &endpoint, "boom").await;
    }
    assert_eq!(breaker::check(pool, &endpoint).await, Verdict::Reject);

    breaker::record_success(pool, &endpoint).await;
    assert_eq!(breaker::check(pool, &endpoint).await, Verdict::Allow);

    // The count resets too, so an old outage does not shorten the fuse on the
    // next unrelated one.
    for _ in 0..4 {
        breaker::record_failure(pool, &endpoint, "boom").await;
    }
    assert_eq!(
        breaker::check(pool, &endpoint).await,
        Verdict::Allow,
        "four failures after a success must not reopen the circuit"
    );

    finish!(db);
}

/// Being told off is not the same as being down.
#[tokio::test]
async fn client_errors_and_rate_limits_do_not_count_against_a_provider() {
    use outturn::gateway::llm::provider::ProviderError;

    assert!(!breaker::counts_as_failure(&ProviderError::RateLimited));
    assert!(!breaker::counts_as_failure(&ProviderError::Upstream(
        "400: model does not support tools".into()
    )));
    assert!(!breaker::counts_as_failure(&ProviderError::Upstream(
        "404: no such model".into()
    )));

    assert!(breaker::counts_as_failure(&ProviderError::Unavailable));
    assert!(breaker::counts_as_failure(&ProviderError::Upstream(
        "503: upstream connect error".into()
    )));
    assert!(breaker::counts_as_failure(&ProviderError::Upstream(
        "error sending request for url".into()
    )));
    // A timeout is the provider failing to answer, not refusing.
    assert!(breaker::counts_as_failure(&ProviderError::Upstream(
        "408: request timeout".into()
    )));
}

// -- Traffic routing -----------------------------------------------------------

use outturn::gateway::routing;

async fn add_route(
    pool: &PgPool,
    tenant: Option<Uuid>,
    traffic: &str,
    priority: i32,
    base_url: &str,
    model: &str,
) {
    sqlx::query(
        "insert into traffic_routes \
             (id, tenant_id, traffic_type, priority, provider, base_url, model) \
         values (uuidv7(), $1, $2, $3, 'openai', $4, $5)",
    )
    .bind(tenant)
    .bind(traffic)
    .bind(priority)
    .bind(base_url)
    .bind(model)
    .execute(pool)
    .await
    .expect("route");
}

/// Routes come back in precedence order, not insertion order.
#[tokio::test]
async fn routes_are_ordered_by_precedence() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 30, "http://third", "c").await;
    add_route(pool, None, "assistant", 10, "http://first", "a").await;
    add_route(pool, None, "assistant", 20, "http://second", "b").await;

    let routes = routing::routes_for(pool, tenant, "assistant")
        .await
        .expect("routes");
    let models: Vec<&str> = routes.iter().map(|r| r.model.as_str()).collect();
    assert_eq!(models, ["a", "b", "c"]);

    finish!(db);
}

/// A tenant's own routes replace the system defaults rather than extending
/// them, so its traffic cannot quietly fall through to somebody else's
/// endpoint once it has said where it wants to go.
#[tokio::test]
async fn tenant_routes_replace_the_system_defaults() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 10, "http://shared", "default").await;
    let inherited = routing::routes_for(pool, tenant, "assistant")
        .await
        .expect("routes");
    assert_eq!(inherited.len(), 1, "a tenant with no routes uses the defaults");
    assert_eq!(inherited[0].model, "default");

    add_route(pool, Some(tenant), "assistant", 10, "http://theirs", "theirs").await;
    let own = routing::routes_for(pool, tenant, "assistant")
        .await
        .expect("routes");
    assert_eq!(own.len(), 1, "configured routes replace, not extend");
    assert_eq!(own[0].model, "theirs");

    finish!(db);
}

/// Traffic types are independent: one class of work being configured says
/// nothing about another.
#[tokio::test]
async fn traffic_types_route_separately() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 10, "http://good", "expensive").await;
    add_route(pool, None, "title", 10, "http://cheap", "small").await;

    let assistant = routing::routes_for(pool, tenant, "assistant").await.expect("a");
    let title = routing::routes_for(pool, tenant, "title").await.expect("t");
    let unknown = routing::routes_for(pool, tenant, "nothing-here").await.expect("u");

    assert_eq!(assistant[0].model, "expensive");
    assert_eq!(title[0].model, "small");
    assert!(unknown.is_empty(), "an unrouted type falls back to static providers");

    finish!(db);
}

/// The breaker and the route list meet: a destination whose circuit is open is
/// skipped, and the next in precedence order serves the request.
#[tokio::test]
async fn an_open_circuit_removes_a_destination_from_the_list() {
    let (db, tenant) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 10, "http://primary", "a").await;
    add_route(pool, None, "assistant", 20, "http://fallback", "b").await;

    let routes = routing::routes_for(pool, tenant, "assistant").await.expect("routes");
    for _ in 0..5 {
        breaker::record_failure(pool, &routes[0].endpoint(), "down").await;
    }

    let mut usable = Vec::new();
    for route in &routes {
        if breaker::check(pool, &route.endpoint()).await == Verdict::Allow {
            usable.push(route.model.as_str());
        }
    }

    assert_eq!(usable, ["b"], "the failed destination drops out of the list");

    finish!(db);
}
