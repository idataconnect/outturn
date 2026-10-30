//! Sleep and timers.

use super::*;

// --- sleep and timers -----------------------------------------------------
//
// An agent asking to be woken later. See `api::wake`.

#[tokio::test]
async fn a_sleep_holds_the_conversation_and_asks_its_owner() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    asleep(&h, workspace, agent, session).await;

    let held: Vec<String> = sqlx::query_scalar(
        "select strength from inhibitors where session_id = $1 and held_by = 'sleep'",
    )
    .bind(session)
    .fetch_all(&h.db.pool)
    .await
    .expect("holds");
    assert_eq!(held, vec!["suspended".to_string()]);

    // Served with the transcript, so a tab opened now shows it asleep.
    let (_, body) = get_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/messages"),
        &admin,
    )
    .await;
    let history: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        history["asleep"]["reason"], "Waiting for the deposit to clear",
        "{body}"
    );

    let (_, body) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let queue: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(queue["items"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(queue["items"][0]["kind"], "sleep");
    assert_eq!(
        queue["items"][0]["payload"]["reason"],
        "Waiting for the deposit to clear"
    );

    // Due in ten minutes rather than now.
    let later: bool = sqlx::query_scalar(
        "select run_after > now() + interval '9 minutes' from jobs where kind = 'chat.wake'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("wake job");
    assert!(later, "the wakeup is not due when the sleep ends");

    // Not answerable as an approval: that would lift the hold with no note.
    let item = queue["items"][0]["id"].as_str().unwrap();
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // And one sleep at a time.
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let (status, body) = ask_to_wait(
        &h,
        lease,
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "sleep",
            "seconds": 60, "reason": "again",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    h.db.cleanup().await;
}

/// The runtime speaks for the turn it holds, and only that one.
#[tokio::test]
async fn a_wait_is_refused_for_a_turn_the_runtime_does_not_hold() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let ask = |session: Uuid, seconds: u64| {
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "sleep",
            "seconds": seconds, "reason": "r",
        })
    };

    let (status, _) = ask_to_wait(&h, Uuid::now_v7(), ask(session, 60)).await;
    assert_eq!(status, StatusCode::CONFLICT, "a lease it does not hold");
    let (status, _) = ask_to_wait(&h, lease, ask(Uuid::now_v7(), 60)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "another conversation");
    let (status, _) = ask_to_wait(&h, lease, ask(session, 0)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "no time at all");
    let (status, _) = ask_to_wait(&h, lease, ask(session, 31 * 86_400)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "past the longest");

    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0, "a refused sleep held the conversation anyway");

    h.db.cleanup().await;
}

/// The case the design is for: messages sent while the agent slept are
/// answered by the wake, together, and the note comes after them.
#[tokio::test]
async fn waking_answers_what_arrived_meanwhile_in_one_turn() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    asleep(&h, workspace, agent, session).await;

    for text in ["any news?", "hello?"] {
        let (status, body) = post_with_cookie(
            &h,
            &format!("/v1/agent-sessions/{session}/messages"),
            &admin,
            &serde_json::json!({ "content": text }).to_string(),
        )
        .await;
        assert!(status.is_success(), "{status}: {body}");
    }

    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/wake"),
        &admin,
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["woke"], true);

    let written = notes(&h, session).await;
    assert_eq!(written.len(), 1);
    let (note, metadata) = &written[0];
    assert_eq!(metadata["wake"]["kind"], "sleep");
    assert_eq!(metadata["wake"]["woken_by"], "Test User");
    assert_eq!(metadata["wake"]["answers"].as_array().unwrap().len(), 2);

    // Both taken by the note, which sorts after them.
    let absorbed: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "select id, absorbed_by from agent_messages \
         where session_id = $1 and content in ('any news?', 'hello?') order by id",
    )
    .bind(session)
    .fetch_all(&h.db.pool)
    .await
    .expect("messages");
    assert_eq!(absorbed.len(), 2);
    for (id, by) in &absorbed {
        assert_eq!(*by, Some(*note));
        assert!(id < note, "the note was written before what it answers");
    }

    // One turn answers the note, ahead of background work, and the hold is gone.
    let priority: i32 = sqlx::query_scalar(
        "select priority from jobs where kind = 'chat.turn' and payload->>'message_id' = $1",
    )
    .bind(note.to_string())
    .fetch_one(&h.db.pool)
    .await
    .expect("wake turn");
    assert_eq!(priority, outturn::jobs::PRIORITY_REALTIME);
    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0);

    // The wakeup then comes due and finds nothing to do.
    sqlx::query("update jobs set run_after = now() where kind = 'chat.wake'")
        .execute(&h.db.pool)
        .await
        .expect("due");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::wake::WAKE],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    let actions = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    outturn::api::wake::fire(&h.db.pool, &actions, &claimed[0].job)
        .await
        .expect("fire");
    assert_eq!(notes(&h, session).await.len(), 1, "woken twice");

    // As does a second press.
    let (_, body) = post_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/wake"),
        &admin,
        "{}",
    )
    .await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["woke"], false);

    h.db.cleanup().await;
}

/// Coming due on its own, with nobody waiting, a sleep wakes as background work.
#[tokio::test]
async fn a_sleep_that_runs_its_course_wakes_on_its_own() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    asleep(&h, workspace, agent, session).await;

    sqlx::query("update jobs set run_after = now() where kind = 'chat.wake'")
        .execute(&h.db.pool)
        .await
        .expect("due");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::wake::WAKE],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    let actions = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    outturn::api::wake::fire(&h.db.pool, &actions, &claimed[0].job)
        .await
        .expect("fire");

    let written = notes(&h, session).await;
    assert_eq!(written.len(), 1);
    assert!(written[0].1["wake"]["woken_by"].is_null());
    let priority: i32 = sqlx::query_scalar(
        "select priority from jobs where kind = 'chat.turn' and payload->>'message_id' = $1",
    )
    .bind(written[0].0.to_string())
    .fetch_one(&h.db.pool)
    .await
    .expect("wake turn");
    assert_eq!(priority, outturn::jobs::PRIORITY_BACKGROUND);

    let (_, body) = get_with_cookie(&h, "/v1/action-items/count", &admin).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["count"], 0);

    h.db.cleanup().await;
}

/// A timer pauses nothing, so it holds nothing and answers nothing on
/// anybody's behalf: it only starts a turn when it fires.
#[tokio::test]
async fn a_timer_holds_nothing_and_fires_a_turn() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let at = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let (status, body) = ask_to_wait(
        &h,
        lease,
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "timer",
            "at": at, "reason": "Check the booking was confirmed",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    outturn::jobs::complete(&h.db.pool, job, Some(lease))
        .await
        .expect("complete");

    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0, "a timer held the conversation");

    let (status, _) = post_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/messages"),
        &admin,
        r#"{"content":"unrelated"}"#,
    )
    .await;
    assert!(status.is_success());

    sqlx::query("update jobs set run_after = now() where kind = 'chat.wake'")
        .execute(&h.db.pool)
        .await
        .expect("due");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::wake::WAKE],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    let actions = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    outturn::api::wake::fire(&h.db.pool, &actions, &claimed[0].job)
        .await
        .expect("fire");

    let written = notes(&h, session).await;
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].1["wake"]["kind"], "timer");
    let absorbed: Option<Uuid> =
        sqlx::query_scalar("select absorbed_by from agent_messages where content = 'unrelated'")
            .fetch_one(&h.db.pool)
            .await
            .expect("message");
    assert_eq!(
        absorbed, None,
        "a timer answered a message that has a turn of its own"
    );

    h.db.cleanup().await;
}

/// Setting a timer never replaces one, so the agent is shown the others and can
/// cancel what it no longer wants -- and only in its own conversation.
#[tokio::test]
async fn timers_can_be_listed_and_cancelled_and_only_here() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let ask = |body: Value| ask_to_wait(&h, lease, body);
    let answer = |body: &str| -> Value {
        let message: Value = serde_json::from_str(body).expect("json");
        serde_json::from_str(message["message"].as_str().expect("message")).expect("inner json")
    };

    let (status, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "timer",
        "seconds": 3600, "reason": "first",
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first = answer(&body);
    assert!(first["other_timers"].as_array().unwrap().is_empty());
    assert!(first["when"].as_str().unwrap().contains(" at "), "{first}");

    let (_, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "timer",
        "seconds": 7200, "reason": "second",
    }))
    .await;
    let second = answer(&body);
    let others = second["other_timers"].as_array().unwrap();
    assert_eq!(
        others.len(),
        1,
        "the first timer was not mentioned: {second}"
    );
    assert_eq!(others[0]["id"], first["id"]);

    let (_, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "list",
    }))
    .await;
    assert_eq!(answer(&body)["timers"].as_array().unwrap().len(), 2);

    // Another conversation's timer is not this one's to cancel.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/agent-sessions",
        &admin,
        &serde_json::json!({ "agent_id": agent, "title": "other" }).to_string(),
    )
    .await;
    let elsewhere: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (other_job, other_lease) = a_running_turn(&h, workspace, agent, elsewhere).await;
    let (status, _) = ask_to_wait(
        &h,
        other_lease,
        serde_json::json!({
            "job_id": other_job, "session_id": elsewhere, "kind": "cancel", "id": first["id"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "cancel", "id": first["id"],
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(answer(&body)["cancelled"]["reason"], "first");

    let (_, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "list",
    }))
    .await;
    let left = answer(&body);
    assert_eq!(left["timers"].as_array().unwrap().len(), 1);
    assert_eq!(left["timers"][0]["reason"], "second");

    // One already firing is past cancelling: its note is on its way.
    let second_id: Uuid = left["timers"][0]["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("update jobs set state = 'running' where id = $1")
        .bind(second_id)
        .execute(&h.db.pool)
        .await
        .expect("firing");
    let (status, _) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "cancel", "id": second_id,
    }))
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a firing timer was cancelled"
    );

    // Twice is not found, rather than a second success.
    let (status, _) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "cancel", "id": first["id"],
    }))
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    h.db.cleanup().await;
}

/// A page says there is another only when there is, and walking `next` visits
/// every row once. `next` used to be set on any page that had rows, so the last
/// page always sent a client back for an empty one.
#[tokio::test]
async fn a_list_says_there_is_more_only_when_there_is() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    for host in ["a.example", "b.example", "c.example"] {
        let (status, body) = h
            .post(
                "/v1/egress-rules",
                Some(&token),
                &serde_json::json!({ "host": host }).to_string(),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let uri = match &after {
            Some(a) => format!("/v1/egress-rules?limit=2&after={a}"),
            None => "/v1/egress-rules?limit=2".to_string(),
        };
        let (_, body) = h.get(&uri, Some(&token)).await;
        let page: Value = serde_json::from_str(&body).expect("page");
        seen.extend(items(&body).into_iter().map(|r| r["host"].clone()));
        pages += 1;
        match page["next"].as_str() {
            Some(next) => after = Some(next.to_string()),
            None => break,
        }
        assert!(pages < 5, "paging never ended: {body}");
    }
    assert_eq!(
        pages, 2,
        "two pages of two for three rows, and no empty third"
    );
    assert_eq!(seen.len(), 3);

    // Exactly a page's worth is one page, not one and an empty one.
    let (_, body) = h.get("/v1/egress-rules?limit=3", Some(&token)).await;
    let page: Value = serde_json::from_str(&body).expect("page");
    assert_eq!(items(&body).len(), 3);
    assert!(
        page["next"].is_null(),
        "a full last page pointed past itself: {body}"
    );
}
