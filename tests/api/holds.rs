//! Kill switches and holds, and what a held turn leaves behind.

use super::*;

/// A workspace kill switch stops a turn before it runs.
///
/// The enforcement point is turn preparation, not the API edge: a message is
/// always accepted, and what a hold stops is the agent acting on it. So the
/// message is taken, no work is handed out, and the session is latched with
/// what stopped it -- see docs/inhibitors.md.
#[tokio::test]
async fn a_kill_switch_stops_a_turn_and_latches_the_session() {
    use outturn::inhibitor::{InhibitorStore, Scope, Strength, TakeInhibitor};

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let runtime = h.runtime_token(acme);

    // The customer's spend cap trips.
    let inhibitors = outturn::inhibitor::PostgresInhibitorStore::new(h.db.pool.clone());
    inhibitors
        .take(TakeInhibitor {
            scope: Scope::Workspace { workspace_id: acme },
            strength: Strength::Stopped,
            reason: "monthly spend cap reached".into(),
            held_by: "billing-bot".into(),
        })
        .await
        .expect("take");

    // The message is still accepted: what a hold stops is the agent, not the
    // person talking to it.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "the hold refused a message");

    // No work: an idle cluster answers 200 with a null body, and a held one
    // looks the same to a runtime -- which is the point. The runtime is not
    // told why, because it is not the tier that decides.
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body, "null",
        "work was handed out while a kill switch was on: {body}"
    );

    // And the session says why, so the next turn does not read an unexplained
    // silence.
    let latched: Option<String> = sqlx::query_scalar(
        "select stopped_reason from agent_sessions where id = $1 and stopped_at is not null",
    )
    .bind(session_id.parse::<Uuid>().unwrap())
    .fetch_optional(&h.db.pool)
    .await
    .expect("query")
    .flatten();
    assert_eq!(latched.as_deref(), Some("monthly spend cap reached"));
}

/// A held turn says so, as a hold rather than as a failure.
///
/// The refusal happens before a placeholder exists, so with no event the
/// message sits in the transcript with no reply and no indication, and the
/// thread view cannot read `/v1/inhibitors` to work out why. `chat.held`
/// rather than `chat.error`: a failure is retried and a stop is not, and the
/// browser's error path files the turn under failures and discards the reply
/// bubble -- a reader shown that for a deliberate pause is told the system
/// broke. See docs/inhibitors.md.
#[tokio::test]
async fn a_held_turn_is_announced_as_held_rather_than_as_a_failure() {
    use outturn::inhibitor::{InhibitorStore, Scope, Strength, TakeInhibitor};

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let inhibitors = outturn::inhibitor::PostgresInhibitorStore::new(h.db.pool.clone());
    inhibitors
        .take(TakeInhibitor {
            scope: Scope::Workspace { workspace_id: acme },
            strength: Strength::Stopped,
            reason: "monthly spend cap reached".into(),
            held_by: "billing-bot".into(),
        })
        .await
        .expect("take");

    let session_id = h.session_with_a_message(&admin, agent_id, "hello").await;

    // The runtime is offered the work and finds none, which is where the
    // refusal happens.
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from events where session_id = $1 order by id")
            .bind(session_id)
            .fetch_all(&h.db.pool)
            .await
            .expect("events");

    assert!(
        kinds.iter().any(|k| k == "chat.held"),
        "a held turn was never announced: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "chat.error"),
        "a deliberate hold was announced as a failure: {kinds:?}"
    );

    // And it carries the latch's own reason, not merely "stopped".
    let held: serde_json::Value = sqlx::query_scalar(
        "select payload from events where session_id = $1 and kind = 'chat.held' order by id limit 1",
    )
    .bind(session_id)
    .fetch_one(&h.db.pool)
    .await
    .expect("held event");
    assert!(
        held["message"].as_str().unwrap_or("").contains("spend cap"),
        "the hold did not say why: {held}"
    );
    assert_eq!(
        held["resumable"],
        serde_json::json!(false),
        "a stop was announced as resuming by itself: {held}"
    );
}

/// Releasing the hold does not resume anything; a person has to.
///
/// This is what makes it a kill switch rather than a pause with a harsher
/// name. Fifty conversations stopped by one switch need fifty deliberate
/// restarts.
#[tokio::test]
async fn a_released_kill_switch_does_not_resume_by_itself() {
    use outturn::inhibitor::{InhibitorStore, Scope, Strength, TakeInhibitor};

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let inhibitors = outturn::inhibitor::PostgresInhibitorStore::new(h.db.pool.clone());
    let hold = inhibitors
        .take(TakeInhibitor {
            scope: Scope::Workspace { workspace_id: acme },
            strength: Strength::Stopped,
            reason: "cap reached".into(),
            held_by: "billing-bot".into(),
        })
        .await
        .expect("take");

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let runtime = h.runtime_token(acme);
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(body, "null", "the hold let work through: {body}");

    // The customer tops up.
    inhibitors.release(hold.id).await.expect("release");

    // Nothing resumes on its own: the latch outlives the hold.
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(
        body, "null",
        "a released hold resumed a stopped session by itself: {body}"
    );

    // A person saying something clears it, and that turn runs.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"are you there?"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a person could not restart it: {body}"
    );
}

/// Stopping an org and stopping one agent are different powers.
///
/// An operator builds and runs agents, so stopping one is theirs; the
/// workspace switch stops work they may know nothing about, so it is not.
#[tokio::test]
async fn stopping_an_org_needs_more_than_stopping_an_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // An operator may stop an agent.
    let (status, body) = h
        .post(
            &format!("/v1/agents/{agent_id}/stop"),
            Some(&operator),
            r#"{"reason":"looping on the same tool"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an operator could not stop an agent: {body}"
    );

    // But not the whole workspace.
    let (status, body) = h
        .post(
            "/v1/workspace/stop",
            Some(&operator),
            r#"{"reason":"nope"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator stopped the whole workspace: {body}"
    );

    // An admin may.
    let (status, body) = h
        .post(
            "/v1/workspace/stop",
            Some(&admin),
            r#"{"reason":"spend cap"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an admin could not stop the workspace: {body}"
    );

    // Both show up, with their reasons, to anyone who can see agents.
    let (status, body) = h.get("/v1/inhibitors", Some(&operator)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let held = items(&body);
    assert_eq!(held.len(), 2, "{held:?}");
    let reasons: Vec<&str> = held.iter().map(|i| i["reason"].as_str().unwrap()).collect();
    assert!(reasons.contains(&"looping on the same tool"), "{reasons:?}");
    assert!(reasons.contains(&"spend cap"), "{reasons:?}");

    // Releasing the workspace hold needs the wider authority too.
    let workspace_hold = held
        .iter()
        .find(|i| i["scope"]["level"] == "workspace")
        .expect("workspace hold");
    let id = workspace_hold["id"].as_str().unwrap();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/inhibitors/{id}"))
        .header("authorization", format!("Bearer {operator}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator released the workspace's hold: {body}"
    );
}

/// A hold says who holds it as the workspace may be told: one of its own
/// people by name, and the operator's staff as the operator -- even staff who
/// also belong to the workspace, since the capacity they acted in is not kept.
#[tokio::test]
async fn a_hold_names_its_holder_as_the_workspace_may_know_them() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let staff = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    for (who, reason) in [(&staff, "testing"), (&admin, "spend cap")] {
        let (status, body) = h
            .post(
                "/v1/workspace/stop",
                Some(who),
                &format!(r#"{{"reason":"{reason}"}}"#),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let (status, body) = h.get("/v1/inhibitors", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = items(&body);
    let holder = |reason: &str| {
        held.iter()
            .find(|i| i["reason"] == reason)
            .unwrap_or_else(|| panic!("no hold for {reason}: {body}"))["holder"]
            .clone()
    };

    let theirs = holder("testing");
    assert_eq!(theirs["kind"], "operator", "{body}");
    assert!(
        theirs.get("name").is_none(),
        "the operator's staff was named: {body}"
    );

    let ours = holder("spend cap");
    assert_eq!(ours["kind"], "person", "{body}");
    assert_eq!(ours["name"], "Test User", "{body}");
}

/// A hold needs a reason, because the conversation it stops cannot explain
/// itself.
#[tokio::test]
async fn a_hold_without_a_reason_is_refused() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post("/v1/workspace/stop", Some(&admin), r#"{"reason":"   "}"#)
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a blank reason was accepted: {body}"
    );
}

/// The gateway's check sees every level, and reports the strongest.
///
/// It holds a turn token naming a workspace and a session but no agent, so the
/// agent level is resolved from the session row. A hold it could not see is a
/// runaway turn that keeps generating past a cap that has already tripped.
#[tokio::test]
async fn the_gateway_sees_a_hold_at_any_level() {
    use outturn::inhibitor::{
        InhibitorStore, Scope, Strength, TakeInhibitor, decide, postgres::covering_session,
    };

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let inhibitors = outturn::inhibitor::PostgresInhibitorStore::new(h.db.pool.clone());

    // Nothing held: nothing to report.
    assert!(
        covering_session(&h.db.pool, acme, session_id)
            .await
            .expect("query")
            .is_empty()
    );

    // An agent hold, which the gateway can only find through the session.
    inhibitors
        .take(TakeInhibitor {
            scope: Scope::Agent {
                workspace_id: acme,
                agent_id,
            },
            strength: Strength::Suspended,
            reason: "waiting on somebody".into(),
            held_by: "tester".into(),
        })
        .await
        .expect("take");
    let decision = decide(
        covering_session(&h.db.pool, acme, session_id)
            .await
            .expect("query"),
    );
    assert_eq!(decision.verdict, outturn::inhibitor::Verdict::Suspended);

    // A stop anywhere outranks it, and its reason is the one reported.
    inhibitors
        .take(TakeInhibitor {
            scope: Scope::Workspace { workspace_id: acme },
            strength: Strength::Stopped,
            reason: "spend cap reached".into(),
            held_by: "billing-bot".into(),
        })
        .await
        .expect("take");
    let decision = decide(
        covering_session(&h.db.pool, acme, session_id)
            .await
            .expect("query"),
    );
    assert_eq!(decision.verdict, outturn::inhibitor::Verdict::Stopped);
    // The deciding hold's reason, and only it: the suspended one is not what
    // stopped this, so naming it would send somebody to release the wrong hold.
    assert_eq!(decision.why(), "spend cap reached");
}

/// A hold that cuts a turn latches the session, even if it is released first.
///
/// The gateway cuts the stream and the turn ends, but the hold may be gone by
/// the time anybody sends another message -- and a session that was never
/// latched would simply carry on. A stop is supposed to need a person to lift
/// it, so the latch is written when generation stops rather than a turn later.
#[tokio::test]
async fn a_hold_that_cut_a_turn_latches_the_session_even_once_released() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"tell me a long story"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "no work: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
    let lease = assignment["lease_token"].as_str().expect("lease");

    // The turn reports what a gateway-cut turn reports: a partial reply, and
    // the reason the stream ended. No hold is taken here at all -- this is the
    // case where it was taken and released while the turn was still streaming,
    // so there is nothing left in the table to find.
    let stream = [
        r#"{"kind":"delta","idx":0,"text":"Once upon a"}"#,
        r#"{"kind":"done","content":"Once upon a","prompt_tokens":10,"completion_tokens":3,"held":"runaway turn"}"#,
    ]
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

    // Latched, with the reason, though nothing is holding the workspace now.
    let latched: Option<String> = sqlx::query_scalar(
        "select stopped_reason from agent_sessions where id = $1 and stopped_at is not null",
    )
    .bind(session_id.parse::<Uuid>().unwrap())
    .fetch_optional(&h.db.pool)
    .await
    .expect("query")
    .flatten();
    assert_eq!(
        latched.as_deref(),
        Some("runaway turn"),
        "a turn cut by a hold left the session able to carry on"
    );

    // And the latch is what the next turn actually trips over. The message is
    // accepted -- input is never blocked -- so what proves the refusal is that
    // no work is handed out for it.
    //
    // Posted by the runtime rather than a person, because a person's message
    // clears the latch by design: asserting on one would test the restart, not
    // the refusal.
    let another = Uuid::now_v7();
    sqlx::query(
        "insert into agent_messages (id, session_id, role, content, metadata, delivery) \
         values ($1, $2, 'user', 'go on', '{}'::jsonb, 'steer')",
    )
    .bind(another)
    .bind(session_id.parse::<Uuid>().unwrap())
    .execute(&h.db.pool)
    .await
    .expect("insert");
    outturn::jobs::enqueue(
        &h.db.pool,
        acme,
        "chat.turn",
        serde_json::json!({
            "workspace_id": acme,
            "session_id": session_id.parse::<Uuid>().unwrap(),
            "agent_id": agent_id.parse::<Uuid>().unwrap(),
            "message_id": another,
        }),
        None,
        Some(&format!("session:{session_id}")),
        outturn::jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body, "null",
        "a latched session handed out work to a message no person sent: {body}"
    );
}

/// A turn a hold cut and that then failed latches too.
///
/// A failure and a stop both end a turn without a reply, and the difference
/// matters: a failure is retried, and a retry that finds the hold released
/// would run the work the hold existed to prevent.
#[tokio::test]
async fn a_held_turn_that_then_failed_still_latches() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"tell me a long story"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
    let lease = assignment["lease_token"].as_str().expect("lease");

    // The stream was cut by a hold, and then the guest fell over while tidying
    // up -- so what the runtime reports is a failure carrying the reason.
    let stream = r#"{"kind":"failed","message":"guest trapped","held":"runaway turn"}"#;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, _) = h.send(req).await;
    // The turn failed, so the report is not a success -- but the latch is
    // written regardless, which is the point.
    assert_ne!(
        status,
        StatusCode::OK,
        "a failed turn reported as succeeding"
    );

    let latched: Option<String> = sqlx::query_scalar(
        "select stopped_reason from agent_sessions where id = $1 and stopped_at is not null",
    )
    .bind(session_id.parse::<Uuid>().unwrap())
    .fetch_optional(&h.db.pool)
    .await
    .expect("query")
    .flatten();
    assert_eq!(
        latched.as_deref(),
        Some("runaway turn"),
        "a held turn that failed left the session able to carry on"
    );
}

/// An ordinary turn leaves the session alone.
#[tokio::test]
async fn a_turn_that_finished_normally_does_not_latch() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
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

    let stopped: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select stopped_at from agent_sessions where id = $1")
            .bind(session_id.parse::<Uuid>().unwrap())
            .fetch_one(&h.db.pool)
            .await
            .expect("query");
    assert!(stopped.is_none(), "an ordinary turn latched the session");
}
