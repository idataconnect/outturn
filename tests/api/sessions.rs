//! Sessions: listing, finding, reading, feedback, compaction and deletion.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_is_one_verdict_per_person_per_reply_or_conversation() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let ana = h
        .login_as("ana@acme.example", None, Some((acme, "admin")))
        .await;
    let bo = h
        .login_as("bo@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post("/v1/agents", Some(&ana), r#"{"name":"Desk","slug":"desk"}"#)
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&ana),
            &format!(r#"{{"agent_id":"{agent_id}"}}"#),
        )
        .await;
    let session: Uuid = serde_json::from_str::<Value>(&body).expect("session")["id"]
        .as_str()
        .expect("id")
        .parse()
        .expect("uuid");

    // A prompt and the reply to it, written as a turn would leave them.
    let (prompt, reply) = (Uuid::now_v7(), Uuid::now_v7());
    for (id, role) in [(prompt, "user"), (reply, "assistant")] {
        sqlx::query(
            "insert into agent_messages (id, session_id, role, content, metadata) \
             values ($1, $2, $3, 'words', '{}')",
        )
        .bind(id)
        .bind(session)
        .bind(role)
        .execute(&h.db.pool)
        .await
        .expect("message");
    }

    let uri = format!("/v1/agent-sessions/{session}/feedback");
    let send = |method: &'static str, uri: String, token: String, body: String| {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body))
            .expect("request");
        h.send(req)
    };

    let (status, _) = send(
        "PUT",
        uri.clone(),
        ana.clone(),
        format!(r#"{{"message_id":"{reply}","verdict":"down","note":"made up a room"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        "PUT",
        uri.clone(),
        ana.clone(),
        r#"{"verdict":"up"}"#.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the whole conversation");

    // Changed in place, not added to.
    send(
        "PUT",
        uri.clone(),
        ana.clone(),
        format!(
            r#"{{"message_id":"{reply}","verdict":"down","note":"charged before confirming"}}"#
        ),
    )
    .await;
    // Somebody else's verdict is their own.
    send(
        "PUT",
        uri.clone(),
        bo.clone(),
        format!(r#"{{"message_id":"{reply}","verdict":"up"}}"#),
    )
    .await;

    // A person's own words are not the agent's to be judged on.
    let (status, _) = send(
        "PUT",
        uri.clone(),
        ana.clone(),
        format!(r#"{{"message_id":"{prompt}","verdict":"down"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, body) = h.get(&uri, Some(&ana)).await;
    let all: Vec<Value> = serde_json::from_str(&body).expect("list");
    assert_eq!(all.len(), 3, "{all:?}");
    let mine_on_reply = all
        .iter()
        .find(|f| f["mine"] == true && f["message_id"] == reply.to_string())
        .expect("ana's verdict on the reply");
    assert_eq!(mine_on_reply["note"], "charged before confirming");
    assert!(
        all.iter()
            .any(|f| f["mine"] == false && f["verdict"] == "up")
    );

    // Withdrawn, and only the reader's own.
    let (status, _) = send(
        "DELETE",
        format!("{uri}?message_id={reply}"),
        ana.clone(),
        String::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h.get(&uri, Some(&bo)).await;
    let left: Vec<Value> = serde_json::from_str(&body).expect("list");
    assert_eq!(
        left.len(),
        2,
        "bo's and ana's whole-conversation verdict remain"
    );

    finish!(h);
}

#[tokio::test]
async fn sessions_list_by_last_activity_and_say_which_are_working() {
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

    let mut ids = Vec::new();
    for _ in 0..3 {
        let (status, body) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        let session: serde_json::Value = serde_json::from_str(&body).expect("session");
        ids.push(session["id"].as_str().expect("id").to_string());
    }

    // The oldest is picked up again, so it belongs at the top however old.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{}/messages", ids[0]),
            Some(&admin),
            r#"{"content":"back again"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let list = |limit: usize, after: Option<String>| {
        let path = match after {
            Some(a) => format!("/v1/agent-sessions?limit={limit}&after={a}"),
            None => format!("/v1/agent-sessions?limit={limit}"),
        };
        let h = &h;
        let admin = &admin;
        async move {
            let (status, body) = h.get(&path, Some(admin)).await;
            assert_eq!(status, StatusCode::OK, "body: {body}");
            serde_json::from_str::<serde_json::Value>(&body).expect("page")
        }
    };

    let page = list(10, None).await;
    let order: Vec<&str> = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|s| s["id"].as_str().expect("id"))
        .collect();
    assert_eq!(order, [ids[0].as_str(), ids[2].as_str(), ids[1].as_str()]);

    // Only the one with a queued turn says it is working.
    let turns: Vec<&serde_json::Value> = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|s| &s["turn"])
        .collect();
    assert_eq!(turns[0], "pending");
    assert!(turns[1].is_null() && turns[2].is_null(), "{turns:?}");

    // The cursor walks the same order a page at a time, skipping nothing.
    let first = list(2, None).await;
    let next = first["next"].as_str().expect("next").to_string();
    let rest = list(2, Some(next.clone())).await;
    assert_eq!(rest["items"][0]["id"], ids[1].as_str());
    assert!(rest["next"].is_null());

    // The session the cursor names becomes active between pages. The cursor
    // holds its old place, so the next page carries on rather than starting
    // again from the top.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{}/messages", ids[2]),
            Some(&admin),
            r#"{"content":"me again"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let rest = list(2, Some(next.clone())).await;
    let ids_after: Vec<&str> = rest["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|s| s["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids_after, [ids[1].as_str()]);

    // And deleted: the page after it is still there.
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/agent-sessions/{}", ids[2]))
        .header("authorization", format!("Bearer {admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert!(status.is_success(), "{status}: {body}");
    let rest = list(2, Some(next)).await;
    assert_eq!(rest["items"][0]["id"], ids[1].as_str());

    // Something that is not a cursor is refused rather than read as none.
    let (status, _) = h
        .get("/v1/agent-sessions?after=nonsense", Some(&admin))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    finish!(h);
}

/// A conversation keeps the system prompt it was given: a skill edited after
/// its first turn reaches a new conversation at once and this one only when it
/// compacts, so every turn sends the same prefix. A different model composes
/// again, since the preamble names the model.
#[tokio::test]
async fn a_conversation_keeps_its_system_prompt_until_it_compacts() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let runtime = h.runtime_token(acme);
    let json = |body: &str| serde_json::from_str::<Value>(body).unwrap();

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"guide","name":"Guide","body":"Say one."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill = json(&body)["id"].as_str().unwrap().to_string();
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
        )
        .await;
    let agent = json(&body)["id"].as_str().unwrap().to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent}/skills"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{skill}"}}]"#)))
        .unwrap();
    assert_eq!(h.send(req).await.0, StatusCode::OK);

    let session = || async {
        let (_, body) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &format!(r#"{{"agent_id":"{agent}","title":""}}"#),
            )
            .await;
        json(&body)["id"].as_str().unwrap().to_string()
    };
    // Sends a message, takes the turn, finishes it, and says what it was given.
    let turn = |session: String| {
        let h = &h;
        let admin = admin.clone();
        let runtime = runtime.clone();
        async move {
            let (status, _) = h
                .post(
                    &format!("/v1/agent-sessions/{session}/messages"),
                    Some(&admin),
                    r#"{"content":"hello"}"#,
                )
                .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
            assert_eq!(status, StatusCode::OK, "no work: {body}");
            let assignment = serde_json::from_str::<Value>(&body).unwrap();
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
            let reply: Uuid = assignment["reply_id"].as_str().unwrap().parse().unwrap();
            let version: Uuid =
                sqlx::query_scalar("select version_id from turn_skills where reply_id = $1")
                    .bind(reply)
                    .fetch_one(&h.db.pool)
                    .await
                    .unwrap();
            (
                assignment["system_prompt"].as_str().unwrap().to_string(),
                version,
            )
        }
    };

    let running = session().await;
    let (first, v1) = turn(running.clone()).await;
    assert!(first.contains("Say one."), "{first}");

    let (status, body) = h
        .post(
            &format!("/v1/skills/{skill}/versions"),
            Some(&admin),
            r#"{"body":"Say two."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // The running conversation: the same prompt, byte for byte, and the turn
    // records the version it was actually given rather than the live one.
    let (second, recorded) = turn(running.clone()).await;
    assert_eq!(
        second, first,
        "the prompt changed under a running conversation"
    );
    assert_eq!(recorded, v1);

    // A new one has the edit at once.
    let (fresh, v2) = turn(session().await).await;
    assert!(fresh.contains("Say two."), "{fresh}");
    assert_ne!(v2, v1);

    // The running conversation says it is behind, and which skill.
    let (_, body) = h
        .get(
            &format!("/v1/agent-sessions/{running}/prompt"),
            Some(&admin),
        )
        .await;
    let status = json(&body);
    assert_eq!(status["current"], false, "{status}");
    assert_eq!(status["skills"][0]["kept"], 1, "{status}");
    assert_eq!(status["skills"][0]["live"], 2, "{status}");

    // Compacting on request takes the new version up. Run the way the API
    // runs it: queued, then claimed by its own loop.
    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{running}/compact"),
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let worker = outturn::api::worker::Worker {
        pool: h.db.pool.clone(),
        agents: h.agents.clone(),
        templates: Arc::new(
            outturn::api::agent_template::PostgresAgentTemplateStore::new(h.db.pool.clone()),
        ),
        skills: h.skills.clone(),
        chat: Arc::new(PostgresChatStore::new(h.db.pool.clone())),
        usage: Arc::new(outturn::api::usage::PostgresUsageStore::new(
            h.db.pool.clone(),
        )),
        settings: Arc::new(outturn::api::settings::PostgresSettingsStore::new(
            h.db.pool.clone(),
        )),
        inhibitors: Arc::new(outturn::inhibitor::PostgresInhibitorStore::new(
            h.db.pool.clone(),
        )),
        actions: Arc::new(outturn::api::actions::PostgresActionStore::new(
            h.db.pool.clone(),
        )),
        minter: None,
        gateway_url: None,
        storage: None,
    };
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::compact::COMPACT],
        1,
        outturn::jobs::DEFAULT_LEASE,
    )
    .await
    .unwrap();
    assert_eq!(claimed.len(), 1);
    worker
        .compact(acme, running.parse().unwrap())
        .await
        .expect("compact");
    let job = &claimed[0].job;
    outturn::jobs::complete(&h.db.pool, job.id, job.lease_token)
        .await
        .unwrap();
    let (_, body) = h
        .get(
            &format!("/v1/agent-sessions/{running}/prompt"),
            Some(&admin),
        )
        .await;
    assert_eq!(json(&body)["current"], true, "{body}");
    let (after, version) = turn(running.clone()).await;
    assert!(after.contains("Say two."), "{after}");
    assert_eq!(version, v2);

    // Another model composes again: the preamble names it. The model is the
    // chain's, so it changes there -- as an upgrade would move it.
    sqlx::query("update models set model = 'other-model' where model = 'test-model'")
        .execute(&h.db.pool)
        .await
        .unwrap();
    let (third, _) = turn(running).await;
    assert!(third.contains("Say two."), "{third}");
    assert!(third.contains("other-model"), "{third}");
}

/// Searching the list narrows it by title, ignoring case and matching part of
/// a word, and pages the way the list does. A typed `%` is a character, not a
/// wildcard.
#[tokio::test]
async fn sessions_can_be_found_by_part_of_their_title() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();
    for title in [
        "Booking the Rose Room",
        "rose garden tour",
        "Invoices for March",
        "Fifty percent: 50% off",
        "500 cranes",
    ] {
        let (status, body) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &serde_json::json!({ "agent_id": agent_id, "title": title }).to_string(),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let titles = |path: String| {
        let h = &h;
        let admin = &admin;
        async move {
            let (status, body) = h.get(&path, Some(admin)).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let page: Value = serde_json::from_str(&body).unwrap();
            let mut found: Vec<String> = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["title"].as_str().unwrap().to_string())
                .collect();
            found.sort();
            (found, page["next"].clone())
        }
    };

    let (found, _) = titles("/v1/agent-sessions?q=ROSE".into()).await;
    assert_eq!(found, ["Booking the Rose Room", "rose garden tour"]);

    let (found, _) = titles("/v1/agent-sessions?q=50%25".into()).await;
    assert_eq!(found, ["Fifty percent: 50% off"], "% is literal");

    let (found, _) = titles("/v1/agent-sessions?q=%20%20".into()).await;
    assert_eq!(found.len(), 5, "blank search is no search");

    let (found, next) = titles("/v1/agent-sessions?q=o&limit=2".into()).await;
    assert_eq!(found.len(), 2);
    let next = next.as_str().expect("more to page through").to_string();
    let (rest, _) = titles(format!("/v1/agent-sessions?q=o&limit=10&after={next}")).await;
    assert_eq!(found.len() + rest.len(), 4, "every title with an o, once");

    h.db.cleanup().await;
}

/// One session can be read on its own, for a page opened on a conversation the
/// paged list has not reached; one that does not exist is not found.
#[tokio::test]
async fn one_session_can_be_read_on_its_own() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"Old one"}}"#),
        )
        .await;
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = h
        .get(&format!("/v1/agent-sessions/{id}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["title"],
        "Old one"
    );

    let (status, _) = h
        .get(
            &format!("/v1/agent-sessions/{}", Uuid::now_v7()),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    h.db.cleanup().await;
}

/// Deleting a conversation takes everything it owns with it.
///
/// The row's delete relied on cascades, and three tables carrying a session
/// id had none: its events, its holds and its grants stayed behind. Jobs
/// carry the id inside their payload, where no cascade reaches, so a wakeup
/// due next week fired against a conversation that was gone and a turn parked
/// on an approval was still waiting to run. Now the holds, the grants and the
/// events cascade (migration 0054), the jobs are cancelled in the delete's
/// own transaction, and the queue item asking about the sleep is settled.
#[tokio::test]
async fn deleting_a_session_leaves_none_of_its_work_behind() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;

    // Asleep: a session hold, a queue item for its owner and a wakeup due later.
    asleep(&h, workspace, agent, session).await;

    // A turn parked on the hold, as one arriving while it slept would be.
    let parked = outturn::jobs::enqueue(
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
    sqlx::query("update jobs set state = 'parked' where id = $1")
        .bind(parked)
        .execute(&h.db.pool)
        .await
        .expect("park");

    // And a grant somebody gave within it.
    sqlx::query(
        "insert into approval_grants \
         (workspace_id, id, session_id, job_id, requires, extent, keyed_on) \
         values ($1, $2, $3, $4, 'charge', 'call', 'whatever')",
    )
    .bind(workspace)
    .bind(Uuid::now_v7())
    .bind(session)
    .bind(parked)
    .execute(&h.db.pool)
    .await
    .expect("grant");

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/agent-sessions/{session}"))
        .header("cookie", format!("outturn_session={admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let count = |sql: &'static str| {
        let pool = h.db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .bind(session)
                .fetch_one(&pool)
                .await
                .expect(sql)
        }
    };
    assert_eq!(
        count("select count(*) from inhibitors where session_id = $1").await,
        0,
        "the session's hold outlived it"
    );
    assert_eq!(
        count("select count(*) from approval_grants where session_id = $1").await,
        0,
        "a grant outlived its session"
    );
    assert_eq!(
        count("select count(*) from events where session_id = $1").await,
        0,
        "the session's events outlived it"
    );
    assert_eq!(
        count("select count(*) from agent_messages where session_id = $1").await,
        0
    );
    let live: i64 = sqlx::query_scalar(
        "select count(*) from jobs where payload->>'session_id' = $1 \
         and state in ('pending', 'parked', 'running')",
    )
    .bind(session.to_string())
    .fetch_one(&h.db.pool)
    .await
    .expect("jobs");
    assert_eq!(live, 0, "work for a deleted session is still queued");
    let open: i64 = sqlx::query_scalar(
        "select count(*) from action_items where payload->>'session_id' = $1 \
         and state = 'pending'",
    )
    .bind(session.to_string())
    .fetch_one(&h.db.pool)
    .await
    .expect("items");
    assert_eq!(
        open, 0,
        "somebody is still being asked about a deleted session"
    );

    // Nothing brings it back: the reaper finds no lease to reap, and neither
    // the wakeup, once due, nor the parked turn is handed out again.
    outturn::jobs::reap_abandoned(&h.db.pool)
        .await
        .expect("reap");
    sqlx::query("update jobs set run_after = now() where kind = 'chat.wake'")
        .execute(&h.db.pool)
        .await
        .expect("due");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::wake::WAKE, "chat.turn"],
        10,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    assert!(
        claimed.is_empty(),
        "a deleted session's work was handed out: {:?}",
        claimed.iter().map(|c| &c.job.kind).collect::<Vec<_>>()
    );

    h.db.cleanup().await;
}
