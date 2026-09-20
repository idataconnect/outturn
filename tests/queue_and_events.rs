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

    // Events and jobs are workspace-scoped by foreign key, so a workspace must exist.
    let workspace_id = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(workspace_id)
        .bind(format!("T{workspace_id}"))
        .bind(format!("t-{}", workspace_id.simple()))
        .execute(pool)
        .await
        .expect("workspace");

    (db, workspace_id)
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    events::append(
        pool,
        workspace,
        None,
        "test.one",
        serde_json::json!({"n": 1}),
    )
    .await
    .expect("append");

    let found = events::wait_for(
        pool,
        &bus,
        workspace,
        None,
        Uuid::nil(),
        100,
        Duration::from_secs(5),
        std::future::pending(),
        None,
    )
    .await
    .expect("wait");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, "test.one");

    finish!(db);
}

#[tokio::test]
async fn long_poll_wakes_on_notify() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    // Give the listener a moment to establish its connection.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let writer = pool.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        events::append(&writer, workspace, None, "test.late", serde_json::json!({}))
            .await
            .expect("append");
    });

    let start = std::time::Instant::now();
    let found = events::wait_for(
        pool,
        &bus,
        workspace,
        None,
        Uuid::nil(),
        100,
        Duration::from_secs(10),
        std::future::pending(),
        None,
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    // Paused only now that connecting is done. Starting paused would trip
    // sqlx's acquire timeout, since the virtual clock races past it while the
    // real socket is still opening -- the clock is virtual, the network is not.
    tokio::time::pause();

    // With time virtual, a realistic 25 second poll times out instantly rather
    // than being shortened to keep the suite fast.
    let started = tokio::time::Instant::now();
    let found = events::wait_for(
        pool,
        &bus,
        workspace,
        None,
        Uuid::nil(),
        100,
        Duration::from_secs(25),
        std::future::pending(),
        None,
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
    let (db, workspace) = setup_or_skip!();
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
    let found = events::wait_for(
        pool,
        &bus,
        workspace,
        None,
        Uuid::nil(),
        100,
        Duration::from_secs(30),
        async move { notify.notified().await },
        None,
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());
    let mine = Uuid::now_v7();
    let theirs = Uuid::now_v7();

    events::append(
        pool,
        workspace,
        Some(theirs),
        "other",
        serde_json::json!({}),
    )
    .await
    .expect("append");

    let found = events::wait_for(
        pool,
        &bus,
        workspace,
        Some(mine),
        Uuid::nil(),
        100,
        Duration::from_millis(500),
        std::future::pending(),
        None,
    )
    .await
    .expect("wait");

    assert!(found.is_empty(), "must not see another session's events");

    finish!(db);
}

#[tokio::test]
async fn cursor_advances_and_does_not_repeat() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    let first = events::append(pool, workspace, None, "a", serde_json::json!({}))
        .await
        .expect("append");
    events::append(pool, workspace, None, "b", serde_json::json!({}))
        .await
        .expect("append");

    let found = events::wait_for(
        pool,
        &bus,
        workspace,
        None,
        first,
        100,
        Duration::from_millis(500),
        std::future::pending(),
        None,
    )
    .await
    .expect("wait");

    assert_eq!(found.len(), 1, "cursor must exclude what was already seen");
    assert_eq!(found[0].kind, "b");

    finish!(db);
}

#[tokio::test]
async fn concurrent_workers_claim_disjoint_jobs() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    for i in 0..10 {
        jobs::enqueue(
            pool,
            workspace,
            "test.work",
            serde_json::json!({"i": i}),
            None,
            None,
            jobs::PRIORITY_BACKGROUND,
        )
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.lease",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.reap",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    // A zero lease is already expired: stands in for a crashed worker.
    let claimed = jobs::claim(pool, &["test.reap"], 10, Duration::from_secs(0))
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);

    let (reaped, _) = jobs::reap_abandoned(pool).await.expect("reap");
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let id = jobs::enqueue(
        pool,
        workspace,
        "test.fail",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    // Default max_attempts is 3.
    for _ in 0..3 {
        let claimed = jobs::claim(pool, &["test.fail"], 10, jobs::DEFAULT_LEASE)
            .await
            .expect("claim");
        assert_eq!(claimed.len(), 1);
        jobs::fail(pool, id, "boom", Duration::from_secs(0), None)
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.delay",
        serde_json::json!({}),
        Some(Duration::from_secs(300)),
        None,
        jobs::PRIORITY_BACKGROUND,
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    let mut tx = pool.begin().await.expect("begin");
    jobs::enqueue(
        &mut *tx,
        workspace,
        "test.tx",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.slow",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
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
    let still_ours = jobs::extend_lease(
        pool,
        id,
        Duration::from_secs(60),
        claimed[0].job.lease_token.expect("a claim issues a token"),
    )
    .await
    .expect("extend");
    assert!(still_ours, "a running job must be able to renew its lease");

    // The reaper must now leave it alone.
    let (reaped, _) = jobs::reap_abandoned(pool).await.expect("reap");
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.lost",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
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

    // The original worker completes its own execution and renews. It is told
    // no, because the claim it holds was replaced -- which is the whole point
    // of the token. Renewing on state alone reported success here, extending
    // the new owner's lease while the old owner carried on believing the job
    // was still its own.
    let renewed = jobs::extend_lease(
        pool,
        id,
        jobs::DEFAULT_LEASE,
        claimed[0].job.lease_token.expect("a claim issues a token"),
    )
    .await
    .expect("extend");
    assert!(
        !renewed,
        "a renewal from a replaced claim reported success, so two workers \
         both believe they hold this job"
    );

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
    workspace: Uuid,
) -> (Uuid, std::sync::Arc<dyn outturn::api::chat::ChatStore>) {
    let user_id = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, $2)")
        .bind(user_id)
        .bind("Test")
        .execute(pool)
        .await
        .expect("user");

    let agent_id = Uuid::now_v7();
    sqlx::query("insert into agents (id, workspace_id, name, slug) values ($1, $2, $3, $4)")
        .bind(agent_id)
        .bind(workspace)
        .bind("A")
        .bind(format!("a-{}", agent_id.simple()))
        .execute(pool)
        .await
        .expect("agent");

    let store: std::sync::Arc<dyn outturn::api::chat::ChatStore> =
        std::sync::Arc::new(outturn::api::chat::PostgresChatStore::new(pool.clone()));
    let session = store
        .create_session(
            workspace,
            user_id,
            outturn::api::chat::CreateSession {
                agent_id,
                title: String::new(),
                account: None,
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    let reply = store
        .append_message(
            session_id,
            "assistant",
            "",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("reply");

    let fragments = ["Small ", "task ", "indeed", "."];
    for (idx, text) in fragments.iter().enumerate() {
        events::append(
            pool,
            workspace,
            Some(session_id),
            "chat.delta",
            serde_json::json!({ "message_id": reply.id, "idx": idx, "text": text }),
        )
        .await
        .expect("delta");
    }
    let full = fragments.concat();
    store
        .set_message_content(
            reply.id,
            &full,
            None,
            None,
            Default::default(),
            serde_json::json!({}),
        )
        .await
        .expect("finalise");

    let history = store.messages(session_id).await.expect("history");
    assert_eq!(history.messages.len(), 1);
    assert_eq!(history.messages[0].content, full);
    // A finished reply's deltas are not assembled -- the stored content is
    // the answer, and the deltas are pruned in time -- so nothing is
    // outstanding. What matters is below: none may be replayed either.
    assert_eq!(
        history.messages[0].delta_next, 0,
        "a finished reply has no deltas still to come"
    );

    // The client polls from the cursor the transcript was read at. Nothing
    // behind it may come back, or the content would be appended to itself.
    let replayed = events::since(pool, workspace, Some(session_id), history.cursor, 100, None)
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    // Created empty and streamed into; content is not stored until the turn
    // ends, so the transcript must assemble it from the deltas so far.
    let reply = store
        .append_message(
            session_id,
            "assistant",
            "",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("reply");

    for (idx, text) in ["Half ", "a "].iter().enumerate() {
        events::append(
            pool,
            workspace,
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
        workspace,
        Some(session_id),
        "chat.delta",
        serde_json::json!({ "message_id": reply.id, "idx": 2, "text": "thought." }),
    )
    .await
    .expect("delta");

    let arrived = events::since(pool, workspace, Some(session_id), history.cursor, 100, None)
        .await
        .expect("since");
    assert_eq!(
        arrived.len(),
        1,
        "only what the content does not already cover"
    );
    assert_eq!(arrived[0].payload["idx"], 2);

    let resumed = format!(
        "{}{}",
        history.messages[0].content,
        arrived[0].payload["text"].as_str().unwrap()
    );
    assert_eq!(resumed, "Half a thought.");

    finish!(db);
}

/// A page assembles a streaming reply's text just as the full transcript
/// does, even with older messages ahead of it that the page excludes.
///
/// The deltas are bounded by the window's id range for speed, and this is
/// what makes that sound: a delta is written after the message it belongs to
/// and both are UUIDv7, so a reply in the page can have no delta below the
/// page's floor. Get that wrong and a reconnecting reader loses the text of
/// the reply it is watching arrive.
#[tokio::test]
async fn paged_read_still_assembles_a_streaming_reply() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    // History the page will exclude, so the window has a real floor.
    for n in 0..6 {
        store
            .append_message(
                session_id,
                "user",
                &format!("old{n}"),
                None,
                Default::default(),
                Default::default(),
                None,
            )
            .await
            .expect("append");
    }

    let reply = store
        .append_message(
            session_id,
            "assistant",
            "",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("reply");

    for (idx, text) in ["Still ", "arriving."].iter().enumerate() {
        events::append(
            pool,
            workspace,
            Some(session_id),
            "chat.delta",
            serde_json::json!({ "message_id": reply.id, "idx": idx, "text": text }),
        )
        .await
        .expect("delta");
    }

    // Finish that reply and start a second one, so the first streaming reply
    // sits in the middle of the window rather than at its edge. A bound taken
    // from the wrong end would now clip its deltas instead of coincidentally
    // keeping them.
    store
        .set_message_content(
            reply.id,
            "Still arriving.",
            None,
            None,
            Default::default(),
            serde_json::json!({}),
        )
        .await
        .expect("finalise");

    let second = store
        .append_message(
            session_id,
            "assistant",
            "",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("second reply");
    events::append(
        pool,
        workspace,
        Some(session_id),
        "chat.delta",
        serde_json::json!({ "message_id": second.id, "idx": 0, "text": "And more." }),
    )
    .await
    .expect("delta");

    // A page small enough to leave the older messages behind.
    let page = store
        .messages_page(session_id, None, 3)
        .await
        .expect("page");
    assert!(page.has_more, "the older messages are behind this page");

    let streaming = page
        .messages
        .iter()
        .find(|m| m.id == second.id)
        .expect("the streaming reply is in the newest page");
    assert_eq!(
        streaming.content, "And more.",
        "a page must assemble the reply's deltas, not drop them"
    );
    assert_eq!(streaming.delta_next, 1, "and account for what it folded in");
}

/// A page holds the newest messages, and says whether anything is behind it.
#[tokio::test]
async fn transcript_page_takes_the_newest_and_reports_more() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    for n in 0..10 {
        store
            .append_message(
                session_id,
                "user",
                &format!("m{n}"),
                None,
                Default::default(),
                Default::default(),
                None,
            )
            .await
            .expect("append");
    }

    let page = store
        .messages_page(session_id, None, 4)
        .await
        .expect("page");
    let contents: Vec<&str> = page.messages.iter().map(|m| m.content.as_str()).collect();
    // Newest four, still oldest-first within the page: a reader appends this
    // to the bottom of the thread, not in reverse.
    assert_eq!(contents, ["m6", "m7", "m8", "m9"]);
    assert!(page.has_more, "six older messages were left behind");
}

/// Walking the cursor backwards reaches the start exactly once, with no row
/// repeated at a page boundary and none skipped.
#[tokio::test]
async fn transcript_pages_back_without_gaps_or_repeats() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    for n in 0..10 {
        store
            .append_message(
                session_id,
                "user",
                &format!("m{n}"),
                None,
                Default::default(),
                Default::default(),
                None,
            )
            .await
            .expect("append");
    }

    let mut seen: Vec<String> = Vec::new();
    let mut before = None;
    loop {
        let page = store
            .messages_page(session_id, before, 3)
            .await
            .expect("page");
        let mut batch: Vec<String> = page.messages.iter().map(|m| m.content.clone()).collect();
        batch.extend(seen);
        seen = batch;
        if !page.has_more {
            break;
        }
        before = page.messages.first().map(|m| m.id);
        assert!(
            before.is_some(),
            "has_more with an empty page would not terminate"
        );
    }

    assert_eq!(
        seen,
        ["m0", "m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9"],
        "the walk should rebuild the transcript exactly"
    );
}

/// The last page says so, rather than leaving a reader to guess from a short
/// one -- a page that happens to land on the boundary is full and final.
#[tokio::test]
async fn transcript_exact_page_knows_it_is_the_last() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    for n in 0..4 {
        store
            .append_message(
                session_id,
                "user",
                &format!("m{n}"),
                None,
                Default::default(),
                Default::default(),
                None,
            )
            .await
            .expect("append");
    }

    // Exactly as many messages as the limit: full, and yet nothing behind it.
    let page = store
        .messages_page(session_id, None, 4)
        .await
        .expect("page");
    assert_eq!(page.messages.len(), 4);
    assert!(
        !page.has_more,
        "a full page on the boundary is still the last"
    );
}

/// The whole-transcript read is what a turn is built from, and keeps its
/// meaning now that a paged read sits beside it.
#[tokio::test]
async fn full_transcript_is_unpaged() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    for n in 0..7 {
        store
            .append_message(
                session_id,
                "user",
                &format!("m{n}"),
                None,
                Default::default(),
                Default::default(),
                None,
            )
            .await
            .expect("append");
    }

    let all = store.messages(session_id).await.expect("history");
    assert_eq!(all.messages.len(), 7);
    assert!(
        !all.has_more,
        "nothing is held back from the whole transcript"
    );
}

/// Messages read back in the order they were appended, ordered by their
/// UUIDv7 keys rather than a separate sequence column.
#[tokio::test]
async fn transcript_orders_by_uuidv7_key() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    for n in 0..5 {
        store
            .append_message(
                session_id,
                "user",
                &format!("m{n}"),
                None,
                Default::default(),
                Default::default(),
                None,
            )
            .await
            .expect("append");
    }

    let history = store.messages(session_id).await.expect("history");
    let contents: Vec<&str> = history
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect();
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    let prompt = store
        .append_message(
            session_id,
            "user",
            "hello",
            None,
            Default::default(),
            Default::default(),
            None,
        )
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

    assert_eq!(
        first.message.id, second.message.id,
        "a retry must take back the same reply"
    );
    assert!(first.created, "the first claim made the reply");
    assert!(
        !second.created,
        "a retry re-announced a reply the browser already has"
    );

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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    let first_prompt = store
        .append_message(
            session_id,
            "user",
            "one",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("first prompt");
    // A real turn enqueues its job alongside the message, which is what marks
    // the reply as one somebody is still filling.
    jobs::enqueue(
        pool,
        workspace,
        "chat.turn",
        serde_json::json!({ "message_id": first_prompt.id }),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("first job");
    let first_reply = store
        .claim_placeholder(first_prompt.id, session_id)
        .await
        .expect("first reply");

    // The second message arrives while the first turn is still generating.
    let second_prompt = store
        .append_message(
            session_id,
            "user",
            "two",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("second prompt");
    let second_reply = store
        .claim_placeholder(second_prompt.id, session_id)
        .await
        .expect("second reply");

    assert_ne!(
        first_reply.message.id, second_reply.message.id,
        "each turn must fill its own reply"
    );

    finish!(db);
}

/// An empty reply nobody is filling is refused rather than buried.
///
/// Writing past one would leave it in the transcript, where every later turn
/// replays it to the model as an empty assistant message.

/// A reply that is only a tool call, whose turn finished, is not abandoned.
///
/// Empty content and a finished job is what a model that ran a tool and then
/// said nothing leaves behind. Treating that as a dead placeholder refused
/// every later message in the session -- one failed tool and the conversation
/// was over for good.
#[tokio::test]
async fn a_tool_only_reply_from_a_finished_turn_does_not_wedge_the_session() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    let prompt = store
        .append_message(
            session_id,
            "user",
            "list my files",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("prompt");

    // The turn ran to completion: its job succeeded.
    let job = jobs::enqueue(
        pool,
        workspace,
        "chat.turn",
        serde_json::json!({ "message_id": prompt.id }),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");
    let claimed = jobs::claim(pool, &["chat.turn"], 1, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    jobs::complete(pool, job, claimed[0].job.lease_token)
        .await
        .expect("complete");

    // And the reply it left is a tool call with no prose after it.
    let reply = store
        .claim_placeholder(prompt.id, session_id)
        .await
        .expect("reply");
    store
        .set_message_content(
            reply.message.id,
            "",
            None,
            None,
            Default::default(),
            serde_json::json!({ "tool_calls": [{ "id": "c1", "name": "list_objects", "is_error": true }] }),
        )
        .await
        .expect("finalise");

    store
        .append_message(
            session_id,
            "user",
            "hello?",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("a finished turn's tool-only reply must not block the session");

    finish!(db);
}

#[tokio::test]
async fn an_abandoned_reply_refuses_further_messages() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, store) = streamed_session(pool, workspace).await;

    let prompt = store
        .append_message(
            session_id,
            "user",
            "hello",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("prompt");
    // No job was ever enqueued for this prompt, so nothing is filling the
    // reply -- the state a worker that died without retrying leaves behind.
    store
        .claim_placeholder(prompt.id, session_id)
        .await
        .expect("reply");

    let refused = store
        .append_message(
            session_id,
            "user",
            "anyone there?",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await;

    assert!(
        matches!(refused, Err(outturn::api::chat::ChatError::Abandoned(_))),
        "expected the abandoned reply to be refused, got {refused:?}"
    );

    // Discarding it unwedges the session, which is what a permanently failed
    // turn does.
    store.discard_placeholder(prompt.id).await.expect("discard");
    store
        .append_message(
            session_id,
            "user",
            "anyone there?",
            None,
            Default::default(),
            Default::default(),
            None,
        )
        .await
        .expect("the session is writable again");

    finish!(db);
}

// -- Provider circuit breaker --------------------------------------------------

use outturn::gateway::breaker::policy::{Caller, Observation};
use outturn::gateway::breaker::{self, Verdict};

/// These circuits are the platform's own, which is what `None` means: one
/// circuit for everybody, because the credential and the rate limit behind it
/// are shared.
const PLATFORM: Option<Uuid> = None;

/// One caller, for tests about failures that need no breadth to count.
fn a_caller() -> Caller {
    Caller {
        workspace_id: Uuid::now_v7(),
        session_id: Uuid::now_v7(),
    }
}

/// A failure nothing but the endpoint explains -- a refused connection rather
/// than an answer. One caller reporting it is evidence on its own, which is
/// what these tests are about.
async fn unreachable(pool: &sqlx::PgPool, endpoint: &str) {
    breaker::observe(
        pool,
        endpoint,
        PLATFORM,
        Observation::Unreachable,
        a_caller(),
        Some("boom"),
    )
    .await;
}

async fn succeeded(pool: &sqlx::PgPool, endpoint: &str) {
    breaker::observe(
        pool,
        endpoint,
        PLATFORM,
        Observation::Success,
        a_caller(),
        None,
    )
    .await;
}

/// The circuit opens only after repeated failures, not on the first one.
#[tokio::test]
async fn the_circuit_opens_after_repeated_failures() {
    let (db, _workspace) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    // A single failure is often a blip or a bad request, so it must not stop
    // every replica from calling the provider.
    unreachable(pool, &endpoint).await;
    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
        Verdict::Allow
    );

    for _ in 0..4 {
        unreachable(pool, &endpoint).await;
    }
    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
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
    let (db, _workspace) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    for _ in 0..5 {
        unreachable(pool, &endpoint).await;
    }
    // Bring the probe forward rather than waiting out the backoff.
    sqlx::query(
        "update provider_health set probe_after = now() - interval '1 second' where endpoint = $1",
    )
    .bind(&endpoint)
    .execute(pool)
    .await
    .expect("age the circuit");

    // Ten replicas reach the breaker at once.
    let mut checks = Vec::new();
    for _ in 0..10 {
        checks.push(breaker::check(pool, &endpoint, PLATFORM));
    }
    let verdicts = futures::future::join_all(checks).await;
    let allowed = verdicts.iter().filter(|v| **v == Verdict::Allow).count();

    assert_eq!(allowed, 1, "exactly one replica may probe, got {allowed}");

    finish!(db);
}

/// A success closes the circuit and clears the history behind it.
#[tokio::test]
async fn a_success_closes_the_circuit() {
    let (db, _workspace) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    for _ in 0..5 {
        unreachable(pool, &endpoint).await;
    }
    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
        Verdict::Reject
    );

    succeeded(pool, &endpoint).await;
    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
        Verdict::Allow
    );

    // The count resets too, so an old outage does not shorten the fuse on the
    // next unrelated one.
    for _ in 0..4 {
        unreachable(pool, &endpoint).await;
    }
    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
        Verdict::Allow,
        "four failures after a success must not reopen the circuit"
    );

    finish!(db);
}

/// One caller's bad request cannot take a provider away from everybody.
///
/// The incident this whole classification exists for, driven through the real
/// table: an agent sends something that makes an upstream throw 500s, over and
/// over. Counting failures would have opened the circuit five requests in. What
/// must happen instead is nothing at all, however loud one caller is.
#[tokio::test]
async fn a_single_caller_answering_badly_does_not_open_a_circuit() {
    let (db, _workspace) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    let noisy = a_caller();
    for _ in 0..50 {
        breaker::observe(
            pool,
            &endpoint,
            PLATFORM,
            Observation::Undetermined,
            noisy,
            Some("500: upstream exploded"),
        )
        .await;
    }

    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
        Verdict::Allow,
        "fifty 500s from one caller is one caller's problem"
    );

    finish!(db);
}

/// The same answer from enough distinct callers is an outage.
///
/// Breadth is what turns an undetermined failure into evidence: once several
/// callers see it, the service is answering and failing for everybody, which is
/// when hammering it helps least.
#[tokio::test]
async fn enough_distinct_callers_seeing_it_opens_the_circuit() {
    let (db, _workspace) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());

    for _ in 0..3 {
        breaker::observe(
            pool,
            &endpoint,
            PLATFORM,
            Observation::Undetermined,
            a_caller(),
            Some("500: upstream exploded"),
        )
        .await;
    }

    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
        Verdict::Reject,
        "three workspaces seeing the same failure is not one caller's problem"
    );

    finish!(db);
}

/// A workspace's own circuit is not the platform's.
///
/// A workspace bringing its own credential does not share a fate with anyone
/// else reaching the same host: one expired key must not close the endpoint for
/// tenants whose keys are fine.
#[tokio::test]
async fn a_workspaces_circuit_is_its_own() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let endpoint = format!("openai:http://{}", Uuid::now_v7());
    let theirs = Some(workspace);

    for _ in 0..5 {
        breaker::observe(
            pool,
            &endpoint,
            theirs,
            Observation::Unreachable,
            a_caller(),
            Some("boom"),
        )
        .await;
    }

    assert_eq!(
        breaker::check(pool, &endpoint, theirs).await,
        Verdict::Reject,
        "the workspace that saw the failures backs off"
    );
    assert_eq!(
        breaker::check(pool, &endpoint, PLATFORM).await,
        Verdict::Allow,
        "everybody else is unaffected by one workspace's credential"
    );

    finish!(db);
}

/// Being told off is not the same as being down, and a 500 is neither.
#[tokio::test]
async fn what_a_provider_error_is_evidence_of() {
    use outturn::gateway::breaker::policy::Observation;
    use outturn::gateway::llm::provider::ProviderError;

    let evidence = outturn::gateway::breaker::observation_for;

    // The provider answered correctly and the request was wrong, or it is
    // alive and pushing back. Neither says anything about its health.
    assert_eq!(
        evidence(&ProviderError::RateLimited),
        Observation::NotEvidence
    );
    assert_eq!(
        evidence(&ProviderError::Upstream(
            "400: model does not support tools".into()
        )),
        Observation::NotEvidence
    );
    assert_eq!(
        evidence(&ProviderError::Upstream("404: no such model".into())),
        Observation::NotEvidence
    );

    // Nothing about one caller's request explains not being there at all, so
    // one caller reporting it is enough to open a circuit.
    assert_eq!(
        evidence(&ProviderError::Unavailable),
        Observation::Unreachable
    );

    // It answered and it failed. From one caller that is indistinguishable
    // from a request that provoked it, so it waits for company rather than
    // counting on its own -- this is the case that used to take a provider
    // away from everybody because one agent kept sending something bad.
    assert_eq!(
        evidence(&ProviderError::Upstream(
            "503: upstream connect error".into()
        )),
        Observation::Undetermined
    );
    assert_eq!(
        evidence(&ProviderError::Upstream("500: internal error".into())),
        Observation::Undetermined
    );
    // A transport error has no status to read, and a timeout is the provider
    // failing to answer rather than refusing. Both are failures; both still
    // want breadth before they mean an outage.
    assert_eq!(
        evidence(&ProviderError::Upstream(
            "error sending request for url".into()
        )),
        Observation::Undetermined
    );
    assert_eq!(
        evidence(&ProviderError::Upstream("408: request timeout".into())),
        Observation::Undetermined
    );
}

// -- Traffic routing -----------------------------------------------------------

use outturn::gateway::routing;

async fn add_route(
    pool: &PgPool,
    workspace: Option<Uuid>,
    traffic: &str,
    priority: i32,
    base_url: &str,
    model: &str,
) {
    sqlx::query(
        "insert into traffic_routes \
             (id, workspace_id, traffic_type, priority, provider, base_url, model) \
         values (uuidv7(), $1, $2, $3, 'openai', $4, $5)",
    )
    .bind(workspace)
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 30, "http://third", "c").await;
    add_route(pool, None, "assistant", 10, "http://first", "a").await;
    add_route(pool, None, "assistant", 20, "http://second", "b").await;

    let routes = routing::routes_for(pool, workspace, "assistant")
        .await
        .expect("routes");
    let models: Vec<&str> = routes.iter().map(|r| r.model.as_str()).collect();
    assert_eq!(models, ["a", "b", "c"]);

    finish!(db);
}

/// A workspace's own routes replace the system defaults rather than extending
/// them, so its traffic cannot quietly fall through to somebody else's
/// endpoint once it has said where it wants to go.
#[tokio::test]
async fn workspace_routes_replace_the_system_defaults() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 10, "http://shared", "default").await;
    let inherited = routing::routes_for(pool, workspace, "assistant")
        .await
        .expect("routes");
    assert_eq!(
        inherited.len(),
        1,
        "a workspace with no routes uses the defaults"
    );
    assert_eq!(inherited[0].model, "default");

    add_route(
        pool,
        Some(workspace),
        "assistant",
        10,
        "http://theirs",
        "theirs",
    )
    .await;
    let own = routing::routes_for(pool, workspace, "assistant")
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
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 10, "http://good", "expensive").await;
    add_route(pool, None, "title", 10, "http://cheap", "small").await;

    let assistant = routing::routes_for(pool, workspace, "assistant")
        .await
        .expect("a");
    let title = routing::routes_for(pool, workspace, "title")
        .await
        .expect("t");
    let unknown = routing::routes_for(pool, workspace, "nothing-here")
        .await
        .expect("u");

    assert_eq!(assistant[0].model, "expensive");
    assert_eq!(title[0].model, "small");
    assert!(
        unknown.is_empty(),
        "an unrouted type falls back to static providers"
    );

    finish!(db);
}

/// The breaker and the route list meet: a destination whose circuit is open is
/// skipped, and the next in precedence order serves the request.
#[tokio::test]
async fn an_open_circuit_removes_a_destination_from_the_list() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    add_route(pool, None, "assistant", 10, "http://primary", "a").await;
    add_route(pool, None, "assistant", 20, "http://fallback", "b").await;

    let routes = routing::routes_for(pool, workspace, "assistant")
        .await
        .expect("routes");
    for _ in 0..5 {
        unreachable(pool, &routes[0].endpoint()).await;
    }

    let mut usable = Vec::new();
    for route in &routes {
        if breaker::check(pool, &route.endpoint(), PLATFORM).await == Verdict::Allow {
            usable.push(route.model.as_str());
        }
    }

    assert_eq!(
        usable,
        ["b"],
        "the failed destination drops out of the list"
    );

    finish!(db);
}

// -- Serialised work ----------------------------------------------------------

/// Two turns in one conversation are answered in order, never at once.
///
/// Concurrent turns each read a history that does not contain the other's
/// reply, so the second answers a question without knowing what was just said
/// -- and the stored transcript then implies a causality that never happened.
#[tokio::test]
async fn work_sharing_a_key_does_not_run_concurrently() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let session = Uuid::now_v7().to_string();

    for i in 0..3 {
        jobs::enqueue(
            pool,
            workspace,
            "test.serial",
            serde_json::json!({ "i": i }),
            None,
            Some(&session),
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }

    let first = jobs::claim(pool, &["test.serial"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(first.len(), 1, "only one turn of a conversation may run");

    // A second worker arriving mid-turn gets nothing, rather than starting a
    // parallel reply.
    let second = jobs::claim(pool, &["test.serial"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert!(
        second.is_empty(),
        "the session is busy, so nothing is claimable"
    );

    // Once the turn finishes, the next is available.
    jobs::complete(pool, first[0].job.id, None)
        .await
        .expect("complete");
    let third = jobs::claim(pool, &["test.serial"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(
        third.len(),
        1,
        "the queue resumes when the session frees up"
    );

    finish!(db);
}

/// A session with a turn running and another queued must not stall the queue.
///
/// The claim takes a bounded number of candidates and then applies the
/// serial-key guard. If keys already running are not excluded before that
/// bound, the busy session's queued turn is the top candidate every time,
/// the guard rejects it, and work for every other session is never looked at
/// -- with a limit of one, which is what runtimes ask with, one person
/// sending two messages froze dispatch for the whole cluster.
#[tokio::test]
async fn a_busy_session_does_not_block_other_sessions_from_being_claimed() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let busy = Uuid::now_v7().to_string();
    let other = Uuid::now_v7().to_string();

    // The busy session: one turn taken, one waiting behind it.
    for i in 0..2 {
        jobs::enqueue(
            pool,
            workspace,
            "test.hol",
            serde_json::json!({ "i": i }),
            None,
            Some(&busy),
            jobs::PRIORITY_REALTIME,
        )
        .await
        .expect("enqueue");
    }
    let running = jobs::claim(pool, &["test.hol"], 1, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(running.len(), 1);

    // Another session, queued after the busy one's second turn.
    let waiting = jobs::enqueue(
        pool,
        workspace,
        "test.hol",
        serde_json::json!({}),
        None,
        Some(&other),
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    let claimed = jobs::claim(pool, &["test.hol"], 1, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(
        claimed.iter().map(|h| h.job.id).collect::<Vec<_>>(),
        vec![waiting],
        "a claim with room for one turn should take the other session's, not stall on the busy one"
    );

    finish!(db);
}

/// Serialisation is per key: different conversations still run in parallel.
#[tokio::test]
async fn different_keys_still_run_in_parallel() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    for _ in 0..3 {
        let session = Uuid::now_v7().to_string();
        jobs::enqueue(
            pool,
            workspace,
            "test.parallel",
            serde_json::json!({}),
            None,
            Some(&session),
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }

    let claimed = jobs::claim(pool, &["test.parallel"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 3, "separate conversations are independent");

    finish!(db);
}

/// Work with no key is unconstrained, as it was before serialisation existed.
#[tokio::test]
async fn unkeyed_work_is_not_serialised() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    for i in 0..4 {
        jobs::enqueue(
            pool,
            workspace,
            "test.unkeyed",
            serde_json::json!({ "i": i }),
            None,
            None,
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }

    let claimed = jobs::claim(pool, &["test.unkeyed"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(
        claimed.len(),
        4,
        "nothing without a key should be held back"
    );

    finish!(db);
}

/// Concurrent claimers cannot both take work for the same key.
///
/// The window between "is anything running for this key" and "mark it running"
/// is where two workers would otherwise both decide yes.
#[tokio::test]
async fn racing_claimers_cannot_both_take_one_key() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let session = Uuid::now_v7().to_string();

    for i in 0..6 {
        jobs::enqueue(
            pool,
            workspace,
            "test.race",
            serde_json::json!({ "i": i }),
            None,
            Some(&session),
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }

    // Fewer racers than the pool has connections. Asking for more than that
    // makes one claimer wait, and under load that wait can outlast sqlx's
    // acquire timeout -- which fails this test for a reason that has nothing
    // to do with what it is testing.
    let racers = 4;
    let mut claims = Vec::new();
    for _ in 0..racers {
        claims.push(jobs::claim(pool, &["test.race"], 10, jobs::DEFAULT_LEASE));
    }
    let results = futures::future::join_all(claims).await;

    // Errors are counted apart from claims, because "a claimer could not
    // reach the database" and "two claimers both won" are opposite findings
    // and must not arrive as the same failure.
    let mut taken = 0usize;
    let mut failed = Vec::new();
    for result in &results {
        match result {
            Ok(claimed) => taken += claimed.len(),
            Err(e) => failed.push(e.to_string()),
        }
    }

    assert!(
        failed.is_empty(),
        "{} of {racers} claimers could not reach the database: {failed:?}",
        failed.len()
    );
    assert_eq!(
        taken, 1,
        "{racers} workers raced and {taken} turns started, which means the \
         serialisation guarantee does not hold"
    );

    finish!(db);
}

#[tokio::test]
async fn a_released_job_is_not_held_to_have_tried() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.release",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    let claimed = jobs::claim(pool, &["test.release"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].job.attempts, 1, "claiming counts an attempt");

    jobs::release(
        pool,
        claimed[0].job.id,
        Duration::from_secs(0),
        jobs::MAX_RELEASES,
        None,
    )
    .await
    .expect("release");

    let (state, attempts): (String, i32) =
        sqlx::query_as("select state, attempts from jobs where id = $1")
            .bind(claimed[0].job.id)
            .fetch_one(pool)
            .await
            .expect("read job");
    assert_eq!(
        state, "pending",
        "a released job did not return to the queue"
    );
    assert_eq!(
        attempts, 0,
        "a job that never ran was charged an attempt, so a busy cluster \
         would exhaust its retries without running it once"
    );

    let again = jobs::claim(pool, &["test.release"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(again.len(), 1, "a released job must be claimable again");

    finish!(db);
}

#[tokio::test]
async fn a_released_job_waits_before_it_is_offered_again() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.backoff",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    let claimed = jobs::claim(pool, &["test.backoff"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    jobs::release(
        pool,
        claimed[0].job.id,
        Duration::from_secs(60),
        jobs::MAX_RELEASES,
        None,
    )
    .await
    .expect("release");

    // Otherwise a cluster with no room spends itself claiming and releasing
    // the same work as fast as it can.
    let again = jobs::claim(pool, &["test.backoff"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert!(
        again.is_empty(),
        "a released job was offered again immediately"
    );

    finish!(db);
}

#[tokio::test]
async fn only_a_running_job_can_be_released() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.norun",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    let pending: uuid::Uuid = sqlx::query_scalar("select id from jobs where kind = $1")
        .bind("test.norun")
        .fetch_one(pool)
        .await
        .expect("read job");

    // A release that could touch a pending job would let a late reply from an
    // abandoned turn give back an attempt that a live claimer is spending.
    assert!(
        jobs::release(
            pool,
            pending,
            Duration::from_secs(0),
            jobs::MAX_RELEASES,
            None
        )
        .await
        .is_err(),
        "a job that was never claimed was released"
    );

    finish!(db);
}

#[tokio::test]
async fn the_backlog_counts_work_that_could_actually_start() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    // One session with six queued turns is one unit of work, not six: the
    // serial key admits one at a time. Counting rows would ask an autoscaler
    // for pods that cannot claim anything.
    let session = Uuid::now_v7().to_string();
    for i in 0..6 {
        jobs::enqueue(
            pool,
            workspace,
            "chat.turn",
            serde_json::json!({ "i": i }),
            None,
            Some(&session),
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }
    // Two more sessions, and two jobs with nothing to serialise on.
    for _ in 0..2 {
        let other = Uuid::now_v7().to_string();
        jobs::enqueue(
            pool,
            workspace,
            "chat.turn",
            serde_json::json!({}),
            None,
            Some(&other),
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }
    for _ in 0..2 {
        jobs::enqueue(
            pool,
            workspace,
            "chat.turn",
            serde_json::json!({}),
            None,
            None,
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }

    let backlog = || async {
        sqlx::query_scalar::<_, i64>(
            "select coalesce(max(claimable), 0) from job_backlog where kind = 'chat.turn'",
        )
        .fetch_one(pool)
        .await
        .expect("backlog")
    };

    // Three serialised keys plus two unconstrained jobs.
    assert_eq!(
        backlog().await,
        5,
        "the backlog counted rows rather than work"
    );

    // The number the autoscaler reads has to be the number a fleet with enough
    // room could start, or it scales towards pods that would sit idle.
    let claimed = jobs::claim(pool, &["chat.turn"], 100, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(
        claimed.len(),
        5,
        "the backlog and the claim disagree about what can start"
    );

    // With those running, only their keys are blocked: nothing is left.
    assert_eq!(
        backlog().await,
        0,
        "work already running was counted as waiting for a pod"
    );

    finish!(db);
}

#[tokio::test]
async fn a_job_nowhere_will_run_eventually_fails_rather_than_spinning() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.noroom",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    // Small budget so the test states the rule rather than the constant.
    let budget = 3;
    let mut outcomes = Vec::new();
    for _ in 0..budget {
        let claimed = jobs::claim(pool, &["test.noroom"], 10, jobs::DEFAULT_LEASE)
            .await
            .expect("claim");
        assert_eq!(claimed.len(), 1, "a released job must come back round");
        outcomes.push(
            jobs::release(
                pool,
                claimed[0].job.id,
                Duration::from_secs(0),
                budget,
                None,
            )
            .await
            .expect("release"),
        );
    }

    assert_eq!(
        outcomes,
        vec![
            jobs::Released::Queued,
            jobs::Released::Queued,
            jobs::Released::GaveUp
        ],
        "a permanently full cluster must stop handing the same work round"
    );

    let (state, error): (String, Option<String>) =
        sqlx::query_as("select state, last_error from jobs where kind = $1")
            .bind("test.noroom")
            .fetch_one(pool)
            .await
            .expect("read job");
    assert_eq!(state, "failed");
    assert_eq!(error.as_deref(), Some("no runtime had room"));

    let again = jobs::claim(pool, &["test.noroom"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert!(again.is_empty(), "a job that gave up was offered again");

    finish!(db);
}

/// A heartbeat from a claim that has since been reaped renews nothing.
///
/// The dangerous case is not that the old holder fails to renew -- it is that
/// it succeeds. Renewing on state alone extends whichever claim is current,
/// tells the previous holder it still owns the job, and leaves two workers
/// streaming the same turn into the same reply.
#[tokio::test]
async fn a_stale_heartbeat_cannot_renew_a_claim_someone_else_holds() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    jobs::enqueue(
        pool,
        workspace,
        "test.stolen",
        serde_json::json!({}),
        None,
        None,
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    // The first holder's claim, with a lease short enough to lapse.
    let first = jobs::claim(pool, &["test.stolen"], 10, Duration::from_secs(0))
        .await
        .expect("claim");
    assert_eq!(first.len(), 1);
    let id = first[0].job.id;

    // Long enough that the reaper's grace window has also passed.
    tokio::time::sleep(jobs::LEASE_HEARTBEAT + Duration::from_millis(500)).await;
    assert_eq!(
        jobs::reap_abandoned(pool).await.expect("reap").0,
        1,
        "the abandoned claim should have been returned"
    );

    // A second holder takes it.
    let second = jobs::claim(pool, &["test.stolen"], 10, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(second.len(), 1, "the job should be claimable again");
    assert_eq!(second[0].job.id, id);

    // The first holder, still running, tries to renew with the token it was
    // issued -- which the second claim has since replaced.
    let renewed = jobs::extend_lease(
        pool,
        id,
        Duration::from_secs(60),
        first[0].job.lease_token.expect("a claim issues a token"),
    )
    .await
    .expect("extend");
    assert!(
        !renewed,
        "a heartbeat from a lapsed claim renewed a job someone else now holds, \
         so both holders believe they own it"
    );

    finish!(db);
}

/// A turn's progress belongs to the reply, never to the prompt.
///
/// Attaching deltas to the message being answered streams the reply into the
/// user's own words and leaves the assistant's message empty for ever: the
/// reader sees what they typed replaced by the answer, and a thinking
/// indicator that never resolves. The two ids are both called `message_id` in
/// their own scopes, which is how they get swapped.
#[tokio::test]
async fn a_turn_reports_against_its_reply_not_its_prompt() {
    use outturn::api::chat::{Delivery, Usage};

    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let (session_id, chat) = streamed_session(pool, workspace).await;

    let prompt = chat
        .append_message(
            session_id,
            "user",
            "What is the weather?",
            None,
            Usage::default(),
            Delivery::Steer,
            None,
        )
        .await
        .expect("prompt");

    let placeholder = chat
        .claim_placeholder(prompt.id, session_id)
        .await
        .expect("placeholder");

    assert_ne!(
        placeholder.message.id, prompt.id,
        "a reply is its own message"
    );

    // What a runtime reports lands on the reply.
    chat.set_message_content(
        placeholder.message.id,
        "It is raining.",
        Some("test-model"),
        None,
        Usage::default(),
        serde_json::json!({}),
    )
    .await
    .expect("finalise");

    let history = chat.messages(session_id).await.expect("history");
    let stored_prompt = history
        .messages
        .iter()
        .find(|m| m.id == prompt.id)
        .expect("the prompt is still there");
    let stored_reply = history
        .messages
        .iter()
        .find(|m| m.id == placeholder.message.id)
        .expect("the reply is still there");

    assert_eq!(
        stored_prompt.content, "What is the weather?",
        "the prompt was overwritten with the reply"
    );
    assert_eq!(
        stored_reply.content, "It is raining.",
        "the reply is empty, so a reader waits on it for ever"
    );

    finish!(db);
}

/// A backlog of scheduled work never puts itself in front of a person.
///
/// The queue is otherwise first-come, so a few hundred background jobs
/// enqueued a moment earlier would each be taken before a chat turn that
/// somebody is sitting and watching. Priority is read before `run_after`, so
/// the next slot to free anywhere in the fleet goes to whoever is waiting.
#[tokio::test]
async fn a_person_waiting_is_served_before_scheduled_work() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    // Queued first, and plenty of it.
    for i in 0..20 {
        jobs::enqueue(
            pool,
            workspace,
            "test.qos",
            serde_json::json!({ "background": i }),
            None,
            None,
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }

    // Queued last, by somebody who is waiting.
    jobs::enqueue(
        pool,
        workspace,
        "test.qos",
        serde_json::json!({ "realtime": true }),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    let claimed = jobs::claim(pool, &["test.qos"], 1, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        claimed[0].job.payload["realtime"],
        serde_json::json!(true),
        "twenty background jobs were taken before the person waiting"
    );

    // And within a priority, it is still oldest first.
    let next = jobs::claim(pool, &["test.qos"], 1, jobs::DEFAULT_LEASE)
        .await
        .expect("claim");
    assert_eq!(
        next[0].job.payload["background"],
        serde_json::json!(0),
        "ordering within a priority stopped being first-come"
    );

    finish!(db);
}

/// The pod count answers with a floor when nothing is happening, and rises
/// for each kind of demand at its own weight.
#[tokio::test]
async fn the_pod_count_leads_the_queue_rather_than_following_it() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    let pods = || async {
        sqlx::query_scalar::<_, i32>("select pods from desired_runtime_pods")
            .fetch_one(pool)
            .await
            .expect("pods")
    };

    let idle = pods().await;
    assert!(
        idle >= 2,
        "an idle cluster should still hold a floor, got {idle}"
    );

    // Conversations somebody is in raise it before any work is queued, which
    // is the half of the estimate that leads rather than follows.
    let (session_id, _) = streamed_session(pool, workspace).await;
    sqlx::query(
        "insert into live_sessions (session_id, expires_at) \
         values ($1, now() + interval '5 minutes')",
    )
    .bind(session_id)
    .execute(pool)
    .await
    .expect("live session");
    let with_session = pods().await;
    assert_eq!(
        with_session,
        idle + 1,
        "a live conversation should be counted before it queues anything, got \
         {with_session}"
    );

    // Expired rows stop counting without anything having to sweep them.
    sqlx::query("update live_sessions set expires_at = now() - interval '1 minute'")
        .execute(pool)
        .await
        .expect("expire");
    assert_eq!(
        pods().await,
        idle,
        "an expired session kept counting, so silence never releases a pod"
    );

    // Background work raises it, but gently -- nobody is waiting.
    for i in 0..8 {
        jobs::enqueue(
            pool,
            workspace,
            "chat.turn",
            serde_json::json!({ "i": i }),
            None,
            None,
            jobs::PRIORITY_BACKGROUND,
        )
        .await
        .expect("enqueue");
    }
    let with_background = pods().await;
    assert_eq!(
        with_background,
        idle + 1,
        "eight background jobs should want one more pod, got {with_background}"
    );

    // The same amount of realtime work wants more, because someone is waiting.
    for i in 0..8 {
        jobs::enqueue(
            pool,
            workspace,
            "chat.turn",
            serde_json::json!({ "r": i }),
            None,
            None,
            jobs::PRIORITY_REALTIME,
        )
        .await
        .expect("enqueue");
    }
    let with_realtime = pods().await;
    assert!(
        with_realtime >= with_background + 4,
        "realtime work should weigh more heavily than background, got \
         {with_realtime} against {with_background}"
    );

    finish!(db);
}

/// A turn somebody stopped must never come back.
///
/// Retrying is the ordinary answer to a failure, and a cancelled turn fails
/// more often than most: cutting its stream is what stopping it means, and a
/// cut stream is an error to everything downstream. Without this the stop puts
/// the turn straight back on the queue to be stopped again, spending its
/// attempts and its allowance on work nobody wants.
#[tokio::test]
async fn a_cancelled_turn_is_not_retried_when_it_fails() {
    let (db, workspace_id) = setup().await;
    let pool = &db.pool;

    let session_id = Uuid::now_v7();
    let job_id = jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        serde_json::json!({ "session_id": session_id }),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    let claimed = jobs::claim(pool, &["chat.turn"], 1, Duration::from_secs(60))
        .await
        .expect("claim");
    let lease = claimed.first().expect("a job to claim").job.lease_token;

    // Stopped while running, then failing -- which is what a cut stream looks
    // like to the tier recording the result.
    jobs::request_cancel(pool, job_id).await.expect("cancel");
    jobs::fail(pool, job_id, "stream ended", Duration::ZERO, lease)
        .await
        .expect("fail");

    let state: String = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("state");
    assert_eq!(
        state, "cancelled",
        "a stopped turn was handed back to the queue, so stopping it starts it again"
    );

    assert!(
        jobs::claim(pool, &["chat.turn"], 1, Duration::from_secs(60))
            .await
            .expect("claim")
            .is_empty(),
        "a cancelled turn was claimable, so the work somebody stopped runs anyway"
    );
}

/// The same, for the pod being lost rather than the turn failing.
#[tokio::test]
async fn a_cancelled_turn_is_not_revived_by_the_reaper() {
    let (db, workspace_id) = setup().await;
    let pool = &db.pool;

    let job_id = jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        serde_json::json!({ "session_id": Uuid::now_v7() }),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    // Claimed with a lease that has already lapsed, as a lost pod leaves it.
    jobs::claim(pool, &["chat.turn"], 1, Duration::ZERO)
        .await
        .expect("claim");
    jobs::request_cancel(pool, job_id).await.expect("cancel");

    jobs::reap_abandoned(pool).await.expect("reap");

    let state: String = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(job_id)
        .fetch_one(pool)
        .await
        .expect("state");
    assert_eq!(
        state, "cancelled",
        "losing the pod running a cancelled turn undid the stop"
    );
}

/// Stop means the turn on screen, which is the running one.
///
/// A session can hold both: a follow-up sent while a turn streams is queued
/// behind it, and job ids being time-ordered makes the queued one the newer.
/// Taking the newest stops the turn nobody is watching, answers "stopped",
/// and discards the follow-up's turn as well.
#[tokio::test]
async fn stopping_a_session_finds_the_turn_that_is_running() {
    let (db, workspace_id) = setup().await;
    let pool = &db.pool;

    let session_id = Uuid::now_v7();
    let payload = serde_json::json!({ "session_id": session_id });

    let running = jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        payload.clone(),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue the first");
    jobs::claim(pool, &["chat.turn"], 1, Duration::from_secs(60))
        .await
        .expect("claim");

    // Queued behind it, and therefore newer.
    let queued = jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        payload,
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue the second");
    assert!(
        queued > running,
        "the queued turn should sort after the running one"
    );

    let found = jobs::live_turn_for_session(pool, workspace_id, session_id)
        .await
        .expect("look up")
        .expect("a live turn");
    assert_eq!(
        found, running,
        "stop resolved the queued turn, so the turn being watched would have \
         kept generating while its follow-up was thrown away"
    );
}

/// With nothing running, the oldest queued turn is the one that answers.
#[tokio::test]
async fn with_nothing_running_the_oldest_queued_turn_is_found() {
    let (db, workspace_id) = setup().await;
    let pool = &db.pool;

    let session_id = Uuid::now_v7();
    let payload = serde_json::json!({ "session_id": session_id });
    let first = jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        payload.clone(),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");
    jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        payload,
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    let found = jobs::live_turn_for_session(pool, workspace_id, session_id)
        .await
        .expect("look up")
        .expect("a live turn");
    assert_eq!(
        found, first,
        "the turn at the front of the queue is the one being waited on"
    );
}

/// A pending turn is over at once; a running one is asked and keeps running.
#[tokio::test]
async fn cancelling_says_what_it_actually_did() {
    let (db, workspace_id) = setup().await;
    let pool = &db.pool;

    let pending = jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        serde_json::json!({ "session_id": Uuid::now_v7() }),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    assert_eq!(
        jobs::request_cancel(pool, pending).await.expect("cancel"),
        jobs::Cancelled::BeforeItRan,
        "nothing had it, so there was nobody to tell and nothing to unwind"
    );
    // Idempotent: pressing stop twice is what somebody does when the first
    // press seems not to have worked.
    assert_eq!(
        jobs::request_cancel(pool, pending).await.expect("again"),
        jobs::Cancelled::AlreadyOver,
    );

    let running = jobs::enqueue(
        pool,
        workspace_id,
        "chat.turn",
        serde_json::json!({ "session_id": Uuid::now_v7() }),
        None,
        None,
        jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");
    jobs::claim(pool, &["chat.turn"], 1, Duration::from_secs(60))
        .await
        .expect("claim");

    assert_eq!(
        jobs::request_cancel(pool, running).await.expect("cancel"),
        jobs::Cancelled::WhileRunning,
        "it is still running, and saying otherwise lies to whoever is watching"
    );
    assert!(
        jobs::cancel_requested(pool, running).await.expect("asked"),
        "the request must outlive the call that made it, or the pod running \
         the turn never finds out"
    );
}

/// A narrowed caller is never shown another agent's events.
///
/// The feed carries the conversation as it is written, so a narrowing that
/// stopped at the transcript would refuse the history and stream the present.
/// Pinned at the store rather than only through the API because the filter is
/// in the query -- and it is written out twice there, once for a session-scoped
/// poll and once for a workspace-wide one, since `sqlx::query` takes a literal
/// and the partial index on `(session_id, id)` is lost to an `or`-guard. Two
/// copies that must agree; this is what says they do.
#[tokio::test]
async fn a_narrowed_caller_is_never_shown_another_agents_events() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;
    let bus = EventBus::spawn(pool.clone());

    let (mine, mine_agent, subject) = session_with_agent(pool, workspace).await;
    let (theirs, theirs_agent, _) = session_with_agent(pool, workspace).await;

    events::append(
        pool,
        workspace,
        Some(theirs),
        "chat.message",
        serde_json::json!({}),
    )
    .await
    .expect("append");
    let ours = events::append(
        pool,
        workspace,
        Some(mine),
        "chat.message",
        serde_json::json!({}),
    )
    .await
    .expect("append");

    let visible = events::Visible::of(
        &outturn::api::scope::Reach::of([mine_agent].into_iter().collect()),
        subject,
    )
    .expect("narrowed to one agent");

    // Workspace-wide: the other agent's session is filtered out in the query.
    let found = events::since(pool, workspace, None, Uuid::nil(), 100, Some(&visible))
        .await
        .expect("since");
    assert_eq!(
        found.len(),
        1,
        "a narrowing let another agent through: {found:?}"
    );
    assert_eq!(found[0].id, ours);

    // Session-scoped at the same session: the other copy of the clause.
    let found = events::wait_for(
        pool,
        &bus,
        workspace,
        Some(theirs),
        Uuid::nil(),
        100,
        Duration::from_millis(300),
        std::future::pending(),
        Some(&visible),
    )
    .await
    .expect("wait");
    assert!(
        found.is_empty(),
        "the session-scoped copy of the filter disagreed with the workspace-wide one: {found:?}"
    );

    let _ = theirs_agent;
    finish!(db);
}

/// A narrowed caller's cursor moves over events they cannot see.
///
/// The cursor is taken from what the caller was handed, so a window holding
/// nothing for them used to leave it exactly where it was -- and the next poll
/// rescanned the same span, and the one after a longer one. A busy agent they
/// cannot see turned an idle reader into a scan of the day's events on every
/// notification. The watermark is the end of the window that *was* examined,
/// which is why it can move without skipping anything still to arrive.
#[tokio::test]
async fn a_watermark_moves_a_cursor_over_events_that_were_filtered_out() {
    let (db, workspace) = setup_or_skip!();
    let pool = &db.pool;

    let (theirs, _, _) = session_with_agent(pool, workspace).await;

    let mut last = Uuid::nil();
    for _ in 0..3 {
        last = events::append(
            pool,
            workspace,
            Some(theirs),
            "chat.delta",
            serde_json::json!({}),
        )
        .await
        .expect("append");
    }

    let high = events::watermark(pool, workspace, None, Uuid::nil(), 100)
        .await
        .expect("watermark");
    assert_eq!(
        high,
        Some(last),
        "the watermark did not reach the end of the window"
    );

    // Bounded by the same limit the read uses, so nothing later is skipped.
    let first_only = events::watermark(pool, workspace, None, Uuid::nil(), 1)
        .await
        .expect("watermark");
    assert!(
        first_only.is_some() && first_only != Some(last),
        "the watermark ran past the window it was asked about: {first_only:?}"
    );

    finish!(db);
}

/// A session belonging to a fresh agent, and who started it.
async fn session_with_agent(pool: &sqlx::PgPool, workspace: Uuid) -> (Uuid, Uuid, Uuid) {
    let user_id = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, $2)")
        .bind(user_id)
        .bind("Test")
        .execute(pool)
        .await
        .expect("user");

    let agent_id = Uuid::now_v7();
    sqlx::query("insert into agents (id, workspace_id, name, slug) values ($1, $2, $3, $4)")
        .bind(agent_id)
        .bind(workspace)
        .bind("A")
        .bind(format!("a-{}", agent_id.simple()))
        .execute(pool)
        .await
        .expect("agent");

    let store: std::sync::Arc<dyn outturn::api::chat::ChatStore> =
        std::sync::Arc::new(outturn::api::chat::PostgresChatStore::new(pool.clone()));
    let session = store
        .create_session(
            workspace,
            user_id,
            outturn::api::chat::CreateSession {
                agent_id,
                title: String::new(),
                account: None,
            },
        )
        .await
        .expect("session");

    (session.id, agent_id, user_id)
}
