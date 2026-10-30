//! Leases at the edges a running turn crosses: lost while the turn is being
//! prepared, and presented stale to the side channels a turn reports
//! through. See "A lease is the only thing joining a claim to the runtime
//! running it" in docs/invariants.md.

use super::*;

/// Sends `body` to a side channel a running turn reports through.
async fn side_channel(h: &Harness, uri: &str, lease: Uuid, body: Value) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    h.send(req).await
}

/// What `/work/gated` and `/work/wait` answer to `lease` for `job`.
async fn gated_and_wait(
    h: &Harness,
    session: Uuid,
    job: Uuid,
    lease: Uuid,
) -> [(&'static str, StatusCode, String); 2] {
    let (gated, gated_body) = side_channel(
        h,
        "/v1/work/gated",
        lease,
        serde_json::json!({
            "session_id": session, "job_id": job, "method": "POST",
            "host": "api.example.com", "path": "/charges", "body": "{}",
        }),
    )
    .await;
    let (wait, wait_body) = side_channel(
        h,
        "/v1/work/wait",
        lease,
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "sleep",
            "seconds": 600, "reason": "Waiting",
        }),
    )
    .await;
    [("gated", gated, gated_body), ("wait", wait, wait_body)]
}

fn assert_refused(answers: [(&'static str, StatusCode, String); 2], why: &str) {
    for (name, status, body) in answers {
        assert_eq!(status, StatusCode::CONFLICT, "{name} took {why}: {body}");
    }
}

/// Reports a turn finished, the whole stream in one line.
pub(super) async fn report_done(
    h: &Harness,
    runtime: &str,
    job: Uuid,
    lease: Uuid,
    content: &str,
) -> (StatusCode, String) {
    let line = serde_json::json!({
        "kind": "done", "content": content, "prompt_tokens": 1, "completion_tokens": 1,
    });
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .body(Body::from(format!("{line}\n")))
        .expect("request");
    h.send(req).await
}

/// A finished turn's lease is spent. The pod that reported it may still hold
/// the token -- a tool call in flight when the reply landed -- and what it
/// raises then belongs to no turn at all.
#[tokio::test]
async fn a_finished_turn_s_lease_is_refused_by_gated_and_wait() {
    let h = harness_or_skip!();
    let (job, lease, session, runtime) = a_leased_turn(&h).await;
    let (job, lease): (Uuid, Uuid) = (job.parse().unwrap(), lease.parse().unwrap());
    let (status, body) = report_done(&h, &runtime, job, lease, "Hi").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    assert_refused(
        gated_and_wait(&h, session, job, lease).await,
        "a finished turn's lease",
    );
    let held: i64 = sqlx::query_scalar("select count(*) from held_requests")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(held, 0, "a spent lease held a request");
    finish!(h);
}

/// A turn handed back is nobody's until it is claimed again, and the pod
/// that handed it back is not the one that claims it.
#[tokio::test]
async fn a_handed_back_turn_s_lease_is_refused_by_gated_and_wait() {
    let h = harness_or_skip!();
    let (job, lease, session, runtime) = a_leased_turn(&h).await;
    let (job, lease): (Uuid, Uuid) = (job.parse().unwrap(), lease.parse().unwrap());
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/abandon"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let state: String = sqlx::query_scalar("select state::text from jobs where id = $1")
        .bind(job)
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(state, "pending", "the hand-back did not return the turn");

    assert_refused(
        gated_and_wait(&h, session, job, lease).await,
        "a handed-back lease",
    );
    finish!(h);
}

/// A lease is for one job. The current lease of one turn presented with the
/// id of another -- two turns on one pod, crossed -- reaches neither.
#[tokio::test]
async fn a_lease_presented_for_another_job_is_refused_by_gated_and_wait() {
    let h = harness_or_skip!();
    let (first, first_lease, first_session, _) = a_leased_turn(&h).await;
    let (first, first_lease): (Uuid, Uuid) = (first.parse().unwrap(), first_lease.parse().unwrap());
    let (workspace, agent): (Uuid, Uuid) =
        sqlx::query_as("select workspace_id, agent_id from agent_sessions where id = $1")
            .bind(first_session)
            .fetch_one(&h.db.pool)
            .await
            .expect("the first turn's session");
    // Another conversation's key, so the serial key admits both at once.
    let other_session = Uuid::now_v7();
    let (second, second_lease) = a_running_turn(&h, workspace, agent, other_session).await;

    for (job, lease) in [(first, first_lease), (second, second_lease)] {
        assert!(
            outturn::jobs::holds_lease(&h.db.pool, job, lease)
                .await
                .unwrap(),
            "each lease is good for its own turn"
        );
    }
    assert_refused(
        gated_and_wait(&h, other_session, second, first_lease).await,
        "another job's lease",
    );
    assert_refused(
        gated_and_wait(&h, first_session, first, second_lease).await,
        "another job's lease",
    );
    finish!(h);
}

/// A turn whose lease is lost while it is being prepared is not handed out.
///
/// Preparing can summarize the conversation, a model call of its own. Here
/// the lease is taken from under it at a moment it is known to be
/// mid-summary -- lapsed, reaped and claimed by a second pod in one step,
/// which is where the reaper and another poll would leave it, without
/// racing the heartbeat that would otherwise renew it first. When the
/// summary comes back, the poll has to notice the claim is no longer its own
/// and hand out nothing, leaving the turn with the pod that now holds it.
///
/// Having nothing to give, the poll waits out its own timeout; that answer
/// is what this test waits for.
#[tokio::test]
async fn a_turn_whose_lease_is_lost_while_it_is_prepared_is_not_handed_out() {
    use common::fake_gateway::{Behavior, FakeGateway};

    let gateway = FakeGateway::start(Behavior::HoldThenReply("Earlier: hellos.".into())).await;
    let h = harness_with_gateway(&gateway.url).await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let runtime = h.runtime_token(acme);
    let json = |body: &str| serde_json::from_str::<Value>(body).unwrap();

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
        )
        .await;
    let agent = json(&body)["id"].as_str().unwrap().to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent}","title":""}}"#),
        )
        .await;
    let session = json(&body)["id"].as_str().unwrap().to_string();
    let say = |content: &'static str| {
        let h = &h;
        let admin = admin.clone();
        let session = session.clone();
        async move {
            let (status, body) = h
                .post(
                    &format!("/v1/agent-sessions/{session}/messages"),
                    Some(&admin),
                    &serde_json::json!({ "content": content }).to_string(),
                )
                .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        }
    };
    // Enough conversation that the smallest budget makes the next turn
    // summarize.
    for _ in 0..outturn::api::chat::summarize::TAIL_MESSAGES {
        say(
            "Hello again. Here is another line of conversation, long enough that \
             a few turns of it outgrow the smallest budget a model may be given.",
        )
        .await;
        let (job, lease, _) = take_assignment(&h).await;
        let (status, body) = report_done(&h, &runtime, job, lease, "ok").await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
    let (status, body) = send_as(
        &h,
        "PUT",
        "/v1/settings/context_budget?model=01a00000-0000-7000-8000-000000000002",
        &admin,
        r#"{"value": 1000}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    say("one more").await;

    let take = h.post("/v1/work", Some(&runtime), "{}");
    tokio::pin!(take);
    tokio::select! {
        _ = &mut take => panic!("the turn was handed out before its summary was written"),
        _ = gateway.held_request_arrived() => {}
    }

    let (job, first): (Uuid, Uuid) = sqlx::query_as(
        "select id, lease_token from jobs where kind = 'chat.turn' and state = 'running'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("the turn being prepared");
    let second = Uuid::now_v7();
    sqlx::query(
        "update jobs set lease_token = $2, leased_until = now() + interval '1 hour' \
         where id = $1",
    )
    .bind(job)
    .bind(second)
    .execute(&h.db.pool)
    .await
    .expect("taken by a second pod");

    gateway.release_held();
    let (status, body) = take.await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body, "null",
        "a turn was handed out under a lease it had lost: {body}"
    );

    assert!(
        !outturn::jobs::holds_lease(&h.db.pool, job, first)
            .await
            .unwrap()
    );
    assert!(
        outturn::jobs::holds_lease(&h.db.pool, job, second)
            .await
            .unwrap(),
        "the first poll's way out disturbed the claim that replaced it"
    );
}
