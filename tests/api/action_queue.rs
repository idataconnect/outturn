//! Requests held for approval and sent on a later turn.

use super::*;

/// A charging skill on an agent with a model, a conversation with a prompt
/// in it, and that prompt's turn taken by a runtime: the setting every
/// held-request test starts from. Returns the session, the job and its lease.
async fn a_turn_that_may_charge(h: &Harness, admin: &str) -> (Uuid, Uuid, Uuid) {
    let charge_file = "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [booking_id, amount_pence]\n---\n\n# charge\n";
    let (status, body) = post_with_cookie(
        h,
        "/v1/skills",
        admin,
        &serde_json::json!({
            "slug": "charging",
            "name": "Charging",
            "body": "Read charge.md before charging anybody.",
            "hosts": ["api.hollowbrook.test"],
            "files": [{ "path": "charge.md", "content": charge_file }],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) =
        post_with_cookie(h, &format!("/v1/skills/{skill}/hosts/approve"), admin, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(admin),
            r#"{"name":"Desk","slug":"desk","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let agent = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent}/skills"))
        .header("cookie", format!("outturn_session={admin}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{skill}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(admin),
            &format!(r#"{{"agent_id":"{agent}","title":""}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session}/messages"),
            Some(admin),
            r#"{"content":"Charge the deposit for bk_1."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (job, lease) = take_turn(h).await;
    (session, job, lease)
}

/// Takes the next turn as a runtime does, returning its job and lease.
async fn take_turn(h: &Harness) -> (Uuid, Uuid) {
    let (job, lease, _) = take_assignment(h).await;
    (job, lease)
}

const HELD_BODY: &str = r#"{"amount_pence":78000,"booking_id":"bk_1","note":"not bound"}"#;

/// Reports the charge refused, the whole request as a runtime relays it.
async fn report_refused_charge(
    h: &Harness,
    session: Uuid,
    job: Uuid,
    lease: Uuid,
    body: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/work/gated")
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "session_id": session,
                "job_id": job,
                "method": "POST",
                "host": "api.hollowbrook.test",
                "path": "/charges",
                "url": "https://api.hollowbrook.test/charges?currency=gbp",
                "headers": [["content-type", "application/json"]],
                "body": body,
                "operation": "charge_payment_account",
            })
            .to_string(),
        ))
        .unwrap();
    h.send(req).await
}

async fn report_held(
    h: &Harness,
    job: Uuid,
    lease: Uuid,
    held: &str,
    outcome: Value,
) -> (StatusCode, String) {
    let mut report = serde_json::json!({ "job_id": job, "held_id": held });
    for (k, v) in outcome.as_object().unwrap() {
        report[k] = v.clone();
    }
    let req = Request::builder()
        .method("POST")
        .uri("/v1/work/held")
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(report.to_string()))
        .unwrap();
    h.send(req).await
}

async fn held_state(h: &Harness, held: &str) -> (String, i32, bool) {
    sqlx::query_as(
        "select state, attempts, spent_at is not null from held_requests where id = $1::uuid",
    )
    .bind(held)
    .fetch_one(&h.db.pool)
    .await
    .expect("held row")
}

/// What a person approves is what is sent. The refused request is held whole,
/// beside the item rather than in it; approving it hands it to the next turn
/// to send, under a token of its own; and the answer spends it, once.
#[tokio::test]
async fn an_approved_request_is_held_and_handed_to_the_next_turn_to_send() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (session, job, lease) = a_turn_that_may_charge(&h, &admin).await;

    let (status, body) = report_refused_charge(&h, session, job, lease, HELD_BODY).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = serde_json::from_str::<Value>(&body).unwrap()["item_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Held beside the item, and not in what every target of it may read.
    let (url, held_body): (String, Option<String>) = sqlx::query_as(
        "select url, body from held_requests where item_id = $1::uuid and state = 'held'",
    )
    .bind(&item)
    .fetch_one(&h.db.pool)
    .await
    .expect("held");
    assert_eq!(url, "https://api.hollowbrook.test/charges?currency=gbp");
    assert_eq!(held_body.as_deref(), Some(HELD_BODY));
    let (_, queue) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    assert!(!queue.contains("not bound"), "{queue}");

    // The turn parks, as it does at its next boundary; then somebody says yes.
    outturn::jobs::park(&h.db.pool, job, Some(lease))
        .await
        .expect("park");
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // No grant for the turn: the request's own travels with its send alone.
    let grants: i64 = sqlx::query_scalar("select count(*) from approval_grants")
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(grants, 0);

    // The resumed turn is handed the request, as it was held, and a token to
    // send it under whose gates are not the turn's.
    let (job, lease, assignment) = take_assignment(&h).await;
    let held = &assignment["held"];
    assert_eq!(held.as_array().map(Vec::len), Some(1), "{assignment}");
    assert_eq!(
        held[0]["url"],
        "https://api.hollowbrook.test/charges?currency=gbp"
    );
    assert_eq!(held[0]["body"], HELD_BODY);
    assert_eq!(held[0]["operation"], "charge_payment_account");
    assert!(assignment["held_token"].is_string(), "{assignment}");
    assert_ne!(
        assignment["held_gates"], assignment["gates"],
        "the grant must not travel with the guest's own calls"
    );
    let held_id = held[0]["id"].as_str().unwrap().to_string();

    // Answered, it is spent; told who approved it and what came back.
    let (status, body) = report_held(
        &h,
        job,
        lease,
        &held_id,
        serde_json::json!({ "outcome": "answered", "status": 201, "body": "{\"charge\":\"ch_1\"}" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let told = serde_json::from_str::<Value>(&body).unwrap()["told"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(told.starts_with("Approved by "), "{told}");
    assert!(told.contains("ch_1"), "{told}");
    assert_eq!(held_state(&h, &held_id).await, ("spent".into(), 0, true));

    // One answer is what a yes is worth.
    let (status, body) = report_held(
        &h,
        job,
        lease,
        &held_id,
        serde_json::json!({ "outcome": "answered", "status": 201, "body": "again" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    finish!(h);
}

/// A body larger than the platform carries is refused rather than held, and
/// nobody is asked about it.
#[tokio::test]
async fn a_request_too_large_to_hold_is_refused() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (session, job, lease) = a_turn_that_may_charge(&h, &admin).await;

    let huge = format!(
        r#"{{"amount_pence":1,"booking_id":"bk_1","pad":"{}"}}"#,
        "x".repeat(outturn::egress::held::BODY_LIMIT)
    );
    let (status, body) = report_refused_charge(&h, session, job, lease, &huge).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    let (_, queue) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    assert!(items(&queue).is_empty(), "{queue}");

    finish!(h);
}

/// Answers the approval the session's first refusal raised, yes, and hands
/// the resumed turn back: its job, lease, and the held request's id.
async fn approved_and_resumed(h: &Harness, admin: &str) -> (Uuid, Uuid, String) {
    let (session, job, lease) = a_turn_that_may_charge(h, admin).await;
    let (status, body) = report_refused_charge(h, session, job, lease, HELD_BODY).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = serde_json::from_str::<Value>(&body).unwrap()["item_id"]
        .as_str()
        .unwrap()
        .to_string();
    outturn::jobs::park(&h.db.pool, job, Some(lease))
        .await
        .expect("park");
    let (status, body) = post_with_cookie(
        h,
        &format!("/v1/approvals/{item}/answer"),
        admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (job, lease, assignment) = take_assignment(h).await;
    let held = assignment["held"][0]["id"].as_str().unwrap().to_string();
    (job, lease, held)
}

/// One that never left is tried again, three times, and is then put back in
/// front of a person as approved but not sent -- who can have it sent again.
#[tokio::test]
async fn a_request_that_never_left_is_tried_three_times_and_then_asked_about() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (job, lease, held) = approved_and_resumed(&h, &admin).await;
    let never = serde_json::json!({ "outcome": "never_sent", "message": "connection refused" });

    for tries in 1..outturn::egress::held::MAX_ATTEMPTS {
        let (status, body) = report_held(&h, job, lease, &held, never.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("will be tried again"), "{body}");
        assert_eq!(
            held_state(&h, &held).await,
            ("approved".into(), tries, false)
        );
    }
    let (status, body) = report_held(&h, job, lease, &held, never.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("could not be sent"), "{body}");
    assert_eq!(held_state(&h, &held).await.0, "unsent");

    // Asked again, and nothing parked on it.
    let (_, queue) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let followup = items(&queue)
        .into_iter()
        .find(|i| i["payload"]["followup"] == "unsent")
        .unwrap_or_else(|| panic!("no follow-up: {queue}"));
    assert!(
        followup["payload"]["reason"]
            .as_str()
            .unwrap()
            .contains("could not be sent"),
        "{followup}"
    );
    let followup = followup["id"].as_str().unwrap().to_string();

    let (status, body) =
        post_with_cookie(&h, &format!("/v1/approvals/{followup}/resend"), &admin, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(held_state(&h, &held).await, ("approved".into(), 0, false));

    finish!(h);
}

/// One that went and was never answered is not sent again where nothing would
/// dedupe it: a person is asked to check, and can mark it done.
#[tokio::test]
async fn a_request_with_no_answer_is_asked_about_and_can_be_marked_done() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (job, lease, held) = approved_and_resumed(&h, &admin).await;

    let (status, body) = report_held(
        &h,
        job,
        lease,
        &held,
        serde_json::json!({ "outcome": "no_answer", "message": "the connection reset" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("not known"), "{body}");
    assert_eq!(held_state(&h, &held).await, ("unknown".into(), 0, false));

    let (_, queue) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let followup = items(&queue)
        .into_iter()
        .find(|i| i["payload"]["followup"] == "unknown")
        .unwrap_or_else(|| panic!("no follow-up: {queue}"));
    let followup = followup["id"].as_str().unwrap().to_string();

    // Not an approval to answer with the original's endpoint semantics: a
    // decline there is "done" here, and the explicit endpoint says so.
    let (status, body) =
        post_with_cookie(&h, &format!("/v1/approvals/{followup}/done"), &admin, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(held_state(&h, &held).await.0, "done");

    // And a second answer to it is told it was already answered.
    let (status, body) =
        post_with_cookie(&h, &format!("/v1/approvals/{followup}/resend"), &admin, "").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("already"), "{body}");
    assert_eq!(held_state(&h, &held).await.0, "done");

    finish!(h);
}

async fn mark_sending(h: &Harness, job: Uuid, lease: Uuid, held: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/work/held/sending")
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "job_id": job, "held_id": held }).to_string(),
        ))
        .unwrap();
    h.send(req).await
}

/// A pod lost between sending an approved request and reporting it leaves the
/// request marked as going. The turn's retry does not send it again -- nothing
/// would dedupe this one -- and a person is asked to check instead.
#[tokio::test]
async fn a_request_lost_in_flight_is_not_sent_again() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (job, lease, held) = approved_and_resumed(&h, &admin).await;

    let (status, body) = mark_sending(&h, job, lease, &held).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(held_state(&h, &held).await.0, "sending");
    // Once: a second runtime is not told to go too.
    let (status, _) = mark_sending(&h, job, lease, &held).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // The pod is lost before it reports, and the turn goes back to the queue.
    outturn::jobs::release(
        &h.db.pool,
        job,
        std::time::Duration::ZERO,
        outturn::jobs::MAX_RELEASES,
        Some(lease),
    )
    .await
    .expect("release");

    let (job, lease, assignment) = take_assignment(&h).await;
    assert!(
        assignment["held"].as_array().is_none_or(Vec::is_empty),
        "handed out to be sent again: {assignment}"
    );
    assert_eq!(held_state(&h, &held).await, ("unknown".into(), 0, false));
    let (_, queue) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let followup = items(&queue)
        .into_iter()
        .find(|i| i["payload"]["followup"] == "unknown")
        .unwrap_or_else(|| panic!("nobody asked to check: {queue}"));
    assert!(
        followup["payload"]["reason"]
            .as_str()
            .unwrap()
            .contains("lost before it could say"),
        "{followup}"
    );

    // And a report from the lost pod arriving late is refused, not recorded.
    let (status, _) = report_held(
        &h,
        job,
        lease,
        &held,
        serde_json::json!({ "outcome": "answered", "status": 201, "body": "late" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    finish!(h);
}

/// Sending a request again starts a turn to send it, now, rather than waiting
/// for whatever the conversation says next; the turn begins with a note
/// nobody typed, and is handed the request to send before its guest runs.
#[tokio::test]
async fn a_resend_starts_a_turn_that_sends_it() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (job, lease, held) = approved_and_resumed(&h, &admin).await;
    let (status, body) = report_held(
        &h,
        job,
        lease,
        &held,
        serde_json::json!({ "outcome": "no_answer", "message": "the connection reset" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    outturn::jobs::complete(&h.db.pool, job, Some(lease))
        .await
        .expect("complete");

    let (_, queue) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let followup = items(&queue)
        .into_iter()
        .find(|i| i["payload"]["followup"] == "unknown")
        .unwrap_or_else(|| panic!("no follow-up: {queue}"));
    let followup = followup["id"].as_str().unwrap().to_string();

    // Through `/answer`, as a client that knows only approvals sends it.
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{followup}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["resumed"], 1);

    let (_, _, assignment) = take_assignment(&h).await;
    assert_eq!(assignment["held"][0]["id"], held.as_str(), "{assignment}");
    let conversation = assignment["conversation"].to_string();
    assert!(
        conversation.contains("asked for the approved charge"),
        "{conversation}"
    );
    assert!(
        conversation.contains("nobody in the conversation sent it"),
        "framed as the platform's: {conversation}"
    );

    finish!(h);
}

/// Sent, with no refusal in the conversation to put the answer in, the model
/// is told on the turns after as well -- not only on the one that sent it,
/// after which it was left a stale refusal and nothing to explain it.
#[tokio::test]
async fn a_sent_request_is_still_told_on_a_later_turn() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (job, lease, held) = approved_and_resumed(&h, &admin).await;
    let (status, body) = report_held(
        &h,
        job,
        lease,
        &held,
        serde_json::json!({ "outcome": "answered", "status": 201, "body": "{\"charge\":\"ch_1\"}" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    outturn::jobs::complete(&h.db.pool, job, Some(lease))
        .await
        .expect("complete");

    let session: Uuid =
        sqlx::query_scalar("select session_id from held_requests where id = $1::uuid")
            .bind(&held)
            .fetch_one(&h.db.pool)
            .await
            .expect("session");
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session}/messages"),
            Some(&admin),
            r#"{"content":"Did it go through?"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (_, _, assignment) = take_assignment(&h).await;
    let conversation = assignment["conversation"].to_string();
    assert!(
        conversation.contains("A request this conversation was waiting on"),
        "{conversation}"
    );
    assert!(conversation.contains("ch_1"), "{conversation}");

    finish!(h);
}
