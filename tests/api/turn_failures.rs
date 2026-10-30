//! A turn interrupted between the steps it takes: the API stopping while a
//! report is half read, and the runtime stopping after the API committed its
//! result but before it heard so. Each is simulated by stopping at that step
//! and then running what recovers it -- the reaper and the next claim --
//! rather than by waiting for a lease to run out.

use super::leases::report_done;
use super::*;

async fn replies(h: &Harness, session: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "select content from agent_messages where session_id = $1 and role = 'assistant' \
         order by id",
    )
    .bind(session)
    .fetch_all(&h.db.pool)
    .await
    .expect("replies")
}

async fn job_state(h: &Harness, job: Uuid) -> String {
    sqlx::query_scalar("select state::text from jobs where id = $1")
        .bind(job)
        .fetch_one(&h.db.pool)
        .await
        .expect("job")
}

/// A committed turn stays committed when the runtime that reported it never
/// hears so.
///
/// The reply, `chat.done` and the job's closing are one transaction, so once
/// the report commits there is no middle for a crash to land in. What is
/// left is the runtime: a pod that died before reading the 204 comes back,
/// or its replacement does, and either hands the turn back or reports it
/// again. Neither may reopen it, the reaper has nothing to take, and the next
/// claim finds nothing -- one answer to one prompt.
#[tokio::test]
async fn a_committed_turn_is_not_reopened_by_a_runtime_that_never_heard() {
    let h = harness_or_skip!();
    let (job, lease, session, runtime) = a_leased_turn(&h).await;
    let (job, lease): (Uuid, Uuid) = (job.parse().unwrap(), lease.parse().unwrap());
    let (status, body) = report_done(&h, &runtime, job, lease, "Hi").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(job_state(&h, job).await, "succeeded");

    // The runtime, not having heard, hands the turn back...
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/abandon"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    // ...and reports it a second time.
    let (status, body) = report_done(&h, &runtime, job, lease, "Hi again").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    assert_eq!(job_state(&h, job).await, "succeeded");
    let (reaped, _) = outturn::jobs::reap_abandoned(&h.db.pool).await.unwrap();
    assert_eq!(reaped, 0, "a committed turn was reaped");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &["chat.turn"],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    assert!(claimed.is_empty(), "a committed turn was handed out again");
    assert_eq!(replies(&h, session).await, vec!["Hi".to_string()]);
    let done: i64 = sqlx::query_scalar(
        "select count(*) from events where session_id = $1 and kind = 'chat.done'",
    )
    .bind(session)
    .fetch_one(&h.db.pool)
    .await
    .expect("count");
    assert_eq!(done, 1, "the reader was told the turn finished twice");
    finish!(h);
}

/// A report cut off part way -- the API pod gone with the request, after a
/// delta was recorded and before the turn was finished -- leaves a turn the
/// reaper recovers. What the reader already saw is kept, sealed as the
/// attempt it was, and the retry answers beside it as the next attempt
/// rather than writing over it (`attempt_for`); the turn finishes once.
///
/// Dropping the request is the crash: the handler stops at whatever await it
/// had reached, with nothing after it run. The body says when the handler
/// asks for its second line, which is after it has dealt with the first, so
/// the drop lands mid-turn by construction rather than by timing. The lease
/// is then made to look lapsed, as it would once nobody renewed it.
#[tokio::test]
async fn a_turn_cut_off_mid_report_is_reaped_and_retried_beside_what_it_said() {
    let h = harness_or_skip!();
    let (job, lease, session, runtime) = a_leased_turn(&h).await;
    let job: Uuid = job.parse().unwrap();

    let asked_again = std::sync::Arc::new(tokio::sync::Notify::new());
    let body = {
        use futures::StreamExt as _;
        let asked_again = asked_again.clone();
        let delta = futures::stream::once(async {
            Ok::<_, std::io::Error>(bytes::Bytes::from(
                "{\"kind\":\"delta\",\"idx\":0,\"text\":\"Half an \"}\n",
            ))
        });
        // Signals when polled, then never yields: a stream whose sender died.
        let stall = futures::stream::once(async move {
            asked_again.notify_one();
            std::future::pending::<()>().await;
        })
        .filter_map(|()| async { None::<Result<bytes::Bytes, std::io::Error>> });
        delta.chain(stall)
    };
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from_stream(body))
        .expect("request");
    tokio::select! {
        _ = h.send(req) => panic!("a report with no end finished"),
        _ = asked_again.notified() => {}
    }

    // Nothing renews it now; the reaper finds it as it would once it lapsed.
    assert_eq!(job_state(&h, job).await, "running");
    sqlx::query("update jobs set leased_until = now() - interval '1 second' where id = $1")
        .bind(job)
        .execute(&h.db.pool)
        .await
        .expect("lapse");
    let (reaped, gave_up) = outturn::jobs::reap_abandoned(&h.db.pool).await.unwrap();
    assert_eq!((reaped, gave_up.len()), (1, 0));
    assert_eq!(job_state(&h, job).await, "pending");

    let (again, lease, _) = take_assignment(&h).await;
    assert_eq!(again, job, "the cut-off turn was not the one retried");
    let (status, body) = report_done(&h, &runtime, job, lease, "Half an answer, whole").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    assert_eq!(job_state(&h, job).await, "succeeded");
    assert_eq!(
        replies(&h, session).await,
        vec!["Half an ".to_string(), "Half an answer, whole".to_string()],
        "the interrupted attempt and the retry should each keep their own words"
    );
    let attempts: Vec<i32> = sqlx::query_scalar(
        "select attempt from agent_messages where session_id = $1 and role = 'assistant' \
         order by attempt",
    )
    .bind(session)
    .fetch_all(&h.db.pool)
    .await
    .expect("attempts");
    assert_eq!(attempts, vec![1, 2]);
    let done: i64 = sqlx::query_scalar(
        "select count(*) from events where session_id = $1 and kind = 'chat.done'",
    )
    .bind(session)
    .fetch_one(&h.db.pool)
    .await
    .expect("count");
    assert_eq!(done, 1, "the turn was announced finished other than once");
    finish!(h);
}
