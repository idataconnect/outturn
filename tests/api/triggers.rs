//! Schedules, webhook triggers, deliveries and mail hooks.

use super::*;

// ---------------------------------------------------------------------------
// Schedules
//
// The unit tests underneath these exercise cron arithmetic in isolation, which
// is exactly why they missed what these catch: every bug found in review lived
// in the path between the row and the firing, not in the arithmetic.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_cannot_be_pointed_at_another_workspaces_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let other = h.make_workspace("Other", "other").await;

    let acme_admin = h
        .login_as("sched-a@acme.example", None, Some((acme, "admin")))
        .await;
    let other_admin = h
        .login_as("sched-b@other.example", None, Some((other, "admin")))
        .await;

    // An agent belonging to the other workspace.
    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&other_admin),
            r#"{"name":"Theirs","slug":"theirs"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let theirs: Value = serde_json::from_str(&body).expect("agent json");
    let theirs = theirs["id"].as_str().expect("agent id");

    // Acme's admin naming it. The row's workspace_id comes from the token and
    // the agent_id from the body, so without a check this stores one
    // workspace beside another's agent -- and the firing loop would run it.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&acme_admin),
            &format!(
                r#"{{"agent_id":"{theirs}","name":"Borrowed","prompt":"go","expression":"0 9 * * *","timezone":"UTC"}}"#
            ),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another workspace's agent was accepted: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_round_trips_through_the_database() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-c@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Reporter","slug":"reporter"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent json");
    let agent_id = agent["id"].as_str().expect("agent id");

    // Every column the row carries is written and read back here. The columns
    // are named in hand-written SQL rather than checked by the compiler, so a
    // typo only ever surfaces at runtime -- which is what this is for.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Morning","prompt":"Summarize yesterday","expression":"0 9 * * 1-5","timezone":"Europe/London","account":"acme-ops"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let created: Value = serde_json::from_str(&body).expect("schedule json");
    let id = created["id"].as_str().expect("schedule id").to_string();

    assert_eq!(created["account"], "acme-ops");
    assert_eq!(created["timezone"], "Europe/London");
    assert!(
        created["next_run_at"].is_string(),
        "a new schedule should know when it fires next: {body}"
    );
    assert_eq!(
        created["upcoming"].as_array().expect("upcoming").len(),
        3,
        "the editor's next-firings list came back empty: {body}"
    );

    // Listing, filtered by agent.
    let (status, body) = h
        .get(&format!("/v1/schedules?agent_id={agent_id}"), Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("Morning"), "body: {body}");

    // Moving a schedule to another agent is refused rather than answered with
    // a 200 that did not do it.
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Other","slug":"other-agent"}"#,
        )
        .await;
    let second: Value = serde_json::from_str(&body).expect("agent json");
    let second_id = second["id"].as_str().expect("agent id");

    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/schedules/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"agent_id":"{second_id}","name":"Morning","prompt":"Summarize yesterday","expression":"0 9 * * 1-5","timezone":"Europe/London"}}"#
        )))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a schedule was silently moved between agents: {body}"
    );

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/schedules/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn firing_a_schedule_produces_a_turn_nobody_sent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-d@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Nightly","slug":"nightly"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent json");
    let agent_id = agent["id"].as_str().expect("agent id");

    let (_, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Nightly sweep","prompt":"check things","expression":"0 * * * *","timezone":"UTC","account":"acme-ops"}}"#
            ),
        )
        .await;
    let created: Value = serde_json::from_str(&body).expect("schedule json");
    let id: Uuid = created["id"].as_str().expect("id").parse().expect("uuid");

    // Owed an hour ago, so the firing is late and something was missed. Set
    // directly because waiting for the clock is not a test.
    sqlx::query("update triggers set next_at = now() - interval '3 hours' where id = $1")
        .bind(id)
        .execute(&h.db.pool)
        .await
        .expect("set due");

    let taken = outturn::api::schedule::postgres::take_due(&h.db.pool, chrono::Utc::now())
        .await
        .expect("take_due")
        .expect("a schedule was due");
    let (schedule, owed) = taken;
    assert_eq!(schedule.id, id);
    // The clear destroys the row's own copy, so the owed time has to survive
    // separately -- passing `now` here is what made every firing look punctual
    // and the skip count permanently zero.
    assert!(
        owed < chrono::Utc::now(),
        "the firing this was owed came back as the present"
    );

    // Nothing is due a second time: the take cleared it, which is what stops
    // two API pods firing one schedule.
    assert!(
        outturn::api::schedule::postgres::take_due(&h.db.pool, chrono::Utc::now())
            .await
            .expect("take_due")
            .is_none(),
        "the same schedule was handed out twice"
    );

    finish!(h);
}

// ---------------------------------------------------------------------------
// Webhook triggers
//
// The public half is the only endpoint here reachable by somebody this
// platform never gave a credential to, so most of what is worth testing is
// what it refuses.
// ---------------------------------------------------------------------------

/// Signs a body the way a sender does, for tests that should be accepted.
/// The window a signature is accepted in, read from the code that enforces it
/// rather than written again here: these tests position signatures relative to
/// its edges, and a copy would keep passing while testing a window production
/// no longer uses.
const TOLERANCE: i64 = outturn::api::webhook::TIMESTAMP_TOLERANCE_SECS;

async fn make_trigger(h: &Harness, token: &str, body: &str) -> (String, String) {
    let (status, created) = h.post("/v1/webhook-triggers", Some(token), body).await;
    assert_eq!(status, StatusCode::CREATED, "body: {created}");
    let v: Value = serde_json::from_str(&created).expect("json");
    let path = v["path"].as_str().expect("path").to_string();
    let secret = v["secret"].as_str().expect("secret").to_string();
    (path, secret)
}

async fn deliver(h: &Harness, path: &str, body: &str, headers: &[(&str, &str)]) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/v1/hooks/{path}"))
        .header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let (status, _) = h
        .send(req.body(Body::from(body.to_string())).expect("request"))
        .await;
    status
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_delivery_starts_a_turn_and_an_unsigned_one_does_not() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-a@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Bookings","prompt":"A booking arrived: {{{{body}}}}","scheme":"hmac"}}"#
        ),
    )
    .await;

    let payload = r#"{"event":"booking.created","id":"bk_1"}"#;
    let (sig, ts) = sign(&secret, payload, chrono::Utc::now().timestamp());

    assert_eq!(
        deliver(
            &h,
            &path,
            payload,
            &[("x-outturn-signature", &sig), ("x-outturn-timestamp", &ts)],
        )
        .await,
        StatusCode::ACCEPTED,
        "a correctly signed delivery was not accepted"
    );

    // Unsigned, and otherwise identical.
    assert_eq!(
        deliver(&h, &path, payload, &[]).await,
        StatusCode::NOT_FOUND,
        "an unsigned delivery was accepted"
    );

    // Signed, then the body changed -- which is the whole reason for signing
    // the body rather than sending a token.
    assert_eq!(
        deliver(
            &h,
            &path,
            r#"{"event":"booking.created","id":"bk_TAMPERED"}"#,
            &[("x-outturn-signature", &sig), ("x-outturn-timestamp", &ts)],
        )
        .await,
        StatusCode::NOT_FOUND,
        "a tampered body was accepted"
    );

    // An old signature, correctly computed. Staleness, not replay: replay
    // within the window is permitted and is recorded as such in
    // docs/triggers.md, because nothing here stores which signatures have
    // been seen.
    let old = chrono::Utc::now().timestamp() - 3600;
    let (old_sig, old_ts) = sign(&secret, payload, old);
    assert_eq!(
        deliver(
            &h,
            &path,
            payload,
            &[
                ("x-outturn-signature", &old_sig),
                ("x-outturn-timestamp", &old_ts)
            ],
        )
        .await,
        StatusCode::NOT_FOUND,
        "a delivery signed outside the timestamp window was accepted"
    );

    // The accepted one produced a session with a message nobody sent.
    let (status, body) = h.get("/v1/agent-sessions", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Bookings"),
        "no session was started by the delivery: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_that_exists_is_indistinguishable_from_one_that_does_not() {
    // Otherwise the endpoint is an oracle for which paths are real, and an
    // unauthenticated caller can enumerate a deployment's triggers.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-b@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk2"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, _) = make_trigger(
        &h,
        &token,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Real","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    let real = deliver(&h, &path, "{}", &[]).await;
    let imaginary = deliver(&h, "0123456789abcdef0123456789abcdef", "{}", &[]).await;
    assert_eq!(real, imaginary, "a real path answered differently");
    assert_eq!(real, StatusCode::NOT_FOUND);

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hourly_ceiling_refuses_rather_than_queueing() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-c@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk3"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    // A ceiling of two, so the third is refused.
    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Busy","prompt":"got {{{{body}}}}","scheme":"shared_secret","max_per_hour":2}}"#
        ),
    )
    .await;

    for n in 1..=2 {
        assert_eq!(
            deliver(&h, &path, "{}", &[("x-outturn-token", &secret)]).await,
            StatusCode::ACCEPTED,
            "delivery {n} of 2 was refused"
        );
    }
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &secret)]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the ceiling did not refuse the third delivery"
    );

    // And the refusal is recorded, because a hook dropping traffic silently
    // looks exactly like a sender that stopped sending.
    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 1,
        "the refusal was not counted: {listed}"
    );
    // The secret is never handed back.
    assert!(
        !listed.contains(&secret),
        "the secret was returned to a reader: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trigger_cannot_be_pointed_at_another_workspaces_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let other = h.make_workspace("Other", "other").await;
    let acme_admin = h
        .login_as("hook-d@acme.example", None, Some((acme, "admin")))
        .await;
    let other_admin = h
        .login_as("hook-e@other.example", None, Some((other, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&other_admin),
            r#"{"name":"Theirs","slug":"theirs-hook"}"#,
        )
        .await;
    let theirs: Value = serde_json::from_str(&body).expect("agent");
    let theirs = theirs["id"].as_str().expect("id");

    let (status, body) = h
        .post(
            "/v1/webhook-triggers",
            Some(&acme_admin),
            &format!(r#"{{"agent_id":"{theirs}","name":"Borrowed","prompt":"go"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another workspace's agent was accepted: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_before_the_credential_is_proved_writes_nothing() {
    // Three findings had one cause: recording a refusal before knowing the
    // caller holds the secret. It let anybody with the URL drive row-locked
    // updates on the row every real delivery needs, filled the operator's one
    // diagnostic with a prober's noise, and made the two 404s distinguishable
    // by the 1.3ms the write costs.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-f@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk4"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, _secret) = make_trigger(
        &h,
        &token,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Quiet","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    // Every refusal that happens before verification.
    let stale = (chrono::Utc::now().timestamp() - 9999).to_string();
    for headers in [
        vec![],
        vec![
            ("x-outturn-signature", "sha256=wrong"),
            ("x-outturn-timestamp", "1"),
        ],
        vec![
            ("x-outturn-signature", "sha256=wrong"),
            ("x-outturn-timestamp", stale.as_str()),
        ],
    ] {
        assert_eq!(
            deliver(&h, &path, "{}", &headers).await,
            StatusCode::NOT_FOUND
        );
    }

    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 0,
        "an unauthenticated caller moved the refusal counter: {listed}"
    );
    assert!(
        rows[0]["last_status"].is_null(),
        "an unauthenticated caller wrote the trigger's status: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_trigger_refuses_without_saying_it_exists() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-g@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk5"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Off","prompt":"got {{{{body}}}}","scheme":"shared_secret","enabled":false}}"#
        ),
    )
    .await;

    // Correct credential, disabled trigger: still a 404, and still no write.
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &secret)]).await,
        StatusCode::NOT_FOUND
    );
    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 0,
        "a disabled trigger was written to: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rotated_secret_replaces_the_one_before_it() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-h@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk6"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (_, created) = h
        .post(
            "/v1/webhook-triggers",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Rotating","prompt":"got {{{{body}}}}","scheme":"shared_secret"}}"#
            ),
        )
        .await;
    let v: Value = serde_json::from_str(&created).expect("json");
    let id = v["id"].as_str().expect("id").to_string();
    let path = v["path"].as_str().expect("path").to_string();
    let first = v["secret"].as_str().expect("secret").to_string();

    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &first)]).await,
        StatusCode::ACCEPTED
    );

    let (status, rotated) = h
        .post(
            &format!("/v1/webhook-triggers/{id}/rotate"),
            Some(&token),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {rotated}");
    let second = serde_json::from_str::<Value>(&rotated).expect("json")["secret"]
        .as_str()
        .expect("secret")
        .to_string();
    assert_ne!(first, second, "rotating returned the same secret");

    // The old one stops working and the new one works. Rotating that left the
    // old credential valid would be a rotation that rotated nothing.
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &first)]).await,
        StatusCode::NOT_FOUND,
        "the old secret still worked after rotation"
    );
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &second)]).await,
        StatusCode::ACCEPTED,
        "the new secret did not work"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_still_fires_through_the_shared_trigger_path() {
    // start_turn moved out of schedule/worker.rs into trigger.rs so webhooks
    // could use it. The copy it replaced had three steps missing, found by an
    // earlier review, and the shared version is now the single place where
    // getting this wrong breaks both trigger kinds at once.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-e@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Nightly","slug":"nightly2"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (_, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Sweep","prompt":"check things","expression":"0 * * * *","timezone":"UTC","account":"acme-ops"}}"#
            ),
        )
        .await;
    let created: Value = serde_json::from_str(&body).expect("json");
    let id: Uuid = created["id"].as_str().expect("id").parse().expect("uuid");

    sqlx::query("update triggers set next_at = now() - interval '1 minute' where id = $1")
        .bind(id)
        .execute(&h.db.pool)
        .await
        .expect("set due");

    let (schedule, owed) =
        outturn::api::schedule::postgres::take_due(&h.db.pool, chrono::Utc::now())
            .await
            .expect("take_due")
            .expect("due");

    let session = outturn::api::trigger::start(
        &h.db.pool,
        outturn::api::trigger::Started {
            workspace_id: schedule.workspace_id,
            agent_id: schedule.agent_id,
            title: schedule.name.clone(),
            prompt: schedule.prompt.clone(),
            account: schedule.account.clone(),
            timezone: Some(schedule.timezone.clone()),
            source: outturn::api::trigger::Source::Schedule(schedule.id),
            metadata: serde_json::json!({ "schedule_id": schedule.id }),
        },
    )
    .await
    .expect("start");
    assert!(owed < chrono::Utc::now());

    // All three steps the earlier review found missing from the old copy.
    let (live, account, sched, events): (i64, Option<String>, Option<Uuid>, i64) = sqlx::query_as(
        "select \
           (select count(*) from live_sessions where session_id = $1), \
           (select account from agent_sessions where id = $1), \
           (select trigger_id from agent_sessions where id = $1), \
           (select count(*) from events where session_id = $1 and kind = 'chat.message')",
    )
    .bind(session)
    .fetch_one(&h.db.pool)
    .await
    .expect("query");

    assert_eq!(live, 1, "the autoscaler cannot see this session");
    assert_eq!(
        account.as_deref(),
        Some("acme-ops"),
        "the ledger loses its account label"
    );
    assert_eq!(
        sched,
        Some(schedule.id),
        "the session forgot which schedule started it"
    );
    assert_eq!(events, 1, "a browser would never see the message arrive");

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_triggered_session_says_what_started_it_after_the_trigger_is_gone() {
    // The link to the schedule is set null when the schedule is deleted, which
    // left a conversation that ran on its own reading afterwards as though a
    // person had opened it.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("origin@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id: Uuid = agent["id"].as_str().expect("id").parse().expect("uuid");
    let (_, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Morning arrivals","prompt":"list them","expression":"0 8 * * *","timezone":"UTC"}}"#
            ),
        )
        .await;
    let schedule: Value = serde_json::from_str(&body).expect("schedule");
    let schedule_id: Uuid = schedule["id"].as_str().expect("id").parse().expect("uuid");

    let session = outturn::api::trigger::start(
        &h.db.pool,
        outturn::api::trigger::Started {
            workspace_id: acme,
            agent_id,
            title: "Morning arrivals".into(),
            prompt: "list them".into(),
            account: None,
            timezone: None,
            source: outturn::api::trigger::Source::Schedule(schedule_id),
            metadata: serde_json::json!({ "schedule_id": schedule_id }),
        },
    )
    .await
    .expect("start");

    let read = |body: String| -> Value {
        serde_json::from_str::<Value>(&body).expect("session")["started_by"].clone()
    };
    let (_, body) = h
        .get(&format!("/v1/agent-sessions/{session}"), Some(&token))
        .await;
    let started = read(body);
    assert_eq!(started["kind"], "schedule");
    assert_eq!(started["id"], schedule_id.to_string());
    assert_eq!(started["name"], "Morning arrivals");

    sqlx::query("delete from triggers where id = $1")
        .bind(schedule_id)
        .execute(&h.db.pool)
        .await
        .expect("delete");
    let (_, body) = h
        .get(&format!("/v1/agent-sessions/{session}"), Some(&token))
        .await;
    let started = read(body);
    assert_eq!(started["kind"], "schedule", "it forgot it was triggered");
    assert!(started["id"].is_null());
    assert!(started["name"].is_null());

    // And a person's own conversation says nothing.
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&token),
            &format!(r#"{{"agent_id":"{agent_id}"}}"#),
        )
        .await;
    assert!(read(body).is_null());

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_ceiling_lets_a_workspace_be_stricter_and_never_looser() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("ws@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as(
            "op@platform.example",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let call = |method: &'static str, uri: &str, token: &str, body: &str| {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .expect("request");
        h.send(req)
    };
    let harness = &h;
    let dictation = |token: String| async move {
        let (_, body) = harness.get("/v1/session", Some(&token)).await;
        serde_json::from_str::<Value>(&body).expect("session")["dictation"].clone()
    };

    // On unless somebody says otherwise, and a workspace may say otherwise.
    assert_eq!(dictation(admin.clone()).await, true);
    let (status, _) = call(
        "PUT",
        "/v1/settings/dictation",
        &admin,
        r#"{"value":"off"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(dictation(admin.clone()).await, false);
    call("DELETE", "/v1/settings/dictation", &admin, "").await;

    // Off at the operator is off everywhere, and cannot be turned back on.
    let (status, _) = call(
        "PUT",
        "/v1/platform/settings/dictation",
        &operator,
        r#"{"value":"off"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call("PUT", "/v1/settings/dictation", &admin, r#"{"value":"on"}"#).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(dictation(admin.clone()).await, false);

    // Nor does a row written before the operator turned it off get through.
    sqlx::query(
        "insert into setting_overrides (workspace_id, agent_id, key, value) \
         values ($1, '00000000-0000-0000-0000-000000000000', 'dictation', '\"on\"')",
    )
    .bind(acme)
    .execute(&h.db.pool)
    .await
    .expect("stale row");
    assert_eq!(dictation(admin.clone()).await, false);
    let (_, body) = h.get("/v1/settings", Some(&admin)).await;
    let view: Vec<Value> = serde_json::from_str(&body).expect("view");
    let row = view
        .iter()
        .find(|e| e["key"] == "dictation")
        .expect("listed");
    assert_eq!(row["value"], "off");
    assert_eq!(row["source"], "operator");

    // Not an agent's to set, and not offered on an agent's page.
    let (_, body) = call(
        "POST",
        "/v1/agents",
        &admin,
        r#"{"name":"Desk","slug":"desk"}"#,
    )
    .await;
    let agent = serde_json::from_str::<Value>(&body).expect("agent")["id"]
        .as_str()
        .expect("id")
        .to_string();
    let (status, _) = call(
        "PUT",
        &format!("/v1/agents/{agent}/settings/dictation"),
        &admin,
        r#"{"value":"off"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, body) = h
        .get(&format!("/v1/agents/{agent}/settings"), Some(&admin))
        .await;
    let view: Vec<Value> = serde_json::from_str(&body).expect("view");
    assert!(!view.iter().any(|e| e["key"] == "dictation"));

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowed_person_cannot_trigger_an_agent_they_were_scoped_away_from() {
    // A trigger is a standing instruction to start sessions with an agent, so
    // it has to clear the bar `sessions::create_session` clears. It did not:
    // `agents:update` is deliberately not narrowed per agent, so creating a
    // schedule or a hook was a way around the narrowing rather than a use of
    // it. Two reviews raised this before it was fixed.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("narrow@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["books", "helpdesk"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&admin),
                &format!(r#"{{"name":"{slug}","slug":"{slug}"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (books, helpdesk) = (ids[0], ids[1]);

    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("narrow@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    h.scopes
        .set(acme, me, &[helpdesk])
        .await
        .expect("set scope");

    // The agent they were given is still reachable both ways.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{helpdesk}","name":"ok","prompt":"go","expression":"0 9 * * *"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, body) = h
        .post(
            "/v1/webhook-triggers",
            Some(&admin),
            &format!(r#"{{"agent_id":"{helpdesk}","name":"ok","prompt":"got {{{{body}}}}"}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // The one they were scoped away from is refused on both.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{books}","name":"sneaky","prompt":"go","expression":"0 9 * * *"}}"#
            ),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a scoped-away agent was given a schedule: {body}"
    );

    let (status, body) = h
        .post(
            "/v1/webhook-triggers",
            Some(&admin),
            &format!(r#"{{"agent_id":"{books}","name":"sneaky","prompt":"got {{{{body}}}}"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a scoped-away agent was given a public endpoint: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_delivery_is_accepted_once_and_refused_after() {
    // A signature stays valid until its timestamp ages out, so without this
    // the same captured request works repeatedly for the whole window -- each
    // acceptance a new session and a new turn against an agent that may act on
    // the world.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("replay@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"replay-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Once","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    let payload = r#"{"event":"booking.created","id":"bk_once"}"#;
    let (sig, ts) = sign(&secret, payload, chrono::Utc::now().timestamp());
    let headers = [
        ("x-outturn-signature", sig.as_str()),
        ("x-outturn-timestamp", ts.as_str()),
    ];

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED,
        "the first delivery was not accepted"
    );

    // Byte for byte the same request. It holds the credential, so it is told
    // plainly what happened rather than hidden behind a 404.
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT,
        "the same delivery was accepted twice"
    );
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT
    );

    // Exactly one session, so the replays produced no work.
    let sessions: i64 =
        sqlx::query_scalar("select count(*) from agent_sessions where started_by = 'webhook'")
            .fetch_one(&h.db.pool)
            .await
            .expect("count");
    assert_eq!(sessions, 1, "a replay started a turn");

    // A genuinely different event, signed at the same instant, is not a
    // replay -- the signature binds the body, so this must still get through.
    let other = r#"{"event":"booking.created","id":"bk_two"}"#;
    let (sig2, ts2) = sign(&secret, other, chrono::Utc::now().timestamp());
    assert_eq!(
        deliver(
            &h,
            &path,
            other,
            &[
                ("x-outturn-signature", sig2.as_str()),
                ("x-outturn-timestamp", ts2.as_str())
            ],
        )
        .await,
        StatusCode::ACCEPTED,
        "a different delivery was mistaken for a replay"
    );

    // And a replay does not move the ceiling's counter, which means one thing.
    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 0,
        "a replay was counted against the hourly ceiling: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forgotten_delivery_can_be_sent_again() {
    // Deliveries are remembered only while their signature would still be
    // accepted. Past that the row protects nothing, and the sweep removes it
    // -- so the table stays the size of a few minutes of traffic rather than
    // growing for ever.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sweep@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"sweep-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Sweepable","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    // Signed near the old edge of the tolerance: still inside it, so it is
    // accepted, but its record expires within the window rather than at some
    // point the test would have to wait for.
    let payload = r#"{"event":"sweep"}"#;
    let behind = chrono::Utc::now().timestamp() - (TOLERANCE - 20);
    let (sig, ts) = sign(&secret, payload, behind);
    let headers = [
        ("x-outturn-signature", sig.as_str()),
        ("x-outturn-timestamp", ts.as_str()),
    ];

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT
    );

    // The record is anchored to the signature, so waiting for one to lapse is
    // waiting for both. Moving the clock forward stands in for that wait --
    // and it moves the *record*, which is what the sweep reads, exactly as far
    // as the signature has already aged past its own edge.
    //
    // Deliberately not `expires_at = now() - interval` out of nowhere: that
    // would sweep a record whose signature was still good, which is a state
    // this code cannot produce and a test asserting on it would be asserting
    // on fiction.
    sqlx::query("update webhook_deliveries set expires_at = to_timestamp($1)")
        .bind((behind - 600 + TOLERANCE) as f64)
        .execute(&h.db.pool)
        .await
        .expect("age");
    let gone = outturn::api::webhook::postgres::forget_expired(&h.db.pool)
        .await
        .expect("sweep");
    assert_eq!(gone, 1, "the sweep forgot nothing");

    // Nothing is let through by the sweep. The record is gone, so the replay
    // check no longer refuses it -- but the signature it carries is the thing
    // the record was anchored to, and in production the two lapse together.
    // Here the signature is still inside its window, so this asserts the
    // narrower fact the sweep is responsible for: the row went, and the table
    // does not grow without bound.
    let remembered: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(remembered, 0, "the sweep left the record behind");

    // The name's own claim, asserted rather than implied: once the record is
    // gone the replay check no longer refuses these bytes. Without this the
    // test passes against a `remember` that refuses everything after the
    // first insert, since every other assertion here is about the sweep's SQL.
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED,
        "a forgotten delivery was still refused as a replay"
    );

    // And a signature genuinely past its window is refused whether or not a
    // record survives -- the timestamp check is what bounds replay once the
    // table has forgotten, and it is checked before the digest.
    let stale = r#"{"event":"stale"}"#;
    let (sig2, ts2) = sign(
        &secret,
        stale,
        chrono::Utc::now().timestamp() - TOLERANCE - 100,
    );
    assert_eq!(
        deliver(
            &h,
            &path,
            stale,
            &[
                ("x-outturn-signature", sig2.as_str()),
                ("x-outturn-timestamp", ts2.as_str())
            ],
        )
        .await,
        StatusCode::NOT_FOUND,
        "a signature past its window was accepted"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_the_ceiling_refused_can_be_sent_again() {
    // The replay record is claimed before the ceiling is consulted, so that a
    // replay spends nobody's allowance. The hazard in that ordering is the
    // mirror image: a delivery the ceiling turns away has already claimed its
    // slot, and a 429 is precisely the response that asks a sender to send the
    // same bytes again. Left claimed, that retry comes back 409 and the event
    // is lost -- silently, which is worse than the duplicate turn the record
    // exists to prevent.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("ceiling@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"ceiling-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    // One delivery an hour, so the second is refused by the ceiling.
    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Tight","prompt":"got {{{{body}}}}","max_per_hour":1}}"#
        ),
    )
    .await;

    let first = r#"{"event":"first"}"#;
    let (sig1, ts1) = sign(&secret, first, chrono::Utc::now().timestamp());
    assert_eq!(
        deliver(
            &h,
            &path,
            first,
            &[
                ("x-outturn-signature", sig1.as_str()),
                ("x-outturn-timestamp", ts1.as_str())
            ],
        )
        .await,
        StatusCode::ACCEPTED
    );

    // A different event, refused because the ceiling is spent.
    let second = r#"{"event":"second"}"#;
    let (sig2, ts2) = sign(&secret, second, chrono::Utc::now().timestamp());
    let headers2 = [
        ("x-outturn-signature", sig2.as_str()),
        ("x-outturn-timestamp", ts2.as_str()),
    ];
    assert_eq!(
        deliver(&h, &path, second, &headers2).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Raise the ceiling, as an operator would, and let the sender retry the
    // same signed request. It must be admitted: it never became a turn, so it
    // is not a replay of one.
    sqlx::query("update triggers set max_per_hour = 100 where config->>'path' = $1")
        .bind(&path)
        .execute(&h.db.pool)
        .await
        .expect("raise");

    assert_eq!(
        deliver(&h, &path, second, &headers2).await,
        StatusCode::ACCEPTED,
        "a ceiling-refused delivery was refused as a replay on retry, so the event was lost"
    );

    // Two turns, not one: the retry produced the turn the 429 had denied.
    let sessions: i64 =
        sqlx::query_scalar("select count(*) from agent_sessions where started_by = 'webhook'")
            .fetch_one(&h.db.pool)
            .await
            .expect("count");
    assert_eq!(sessions, 2, "the retried delivery did not start a turn");

    // And now that it *has* become a turn, sending it a third time is a real
    // replay and must be refused.
    assert_eq!(
        deliver(&h, &path, second, &headers2).await,
        StatusCode::CONFLICT,
        "a delivery that became a turn was replayable"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_losing_racer_is_not_undone_by_the_winner_failing() {
    // Two byte-identical deliveries arrive together; one claims the record and
    // the other is refused as a replay. If the claim were keyed on the digest
    // alone, the winner failing afterwards would release the row the loser's
    // refusal rests on -- no turn, a 409 already sent saying one had started,
    // and the event lost. The claimant is what keeps a release to its own row.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("race@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"race-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    // One an hour, so the second delivery is the one the ceiling refuses.
    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Racy","prompt":"got {{{{body}}}}","max_per_hour":1}}"#
        ),
    )
    .await;

    // Spend the allowance, so the next delivery claims its record and is then
    // refused by the ceiling -- the winner-fails half of the race, made
    // deterministic.
    let first = r#"{"event":"first"}"#;
    let (sig1, ts1) = sign(&secret, first, chrono::Utc::now().timestamp());
    assert_eq!(
        deliver(
            &h,
            &path,
            first,
            &[
                ("x-outturn-signature", sig1.as_str()),
                ("x-outturn-timestamp", ts1.as_str())
            ],
        )
        .await,
        StatusCode::ACCEPTED
    );

    let racer = r#"{"event":"racer"}"#;
    let (sig2, ts2) = sign(&secret, racer, chrono::Utc::now().timestamp());
    let headers2 = [
        ("x-outturn-signature", sig2.as_str()),
        ("x-outturn-timestamp", ts2.as_str()),
    ];

    // Claims, then loses to the ceiling, then releases its own row.
    assert_eq!(
        deliver(&h, &path, racer, &headers2).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // The racer released its own row and nothing else: the first delivery
    // became a turn, so its record must survive -- releasing by digest alone
    // would be indistinguishable here, but a release that took the wrong row
    // would leave that first delivery replayable.
    let remembered: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        remembered, 1,
        "the racer did not release its own row, or took one that was not its"
    );

    // Which row survived: the one that became a turn, still unreplayable.
    assert_eq!(
        deliver(
            &h,
            &path,
            first,
            &[
                ("x-outturn-signature", sig1.as_str()),
                ("x-outturn-timestamp", ts1.as_str())
            ],
        )
        .await,
        StatusCode::CONFLICT,
        "the racer's release took the accepted delivery's record with it"
    );

    // And the racer's own bytes are retryable, which is what the release is
    // for: raise the ceiling and the event it was refused for still lands.
    sqlx::query("update triggers set max_per_hour = 100 where config->>'path' = $1")
        .bind(&path)
        .execute(&h.db.pool)
        .await
        .expect("raise");
    assert_eq!(
        deliver(&h, &path, racer, &headers2).await,
        StatusCode::ACCEPTED,
        "the racer could not be retried after its claim was released"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_secret_sender_may_repeat_a_payload() {
    // `shared_secret` binds no time, so the only thing to key a replay record
    // on is the token and the body -- which makes a heartbeat, a retry, or the
    // same event reported twice indistinguishable from a replay. It would also
    // buy nothing: the credential travels on every delivery and never expires,
    // so anyone holding it mints fresh requests rather than resending an old
    // one. Refusing real traffic to defend against nothing is the trade this
    // pins shut.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("repeat@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"repeat-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Repeats","prompt":"got {{{{body}}}}","scheme":"shared_secret"}}"#
        ),
    )
    .await;

    let payload = r#"{"event":"heartbeat"}"#;
    for attempt in 1..=3 {
        assert_eq!(
            deliver(&h, &path, payload, &[("x-outturn-token", &secret)]).await,
            StatusCode::ACCEPTED,
            "an identical shared_secret payload was refused on attempt {attempt}"
        );
    }

    // And nothing was written to the replay table, which is what keeps the
    // sweep bounded by hmac traffic alone.
    let remembered: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        remembered, 0,
        "a shared_secret delivery was recorded, so identical payloads will collide"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_future_dated_signature_is_remembered_until_it_stops_verifying() {
    // The window is checked with `abs`, so a signature stamped in the future
    // verifies now *and* keeps verifying until its own timestamp plus the
    // tolerance. Anchoring the record to arrival instead would sweep it while
    // the signature it refuses is still good -- and the replay walks in behind
    // the sweep. The record must outlive the credential, not the request.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("ahead@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"ahead-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Ahead","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    // Stamped near the far edge of the tolerance, which still verifies.
    let ahead = chrono::Utc::now().timestamp() + (TOLERANCE - 60);
    let payload = r#"{"event":"ahead"}"#;
    let (sig, ts) = sign(&secret, payload, ahead);
    let headers = [
        ("x-outturn-signature", sig.as_str()),
        ("x-outturn-timestamp", ts.as_str()),
    ];

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED
    );

    // The record must still be there after an arrival-anchored one would have
    // gone, because the signature is still being accepted.
    let gone = outturn::api::webhook::postgres::forget_expired(&h.db.pool)
        .await
        .expect("sweep");
    assert_eq!(gone, 0, "a still-valid signature was forgotten");

    let expires: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select expires_at from webhook_deliveries")
            .fetch_one(&h.db.pool)
            .await
            .expect("expiry");
    // Strictly past what arrival-anchoring would give. Signed at now+240 with
    // a 300s tolerance, the signature is good until now+540; an arrival
    // anchor would expire at now+300. Asserting past 300 is what makes the
    // two distinguishable -- an earlier version compared against 240, which
    // both satisfy, and so pinned nothing.
    let floor = chrono::Utc::now().timestamp() + TOLERANCE;
    assert!(
        expires.timestamp() > floor,
        "expiry {} is not past the arrival-anchored {floor}, so the record dies before the signature",
        expires.timestamp()
    );

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT,
        "a future-dated signature was replayable"
    );

    finish!(h);
}

/// A sender that names its deliveries has each event run once: the same id
/// again, freshly signed so it is no replay, is refused as already accepted,
/// and another id starts its own turn. A sender that names nothing is
/// unaffected -- every test above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_named_twice_starts_one_turn() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-dedupe@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk-dedupe"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");
    let (path, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Bookings","prompt":"A booking: {{{{body}}}}","scheme":"hmac"}}"#
        ),
    )
    .await;

    let send = |id: &'static str, at: i64| {
        let (sig, ts) = sign(&secret, r#"{"id":"bk_1"}"#, at);
        let h = &h;
        let path = path.clone();
        async move {
            deliver(
                h,
                &path,
                r#"{"id":"bk_1"}"#,
                &[
                    ("x-outturn-signature", &sig),
                    ("x-outturn-timestamp", &ts),
                    ("x-outturn-delivery", id),
                ],
            )
            .await
        }
    };
    let now = chrono::Utc::now().timestamp();
    assert_eq!(send("evt_1", now).await, StatusCode::ACCEPTED);
    assert_eq!(
        send("evt_1", now - 1).await,
        StatusCode::CONFLICT,
        "the same event ran twice"
    );
    assert_eq!(send("evt_2", now - 2).await, StatusCode::ACCEPTED);

    let started: i64 =
        sqlx::query_scalar("select count(*) from agent_sessions where started_by = 'webhook'")
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    assert_eq!(started, 2);
    finish!(h);
}

/// A schedule's firing is named by the time it was due, so the same firing
/// taken twice -- two pods, a retry -- starts one turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_firing_taken_twice_starts_one_turn() {
    use outturn::api::trigger::{Fired, Firer, Firing, Source, fire};
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-dedupe@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk-sched"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id: Uuid = agent["id"].as_str().unwrap().parse().unwrap();
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Morning","prompt":"Report.","expression":"0 9 * * *"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let schedule: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let firer = Firer {
        id: schedule,
        workspace_id: acme,
        agent_id,
        name: "Morning".into(),
        prompt: "Report.".into(),
        account: None,
        source: Source::Schedule(schedule),
    };
    let firing = || Firing {
        text: None,
        external_id: Some("2026-10-19T09:00:00+00:00".into()),
        timezone: None,
        metadata: serde_json::json!({}),
    };
    assert!(matches!(
        fire(&h.db.pool, &firer, firing()).await,
        Fired::Started(_)
    ));
    assert!(matches!(
        fire(&h.db.pool, &firer, firing()).await,
        Fired::Duplicate
    ));
    finish!(h);
}

/// A trigger's turns are bounded at once as well as per hour: past its
/// maximum unfinished, a firing is refused and not counted against the hour,
/// and once one finishes the next goes ahead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trigger_runs_no_more_turns_at_once_than_it_may() {
    use outturn::api::trigger::{Ceiling, Fired, Firer, Firing, Source, fire};
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-ceiling@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk-ceiling"}"#,
        )
        .await;
    let agent_id: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Often","prompt":"Report.","expression":"* * * * *"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let schedule: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query("update triggers set max_concurrent = 2 where id = $1")
        .bind(schedule)
        .execute(&h.db.pool)
        .await
        .unwrap();

    let firer = Firer {
        id: schedule,
        workspace_id: acme,
        agent_id,
        name: "Often".into(),
        prompt: "Report.".into(),
        account: None,
        source: Source::Schedule(schedule),
    };
    let firing = |n: u32| Firing {
        text: None,
        external_id: Some(format!("2026-10-19T09:0{n}:00+00:00")),
        timezone: None,
        metadata: serde_json::json!({}),
    };
    assert!(matches!(
        fire(&h.db.pool, &firer, firing(1)).await,
        Fired::Started(_)
    ));
    assert!(matches!(
        fire(&h.db.pool, &firer, firing(2)).await,
        Fired::Started(_)
    ));
    assert!(matches!(
        fire(&h.db.pool, &firer, firing(3)).await,
        Fired::OverCeiling(Ceiling::Concurrent)
    ));
    let counted: i32 = sqlx::query_scalar("select window_count from triggers where id = $1")
        .bind(schedule)
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(
        counted, 2,
        "a refused firing is not counted against the hour"
    );

    // One finishes, and the next goes ahead -- the refused one too, its
    // dedupe record never having been written.
    sqlx::query(
        "update jobs set state = 'succeeded' where id = \
         (select id from jobs where workspace_id = $1 and kind = 'chat.turn' order by id limit 1)",
    )
    .bind(acme)
    .execute(&h.db.pool)
    .await
    .unwrap();
    assert!(matches!(
        fire(&h.db.pool, &firer, firing(3)).await,
        Fired::Started(_)
    ));
    finish!(h);
}

/// An agent's triggers in one list, every kind, with what each kind has of
/// its own -- and never a webhook's secret, which is said once, when it is made.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agents_triggers_are_one_list() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("triggers-list@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk-triggers"}"#,
        )
        .await;
    let agent = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(r#"{{"agent_id":"{agent}","name":"Morning","prompt":"Report.","expression":"0 9 * * *"}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, secret) = make_trigger(
        &h,
        &token,
        &format!(
            r#"{{"agent_id":"{agent}","name":"Bookings","prompt":"{{{{body}}}}","scheme":"hmac"}}"#
        ),
    )
    .await;

    let (status, listed) = h
        .get(&format!("/v1/agents/{agent}/triggers"), Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let rows = items(&listed);
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["webhook", "schedule"], "by name: Bookings, Morning");
    assert_eq!(rows[1]["config"]["expression"], "0 9 * * *");
    assert_eq!(rows[0]["max_concurrent"], 4, "the default ceiling");
    assert!(!listed.contains(&secret), "a secret is never listed");
    finish!(h);
}

/// Mail delivered to an email trigger starts a turn from an allowed,
/// authenticated sender -- once, however often the service delivers it -- and
/// starts nothing for anybody else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mail_starts_a_turn_from_an_allowed_sender_once() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("mail@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk-mail"}"#,
        )
        .await;
    let agent = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, created) = h
        .post(
            "/v1/webhook-triggers",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent}","kind":"email","scheme":"path","senders":["@example.com"],"name":"Inbox","prompt":"Mail arrived:\n{{{{body}}}}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let path = serde_json::from_str::<Value>(&created).unwrap()["path"]
        .as_str()
        .unwrap()
        .to_string();

    let mail = |from: &str, id: &str| {
        format!(
            "Authentication-Results: mx.example.net; dmarc=pass\r\nFrom: {from}\r\n\
             Subject: Garden room\r\nMessage-ID: <{id}>\r\n\r\nIs it free on the 24th?\r\n"
        )
    };
    let sessions = || async {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from agent_sessions where workspace_id = $1 and started_by = 'email'",
        )
        .bind(acme)
        .fetch_one(&h.db.pool)
        .await
        .unwrap()
    };

    let status = deliver(&h, &path, &mail("jo@example.com", "m1@example.com"), &[]).await;
    assert!(status.is_success(), "{status}");
    assert_eq!(sessions().await, 1);

    let status = deliver(&h, &path, &mail("jo@example.com", "m1@example.com"), &[]).await;
    assert_eq!(status, StatusCode::OK, "delivered twice, acknowledged");
    assert_eq!(sessions().await, 1, "and run once");

    let status = deliver(
        &h,
        &path,
        &mail("eve@elsewhere.com", "m2@elsewhere.com"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "taken, and not acted on");
    assert_eq!(sessions().await, 1, "a sender not allowed starts nothing");
    finish!(h);
}

/// A mail hook keeps a secret exactly when its scheme checks one: none for its
/// address alone, one made -- and shown once -- when it moves to a scheme that
/// checks one, and none again when it moves back. Its secret rotates while it
/// has one, and rotating one reached by its address alone is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mail_hook_keeps_a_secret_only_when_its_scheme_checks_one() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("mail-secret@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk-mail-secret"}"#,
        )
        .await;
    let agent = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let input = |scheme: &str| {
        format!(
            r#"{{"agent_id":"{agent}","kind":"email","scheme":"{scheme}","senders":["*"],"name":"Inbox","prompt":"{{{{body}}}}"}}"#
        )
    };
    let (status, created) = h
        .post("/v1/webhook-triggers", Some(&token), &input("path"))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let created: Value = serde_json::from_str(&created).unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let stored = || async {
        sqlx::query_scalar::<_, String>(
            "select config->>'secret' from triggers where id = $1::uuid",
        )
        .bind(&id)
        .fetch_one(&h.db.pool)
        .await
        .unwrap()
    };
    assert_eq!(stored().await, "", "none kept for the address alone");

    let (status, moved) = h
        .patch(
            &format!("/v1/webhook-triggers/{id}"),
            Some(&token),
            &input("basic"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    let shown = serde_json::from_str::<Value>(&moved).unwrap()["secret"]
        .as_str()
        .expect("the new secret, shown once")
        .to_string();
    assert_eq!(stored().await, shown);

    let rotate = || async {
        h.post(
            &format!("/v1/webhook-triggers/{id}/rotate"),
            Some(&token),
            "",
        )
        .await
    };
    let (status, rotated) = rotate().await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a mail hook's secret rotates: {rotated}"
    );
    let rotated = serde_json::from_str::<Value>(&rotated).unwrap()["secret"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(rotated, shown);
    assert_eq!(stored().await, rotated);

    let (_, back) = h
        .patch(
            &format!("/v1/webhook-triggers/{id}"),
            Some(&token),
            &input("path"),
        )
        .await;
    assert!(
        serde_json::from_str::<Value>(&back)
            .unwrap()
            .get("secret")
            .is_none()
    );
    assert_eq!(stored().await, "", "cleared when nothing checks it");

    let (status, refused) = rotate().await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(stored().await, "", "and none minted by rotating");
    finish!(h);
}
