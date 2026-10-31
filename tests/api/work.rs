//! Taking work: the runtime key, leases, reports and how a turn is prepared.

use super::*;

// -- Work distribution --------------------------------------------------------

/// No workspace role, however senior, can take work off the queue.
///
/// A turn handed out carries whichever workspace's transcript it belongs to,
/// that workspace's egress rules, and a token minted for it -- so anything that
/// can ask for work can ask for everyone's. Checking only that a Viewer is
/// refused would pass while an Admin walked through, which is exactly what
/// happened when these endpoints were gated on an authority workspaces hold.
#[tokio::test]
async fn no_workspace_role_can_take_work() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;

    for (email, role) in [
        ("viewer@acme.example", "viewer"),
        ("operator@acme.example", "operator"),
        ("admin@acme.example", "admin"),
    ] {
        let token = h.login_as(email, None, Some((acme, role))).await;
        let (status, body) = h.post("/v1/work", Some(&token), "{}").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{role:?} was handed work: {body}"
        );
    }

    // And a system administrator is a person, not the tier that runs turns.
    let sysadmin = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h.post("/v1/work", Some(&sysadmin), "{}").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a system admin took work: {body}"
    );

    let (status, _) = h.post("/v1/work", None, "{}").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A turn may only be reported while it is out with a runtime.
///
/// The claim is the ticket. Without this check a job id is enough to write
/// into a transcript -- including a second time, over a reply already
/// finished by the runtime that really ran it.
#[tokio::test]
async fn a_turn_that_is_not_running_cannot_be_reported() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let runtime = h.runtime_token(acme);

    // Queued but never handed out, so nothing is running it.
    let job = outturn::jobs::enqueue(
        &h.db.pool,
        acme,
        "chat.turn",
        serde_json::json!({}),
        None,
        None,
        outturn::jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, Uuid::now_v7().to_string())
        .body(Body::from("{}\n"))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a job nobody is running was reported: {body}"
    );
}

/// An empty queue is answered with nothing, not an error.
#[tokio::test]
async fn asking_for_work_when_there_is_none_says_so() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let runtime = h.runtime_token(acme);

    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body, "null",
        "an idle cluster should answer null, got {body}"
    );
}

/// A message absorbed by an attempt that was lost is offered to the retry.
///
/// The gateway marks a message as taken when it hands it over. If the pod
/// running that turn then dies, the retry starts over from the prompt -- and
/// unless the mark is cleared, the taken message is never handed over again,
/// while its own turn completes as "already answered". The message vanishes.
#[tokio::test]
async fn a_message_absorbed_by_a_lost_attempt_is_offered_to_the_retry() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    let session_id: Uuid = session["id"].as_str().expect("id").parse().expect("uuid");

    // The prompt, taken by a runtime.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"one"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job: Uuid = assignment["job_id"]
        .as_str()
        .expect("job")
        .parse()
        .expect("uuid");
    let lease = assignment["lease_token"]
        .as_str()
        .expect("lease")
        .to_string();
    let reply: Uuid = assignment["reply_id"]
        .as_str()
        .expect("reply")
        .parse()
        .expect("uuid");

    // A second message arrives mid-turn and the gateway hands it over.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"two"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let taken = outturn::gateway::routing::take_pending(&h.db.pool, session_id, reply)
        .await
        .expect("take");
    assert_eq!(
        taken.len(),
        1,
        "the steer should have been handed over once"
    );

    // The runtime dies without reporting, and hands the turn back.
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/abandon"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, &lease)
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The retry is handed the same turn, and the steer is pending again.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let retry: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    assert_eq!(
        retry["job_id"], assignment["job_id"],
        "the retry should be the same job"
    );

    let again = outturn::gateway::routing::take_pending(&h.db.pool, session_id, reply)
        .await
        .expect("take");
    assert_eq!(
        again.len(),
        1,
        "the message the lost attempt absorbed was never offered to the retry"
    );
    assert_eq!(again[0].content, "two");
}

/// Thinking off with prompt repetition sends the turn's prompt twice and
/// stores it once.
///
/// The setting accepts the value and reads it back, the runtime is handed the
/// repeated form with thinking off, and the transcript keeps what was typed.
#[tokio::test]
async fn prompt_repetition_reaches_the_model_and_not_the_transcript() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id").to_string();

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/settings/reasoning_effort"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"value":"none_repeat_prompt"}"#))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    let (_, body) = h
        .get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin))
        .await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
    assert_eq!(effort["value"], "none_repeat_prompt");

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    let session_id = session["id"].as_str().expect("id").to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"What is 17 times 23?"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    // Effort is the gateway's to send, per model; the setting's own value
    // never travels, and the gateway sends "none" for it
    // (model_settings::tests::a_template_wins_where_it_is_valid).
    assert!(
        assignment["reasoning_effort"].is_null(),
        "effort travelled with the turn: {}",
        assignment["reasoning_effort"]
    );
    let sent = assignment["conversation"].to_string();
    assert!(
        sent.contains(r"What is 17 times 23?\nLet me repeat that:\nWhat is 17 times 23?"),
        "the prompt was not repeated: {sent}"
    );

    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.contains("What is 17 times 23?") && !body.contains("Let me repeat that"),
        "the stored transcript should hold the prompt as typed: {body}"
    );
}

/// Only the runtime key takes work; a signed token never does.
///
/// The runtime holds no signing key, so nothing it could present is a token.
/// Conversely a turn token -- which the runtime does hold, one per turn --
/// must not open the work endpoints, or a leaked one would let its holder ask
/// for everyone's turns.
#[tokio::test]
async fn only_the_runtime_key_takes_work() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;

    let wrong = "not-the-key-not-the-key-not-the-key-no";
    let (status, body) = h.post("/v1/work", Some(wrong), "{}").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a wrong key was accepted: {body}"
    );

    let turn = h
        .minter
        .mint_turn(
            Uuid::now_v7(),
            acme,
            outturn::egress::commit::empty_root(),
            outturn::egress::gate::Gates::none().root(acme),
            None,
        )
        .expect("token");
    let (status, body) = h.post("/v1/work", Some(&turn), "{}").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a turn token opened the work endpoint: {body}"
    );

    let (status, body) = h.post("/v1/work", Some(&h.runtime_token(acme)), "{}").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the runtime key was refused: {body}"
    );
}

/// A running turn trades its gateway token in before it runs out.
///
/// A turn has no upper bound while it works -- onboarding in long-horizon mode
/// can run for hours -- and a five-minute token failed an eight-minute turn on
/// its next `fetch_url`. The runtime now asks for a fresh one once less than
/// fifteen minutes remain, and these are the rules the API holds it to.
#[tokio::test]
async fn a_running_turn_can_refresh_its_gateway_token() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    let session_id: Uuid = session["id"].as_str().expect("id").parse().expect("uuid");

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job").to_string();
    let lease = assignment["lease_token"]
        .as_str()
        .expect("lease")
        .to_string();
    let original = assignment["gateway_token"]
        .as_str()
        .expect("token")
        .to_string();
    assert!(
        assignment["gateway_token_expires_at"].is_string(),
        "the runtime is told when its token runs out: {body}"
    );

    let refresh = |lease: String, token: String| {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/v1/work/{job}/token"))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {runtime}"))
            .header(outturn::api::work::LEASE_HEADER, lease)
            .body(Body::from(
                serde_json::json!({ "gateway_token": token }).to_string(),
            ))
            .expect("request");
        h.send(req)
    };

    // The ordinary case: the new token says exactly what the old one did.
    let (status, body) = refresh(lease.clone(), original.clone()).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let refreshed: serde_json::Value = serde_json::from_str(&body).expect("refreshed");
    let before = h.gateway_validator.validate(&original).expect("original");
    let after = h
        .gateway_validator
        .validate(refreshed["gateway_token"].as_str().expect("token"))
        .expect("refreshed token is a gateway token");
    assert_eq!(after.subject, before.subject);
    assert_eq!(after.workspace_id, before.workspace_id);
    assert_eq!(
        after.egress_commitment, before.egress_commitment,
        "a reissued token may claim no more reach than the one it replaced"
    );
    assert_eq!(after.gate_commitment, before.gate_commitment);

    // Somebody else's lease: this pod is not running the turn.
    let (status, _) = refresh(Uuid::now_v7().to_string(), original.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A browser token cannot be traded in for a turn token.
    let (status, _) = refresh(lease.clone(), admin.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Another conversation's turn token, under this turn's lease. A runtime
    // holding two turns must not be able to swap one tenant's reach for
    // another's.
    let other = h
        .minter
        .mint_turn(
            Uuid::now_v7(),
            acme,
            before.egress_commitment.expect("egress"),
            before.gate_commitment.expect("gates"),
            None,
        )
        .expect("mint");
    let (status, _) = refresh(lease.clone(), other).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // An expired token is refused rather than read. A path honoring expired
    // credentials is the exception that outlives its reason.
    let expired = h
        .minter
        .mint_with_lifetime(
            outturn::auth::AUDIENCE_GATEWAY,
            session_id,
            acme,
            &[Role::Turn.to_string()],
            before.egress_commitment,
            before.gate_commitment,
            std::time::Duration::from_secs(1),
        )
        .expect("mint");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let (status, _) = refresh(lease, expired).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A turn that cannot be prepared -- here, a deployment with no model -- is
/// reported to the conversation and does not wedge it: once a model is
/// configured the next prompt is answered.
///
/// The failure used to leave the placeholder reply behind, empty, under a
/// job whose state the abandoned-reply guard did not list -- and every
/// later message in the session was refused as abandoned.
#[tokio::test]
async fn a_turn_that_failed_to_prepare_does_not_wedge_the_session() {
    let h = harness_or_skip!();
    // The harness seeds a chain for every test; this one is about having none.
    sqlx::query("delete from chain_assignments")
        .execute(&h.db.pool)
        .await
        .expect("no model configured");

    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent_id: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let session_id = h.session_with_a_message(&admin, agent_id, "hello").await;
    // Its first attempt is its last, so the failure is final at once.
    sqlx::query("update jobs set max_attempts = 1 where kind = 'chat.turn'")
        .execute(&h.db.pool)
        .await
        .expect("one attempt");

    // The runtime's poll fails the turn where it would have prepared it, and
    // finds nothing else to do.
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, "null", "a turn with no model was handed out: {body}");
    let error: Value = sqlx::query_scalar(
        "select payload from events where session_id = $1 and kind = 'chat.error'",
    )
    .bind(session_id)
    .fetch_one(&h.db.pool)
    .await
    .expect("the failure was announced");
    assert!(
        error["message"].as_str().unwrap().contains("no model"),
        "{error}"
    );

    // The operator gives it a model: the seeded chain, assigned again.
    sqlx::query(
        "insert into chain_assignments (id, workspace_id, traffic_type, chain_id) \
         values ($1, null, 'assistant', '01a00000-0000-7000-8000-000000000003')",
    )
    .bind(Uuid::now_v7())
    .execute(&h.db.pool)
    .await
    .expect("a model configured");
    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello again"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "the session wedged: {body}");

    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"]
        .as_str()
        .expect("the second prompt's turn");
    let lease = assignment["lease_token"].as_str().expect("lease");
    let stream = r#"{"kind":"done","content":"Hi","prompt_tokens":10,"completion_tokens":1}"#;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let (_, body) = h
        .get(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
        )
        .await;
    let history: Value = serde_json::from_str(&body).expect("history");
    let replies: Vec<&str> = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "assistant")
        .map(|m| m["content"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        replies,
        vec!["Hi"],
        "the failed turn's empty placeholder should be gone: {body}"
    );
    finish!(h);
}

/// Reports a turn over a stream that stays open while the lease is taken away,
/// then sends `last` (or nothing) and closes -- the shape of a real turn whose
/// pod stalled long enough to be reaped.
async fn report_while_the_lease_is_lost(h: &Harness, last: Option<&'static str>) -> StatusCode {
    let (job, lease, _, runtime) = a_leased_turn(h).await;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    // Says when the endpoint first reads the body, and yields nothing itself.
    let read_started = std::sync::Arc::new(tokio::sync::Notify::new());
    let reading = {
        use futures::StreamExt as _;
        let read_started = read_started.clone();
        futures::stream::once(async move {
            read_started.notify_one();
        })
        .filter_map(|()| async { None::<Result<bytes::Bytes, std::io::Error>> })
    };
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from_stream(futures::StreamExt::chain(
            reading,
            tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
        .expect("request");

    let pool = h.db.pool.clone();
    let job_id: Uuid = job.parse().unwrap();
    let feed = async move {
        // Past the endpoint's upfront lease check, then reaped and handed on.
        // The endpoint reads the body only once the check has passed, so its
        // first read is the moment to take the lease -- waited for, where a
        // sleep guessed at it.
        read_started.notified().await;
        sqlx::query("update jobs set lease_token = $2 where id = $1")
            .bind(job_id)
            .bind(Uuid::now_v7())
            .execute(&pool)
            .await
            .expect("hand the lease on");
        if let Some(line) = last {
            tx.send(Ok(bytes::Bytes::from(format!("{line}\n"))))
                .await
                .ok();
        }
        drop(tx);
    };
    let ((status, _), ()) = tokio::join!(h.send(req), feed);
    status
}

/// A turn that finished after its lease was handed on is refused, not a fault.
///
/// Found on a real session: the pod stalled, was reaped, and reported its
/// result once the model finally answered -- and got a 500, which reads as the
/// API being broken rather than as "this turn is someone else's now".
#[tokio::test]
async fn a_report_whose_lease_lapsed_midway_is_refused_cleanly() {
    let h = harness_or_skip!();
    let status = report_while_the_lease_is_lost(
        &h,
        Some(r#"{"kind":"done","content":"late","prompt_tokens":1,"completion_tokens":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// The same, where the turn failed rather than finished -- and nothing is said
/// about the failure, because the session is now another pod's to speak for.
#[tokio::test]
async fn a_failed_turn_whose_lease_lapsed_says_nothing_to_the_session() {
    let h = harness_or_skip!();
    let before: i64 = sqlx::query_scalar("select count(*) from events where kind = 'chat.error'")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");

    let status = report_while_the_lease_is_lost(&h, None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let after: i64 = sqlx::query_scalar("select count(*) from events where kind = 'chat.error'")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        after, before,
        "a pod that no longer holds the turn told its reader it had failed"
    );
}

/// A lease that was reaped is refused on every channel a running turn
/// reports through, not just at report.
///
/// Reaping returned the job to the queue and left its token in the row, and
/// the endpoints for approvals, sleeps, invocation keys and token refresh
/// compared tokens without asking whether the job was still running -- so a
/// pod whose turn had been taken away could go on parking the session and
/// filling its approvers' queue, and once a second pod claimed the turn,
/// two pods spoke for it.
#[tokio::test]
async fn a_reaped_turn_s_lease_is_refused_on_every_side_channel() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;

    // Claimed with a lease already over, the way a pod that stopped renewing
    // looks to the reaper -- no waiting for one to lapse.
    outturn::jobs::enqueue(
        &h.db.pool,
        workspace,
        "chat.turn",
        serde_json::json!({
            "workspace_id": workspace, "session_id": session,
            "agent_id": agent, "message_id": Uuid::now_v7(),
        }),
        None,
        Some(&session.to_string()),
        outturn::jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");
    let claimed = outturn::jobs::claim(&h.db.pool, &["chat.turn"], 1, std::time::Duration::ZERO)
        .await
        .expect("claim");
    let job = claimed[0].job.id;
    let stale = claimed[0].job.lease_token.expect("lease");
    let (reaped, _) = outturn::jobs::reap_abandoned(&h.db.pool).await.unwrap();
    assert_eq!(reaped, 1);

    let side_channels = |lease: Uuid| {
        let h = &h;
        async move {
            let send = |uri: String, body: Value| async move {
                let req = Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
                    .header(outturn::api::work::LEASE_HEADER, lease.to_string())
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request");
                h.send(req).await
            };
            vec![
                (
                    "gated",
                    send(
                        "/v1/work/gated".into(),
                        serde_json::json!({
                            "session_id": session, "job_id": job, "method": "POST",
                            "host": "api.example.com", "path": "/charges", "body": "{}",
                        }),
                    )
                    .await,
                ),
                (
                    "wait",
                    send(
                        "/v1/work/wait".into(),
                        serde_json::json!({
                            "job_id": job, "session_id": session, "kind": "sleep",
                            "seconds": 600, "reason": "Waiting",
                        }),
                    )
                    .await,
                ),
                (
                    "invocations",
                    send(
                        "/v1/work/invocations/claim".into(),
                        serde_json::json!({ "job_id": job, "key": "k" }),
                    )
                    .await,
                ),
                (
                    "token",
                    send(
                        format!("/v1/work/{job}/token"),
                        serde_json::json!({ "gateway_token": "stale" }),
                    )
                    .await,
                ),
            ]
        }
    };

    // Reaped and back in the queue: nobody holds it, least of all the pod
    // that let it lapse.
    for (name, (status, body)) in side_channels(stale).await {
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{name} took a reaped lease: {body}"
        );
    }

    // Claimed again by another pod: the old token is still refused, and the
    // new holder is the one the conversation now listens to.
    let again = outturn::jobs::claim(
        &h.db.pool,
        &["chat.turn"],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    assert_eq!(again[0].job.id, job);
    let current = again[0].job.lease_token.expect("lease");
    assert_ne!(current, stale);
    for (name, (status, body)) in side_channels(stale).await {
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{name} took a replaced lease: {body}"
        );
    }
    let (status, body) = ask_to_wait(
        &h,
        current,
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "sleep",
            "seconds": 600, "reason": "Waiting for the deposit to clear",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A turn's lease is renewed while its summary is being written.
///
/// Preparing a turn can summarize the conversation, which is a model call
/// that takes as long as the model takes, and the lease is deliberately
/// shorter than a model is allowed to be. With renewal starting only at the
/// runtime's first report, a summary slower than the lease let the reaper
/// hand the turn to a second pod -- which summarized it again -- while this
/// one handed out an assignment under a dead lease, and the runtime ran the
/// whole turn to be refused at report.
///
/// The gateway says when the summary request reaches it, so the lease is
/// made to look lapsed at a moment the turn is known to be mid-summary rather
/// than at a guess. Renewal is then waited for, not timed.
#[tokio::test]
async fn a_turn_s_lease_is_renewed_while_its_summary_is_written() {
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
    // Enough turns that a summary has something to replace: the boundary
    // keeps a tail of recent messages and summarizes only what is before it.
    for _ in 0..outturn::api::chat::summarize::TAIL_MESSAGES {
        say(
            "Hello again. Here is another line of conversation, long enough that \
             a few turns of it outgrow the smallest budget a model may be given.",
        )
        .await;
        let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
        assert_eq!(status, StatusCode::OK, "no work: {body}");
        let assignment = json(&body);
        let job = assignment["job_id"].as_str().unwrap();
        let lease = assignment["lease_token"].as_str().unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/v1/work/{job}/events"))
            .header("authorization", format!("Bearer {runtime}"))
            .header(outturn::api::work::LEASE_HEADER, lease)
            .body(Body::from(
                "{\"kind\":\"done\",\"content\":\"ok\",\"prompt_tokens\":1,\"completion_tokens\":1}\n",
            ))
            .unwrap();
        assert_eq!(h.send(req).await.0, StatusCode::NO_CONTENT);
    }
    assert!(
        gateway.requests().is_empty(),
        "nothing should have been summarized under the default budget"
    );

    // The smallest budget allowed, which the conversation above is well
    // over, so the next turn compacts.
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

    // Taking the turn stalls inside the summary, which the gateway holds.
    let take = h.post("/v1/work", Some(&runtime), "{}");
    tokio::pin!(take);
    tokio::select! {
        _ = &mut take => panic!("the turn was handed out before its summary was written"),
        _ = gateway.held_request_arrived() => {}
    }

    // Mid-summary, the lease is made to look as though it has run out. The
    // heartbeat renews from a spawned task, so it goes on without the poll
    // above being driven; renewal is waited for rather than timed, and
    // never comes if there is no heartbeat.
    let (job, leased_until): (Uuid, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "select id, leased_until from jobs where kind = 'chat.turn' and state = 'running'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("the turn being prepared");
    sqlx::query("update jobs set leased_until = now() - interval '1 second' where id = $1")
        .bind(job)
        .execute(&h.db.pool)
        .await
        .unwrap();
    let renewed = async {
        loop {
            let now_until: chrono::DateTime<chrono::Utc> =
                sqlx::query_scalar("select leased_until from jobs where id = $1")
                    .bind(job)
                    .fetch_one(&h.db.pool)
                    .await
                    .unwrap();
            if now_until >= leased_until {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };
    // A bound no working heartbeat reaches, so a missing one fails rather
    // than hangs.
    tokio::time::timeout(outturn::jobs::LEASE_HEARTBEAT * 10, renewed)
        .await
        .expect("the lease was not renewed while the summary was being written");

    // Renewed, so the reaper leaves it alone and the summary is written once.
    let (reaped, _) = outturn::jobs::reap_abandoned(&h.db.pool).await.unwrap();
    assert_eq!(
        reaped, 0,
        "a turn being prepared was reaped out from under its pod"
    );

    gateway.release_held();
    let (status, body) = take.await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let assignment = json(&body);
    assert_eq!(assignment["job_id"].as_str().unwrap(), job.to_string());
    let lease: Uuid = assignment["lease_token"].as_str().unwrap().parse().unwrap();
    assert!(
        outturn::jobs::holds_lease(&h.db.pool, job, lease)
            .await
            .unwrap(),
        "the assignment was handed out under a lease that is not current"
    );
    assert_eq!(
        gateway.requests().len(),
        1,
        "the conversation was summarized more than once"
    );
}

/// A turn the provider declined is shown as that, live and after a reload,
/// and is not tried again: the same conversation is declined the same way.
#[tokio::test]
async fn a_declined_turn_is_kept_as_a_refusal_and_not_retried() {
    let h = harness_or_skip!();
    let (job, lease, session_id, runtime) = a_leased_turn(&h).await;

    let refusal = serde_json::json!({
        "reason": "This request was blocked.",
        "category": "reasoning_extraction",
        "model": "claude-sonnet-5-5",
    });
    let stream = serde_json::json!({
        "kind": "failed", "message": "This request was blocked.", "terminal": true, "refusal": refusal,
    })
    .to_string();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let kept: Value = sqlx::query_scalar(
        "select metadata->'refused' from agent_messages where session_id = $1 and role = 'user'",
    )
    .bind(session_id)
    .fetch_one(&h.db.pool)
    .await
    .expect("the prompt");
    assert_eq!(kept, refusal, "kept on the prompt for a reader who reloads");

    let said: Value = sqlx::query_scalar(
        "select payload from events where session_id = $1 and kind = 'chat.error'",
    )
    .bind(session_id)
    .fetch_one(&h.db.pool)
    .await
    .expect("the live event");
    assert_eq!(said["refusal"], refusal);

    let state: String = sqlx::query_scalar("select state from jobs where id = $1::uuid")
        .bind(&job)
        .fetch_one(&h.db.pool)
        .await
        .expect("the job");
    assert_eq!(state, "failed", "not put back to be declined again");
}

/// What a reply was signed with is kept on it, and handed back with the next
/// turn's conversation, so the model reads its own past tool calls with the
/// reasoning that made them.
#[tokio::test]
async fn a_signed_reply_is_kept_and_handed_to_the_next_turn() {
    let h = harness_or_skip!();
    let (job, lease, session_id, runtime) = a_leased_turn(&h).await;

    let thinking =
        serde_json::json!({"type": "thinking", "thinking": "Check.", "signature": "c2ln"});
    let stream = [
        serde_json::json!({"kind": "tool", "id": "c1", "name": "get_current_time", "action": "Reading the clock", "arguments": "{}"}),
        serde_json::json!({"kind": "tool_result", "id": "c1", "details": "noon", "content": "noon", "is_error": false}),
        serde_json::json!({"kind": "delta", "idx": 0, "text": "Noon."}),
        serde_json::json!({"kind": "done", "content": "Noon.", "prompt_tokens": 1, "completion_tokens": 1,
                           "cache_read_tokens": 0, "cache_write_tokens": 0, "reasoning_tokens": 0,
                           "model": "claude-sonnet-5-5",
                           "signed": [{"first_call": "c1", "model": "claude-sonnet-5-5", "thinking": [thinking]}]}),
    ]
    .map(|line| line.to_string())
    .join("\n");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let (first_call, stored): (String, Value) =
        sqlx::query_as("select first_call, thinking from reply_signatures where session_id = $1")
            .bind(session_id)
            .fetch_one(&h.db.pool)
            .await
            .expect("kept");
    assert_eq!(first_call, "c1");
    assert_eq!(stored[0], thinking);
    // Kept beside the reply, never in it: a person reading the transcript
    // has the thinking already, and nothing to do with a signature.
    let metadata: Value = sqlx::query_scalar(
        "select metadata from agent_messages where session_id = $1 and role = 'assistant'",
    )
    .bind(session_id)
    .fetch_one(&h.db.pool)
    .await
    .expect("the reply");
    assert!(metadata.get("signed").is_none(), "{metadata}");

    let workspace: Uuid =
        sqlx::query_scalar("select workspace_id from agent_sessions where id = $1")
            .bind(session_id)
            .fetch_one(&h.db.pool)
            .await
            .expect("workspace");
    let admin = h
        .login_as("second@acme.example", None, Some((workspace, "admin")))
        .await;
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"and now?"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let next: Value = serde_json::from_str(&body).expect("assignment");
    assert_eq!(
        next["signed"][0]["first_call"], "c1",
        "the next turn was not handed what the reply was signed with: {body}"
    );
    assert_eq!(next["signed"][0]["thinking"][0], thinking);
}

/// A call held for approval is reported as held, not failed, and what a person
/// reads of it carries neither the platform's mark nor the line telling the
/// model not to retry. The model's own copy keeps both: the retraction once
/// somebody approves matches on them.
#[tokio::test]
async fn a_gated_call_reaches_the_browser_as_waiting_not_failed() {
    let h = harness_or_skip!();
    let (job, lease, session_id, runtime) = a_leased_turn(&h).await;

    // The server quotes the sentence too, so what is cut must be only the
    // gate's own suffix, not every copy of it.
    let refusal = format!(
        "this needs approval before it can go out: POST /charges requires \"charge\" \
         (the server says: Do not retry this request.). Somebody has been asked to \
         approve it, and this conversation will pause until they answer. {}",
        outturn::egress::gate::gated_suffix(),
    );
    // A page that happens to carry the mark is a page, not a held call.
    let page = format!(
        "{{\"status\":200,\"body\":\"{}\"}}",
        outturn::egress::gate::gated_suffix()
    );
    let stream = [
        serde_json::json!({"kind": "tool", "id": "c1", "name": "fetch_url", "action": "Charging", "arguments": "{}"}),
        serde_json::json!({"kind": "tool_result", "id": "c1", "details": refusal, "content": refusal, "is_error": true}),
        serde_json::json!({"kind": "tool", "id": "c2", "name": "fetch_url", "action": "Reading", "arguments": "{}"}),
        serde_json::json!({"kind": "tool_result", "id": "c2", "details": page, "content": page, "is_error": false}),
        // As `fetch_url` reports a failure: the refusal inside its JSON.
        serde_json::json!({"kind": "tool", "id": "c3", "name": "fetch_url", "action": "Charging", "arguments": "{}"}),
        serde_json::json!({"kind": "tool_result", "id": "c3", "is_error": true,
            "details": serde_json::json!({"method": "POST", "url": "http://x.example/charges", "error": refusal}).to_string(),
            "content": serde_json::json!({"method": "POST", "url": "http://x.example/charges", "error": refusal}).to_string()}),
        serde_json::json!({"kind": "done", "content": "", "prompt_tokens": 1, "completion_tokens": 1, "cache_read_tokens": 0, "cache_write_tokens": 0, "reasoning_tokens": 0}),
    ]
    .map(|line| line.to_string())
    .join("\n");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let results: Vec<Value> = sqlx::query_scalar(
        "select payload from events where session_id = $1 and kind = 'chat.tool_result' order by id",
    )
    .bind(session_id)
    .fetch_all(&h.db.pool)
    .await
    .expect("the result events");
    let (held, fetched) = (&results[0], &results[1]);

    assert_eq!(held["gated"], true, "{held}");
    let shown = held["details"].as_str().expect("details");
    assert!(shown.starts_with("this needs approval"), "{shown}");
    assert!(
        shown.ends_with("until they answer."),
        "the suffix stayed: {shown:?}"
    );
    assert!(
        shown.contains("the server says: Do not retry this request."),
        "a quoted copy of the sentence was cut: {shown}"
    );

    assert_eq!(
        fetched["gated"], false,
        "a fetched page posed as a held call: {fetched}"
    );
    assert_eq!(
        fetched["details"].as_str(),
        Some(page.as_str()),
        "a page was rewritten"
    );

    let wrapped = &results[2];
    assert_eq!(
        wrapped["gated"], true,
        "a held call inside JSON was missed: {wrapped}"
    );
    let shown: Value =
        serde_json::from_str(wrapped["details"].as_str().expect("details")).expect("still JSON");
    assert!(
        shown["error"]
            .as_str()
            .unwrap()
            .ends_with("until they answer."),
        "the suffix stayed: {shown}"
    );
    assert_eq!(shown["url"], "http://x.example/charges");
}
