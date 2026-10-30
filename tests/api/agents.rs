//! Agents: managing them, who sees which, the roster and its activity.

use super::*;

#[tokio::test]
async fn operator_can_manage_agents_in_own_workspace() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Support","slug":"support","system_prompt":"Be helpful."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, body) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Support"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn agents_are_invisible_across_workspaces() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    let acme_agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Acme Secret".into(),
                slug: "acme-secret".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("create");

    // A user in Globex must not see or reach Acme's agent, even knowing its id.
    let token = h
        .login_as("user@globex.example", None, Some((globex, "admin")))
        .await;

    let (status, body) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("Acme Secret"), "leaked: {body}");

    let (status, _) = h
        .get(&format!("/v1/agents/{}", acme_agent.id), Some(&token))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another workspace's agent must read as absent"
    );

    finish!(h);
}

#[tokio::test]
async fn system_admin_sees_only_the_workspace_they_are_scoped_to() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    h.agents
        .create(
            globex,
            outturn::api::agent::CreateAgent {
                name: "Globex Bot".into(),
                slug: "globex-bot".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("create");

    // Scoped to Acme: system_admin is not a license to see every workspace at
    // once, it is the ability to mint a token for any of them.
    let token = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (status, body) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("Globex Bot"),
        "scoped token must not cross workspaces: {body}"
    );

    finish!(h);
}

#[tokio::test]
async fn viewer_cannot_create_agents() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
        .await;

    let (status, _) = h
        .post("/v1/agents", Some(&token), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Reading is still allowed.
    let (status, _) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);

    finish!(h);
}

#[tokio::test]
async fn operator_cannot_delete_agents() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Keeper".into(),
                slug: "keeper".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("create");

    let token = h
        .login_as("op2@acme.example", None, Some((acme, "operator")))
        .await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/agents/{}", agent.id))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn slugs_are_unique_per_workspace_not_globally() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    let make = || outturn::api::agent::CreateAgent {
        name: "Support".into(),
        slug: "support".into(),
        description: String::new(),
        system_prompt: String::new(),
        policy: None,
    };

    h.agents.create(acme, make()).await.expect("acme");
    // Two workspaces may both have a "support" agent without colliding.
    h.agents.create(globex, make()).await.expect("globex");

    // But not twice within one workspace.
    let dup = h.agents.create(acme, make()).await;
    assert!(
        dup.is_err(),
        "duplicate slug within a workspace must be refused"
    );

    finish!(h);
}

#[tokio::test]
async fn partial_update_leaves_other_fields_intact() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Original".into(),
                slug: "original".into(),
                description: "keep me".into(),
                system_prompt: "keep this too".into(),
                policy: None,
            },
        )
        .await
        .expect("create");

    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/agents/{}", agent.id))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"Renamed"}"#))
        .expect("request");

    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("Renamed"), "body: {body}");
    assert!(body.contains("keep me"), "description must survive: {body}");
    assert!(
        body.contains("keep this too"),
        "prompt must survive: {body}"
    );

    finish!(h);
}

/// An agent's policy can be set when it is created, not only afterwards.
///
/// Everything in the policy affects the very first turn: which model runs it,
/// which route serves it, and whether the model deliberates before answering.
/// Creating an agent without one meant every agent made through the product
/// ran with thinking on, which on a local model is half a minute of silence
/// before the first visible character — and the only cure was an UPDATE.
#[tokio::test]
async fn an_agent_can_be_created_with_a_policy() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Quick","slug":"quick","policy":{"reasoning_effort":"none"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(
        body.contains(r#""reasoning_effort":"none""#),
        "the policy was dropped on the way in: {body}"
    );

    // And an agent created without one still gets a usable empty policy
    // rather than null, which the turn path reads with `.get`.
    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Plain","slug":"plain"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(body.contains(r#""policy":{}"#), "body: {body}");
}

#[tokio::test]
async fn the_roster_says_which_agents_a_caller_may_talk_to() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;
    let viewer = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
        .await;

    let make = |slug: &'static str| {
        let h = &h;
        let admin = &admin;
        async move {
            let body = format!(r#"{{"name":"{slug}","slug":"{slug}"}}"#);
            let (_, made) = h.post("/v1/agents", Some(admin), &body).await;
            serde_json::from_str::<serde_json::Value>(&made).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap()
        }
    };
    let billing = make("billing").await;
    let support = make("support").await;
    // Accepts a session and then fails its first turn, so it is offered to
    // nobody, whoever they are.
    let retired = make("retired").await;
    let (status, body) = h
        .send(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/agents/{retired}"))
                .header("authorization", format!("Bearer {admin}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":false}"#))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let chat_with = |token: String| {
        let h = &h;
        async move {
            let (status, body) = h.get("/v1/agents", Some(&token)).await;
            assert_eq!(status, StatusCode::OK, "body: {body}");
            let listed = items(&body);
            // The whole roster either way: narrowing hides conversations, not
            // which agents exist.
            assert_eq!(listed.len(), 3);
            listed
                .iter()
                .filter(|a| a["can_chat"] == true)
                .map(|a| a["id"].as_str().unwrap().parse::<Uuid>().unwrap())
                .collect::<Vec<_>>()
        }
    };

    // What the roster offers is what starting a conversation allows, agent by
    // agent -- the point of the field is that the two cannot disagree. Asked
    // before narrowing and again after, since each refuses for its own reason.
    let agrees = |token: String| {
        let h = &h;
        let chat_with = &chat_with;
        async move {
            let offered = chat_with(token.clone()).await;
            for agent in [billing, support, retired] {
                let body = format!(r#"{{"agent_id":"{agent}","title":"t"}}"#);
                let (status, made) = h.post("/v1/agent-sessions", Some(&token), &body).await;
                assert_eq!(
                    status == StatusCode::CREATED,
                    offered.contains(&agent),
                    "agent {agent}: offered {}, starting one answered {status}: {made}",
                    offered.contains(&agent),
                );
            }
        }
    };
    agrees(operator.clone()).await;

    let mut both = chat_with(operator.clone()).await;
    both.sort();
    let mut expected = vec![billing, support];
    expected.sort();
    assert_eq!(both, expected, "unnarrowed, every enabled agent is offered");

    let op_id: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("op@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the operator's account");
    h.scopes
        .set(acme, op_id, &[billing])
        .await
        .expect("set scope");
    assert_eq!(chat_with(operator.clone()).await, vec![billing]);
    agrees(operator).await;

    // Somebody who may not start conversations at all is offered none.
    assert!(chat_with(viewer).await.is_empty());
}

#[tokio::test]
async fn agent_activity_counts_live_turns_per_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let mut agents = Vec::new();
    for slug in ["busy", "quiet"] {
        let (status, body) = h
            .post(
                "/v1/agents",
                Some(&admin),
                &format!(
                    r#"{{"name":"{slug}","slug":"{slug}","policy":{{"model":"test-model"}}}}"#
                ),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
        agents.push(agent["id"].as_str().expect("id").to_string());
    }

    // Two conversations with the busy agent, each with a turn queued.
    let mut busy_sessions = Vec::new();
    for _ in 0..2 {
        let (status, body) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &format!(r#"{{"agent_id":"{}","title":""}}"#, agents[0]),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        let session: serde_json::Value = serde_json::from_str(&body).expect("session");
        let id = session["id"].as_str().expect("id").to_string();
        let (status, _) = h
            .post(
                &format!("/v1/agent-sessions/{id}/messages"),
                Some(&admin),
                r#"{"content":"hello"}"#,
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        busy_sessions.push(id);
    }

    // A runtime takes one of them.
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, body) = h.get("/v1/agents/activity", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).expect("rows");
    let of = |id: &str| {
        rows.iter()
            .find(|r| r["agent_id"] == id)
            .unwrap_or_else(|| panic!("no row for {id}: {rows:?}"))
    };

    let busy = of(&agents[0]);
    assert_eq!(
        (busy["running"].as_i64(), busy["queued"].as_i64()),
        (Some(1), Some(1))
    );
    assert_eq!(busy["waiting"], 0);
    assert!(
        busy_sessions
            .iter()
            .any(|s| busy["live_session_id"] == s.as_str())
    );
    assert!(busy["last_active_at"].is_string());

    // An agent nobody has spoken to is idle and was never active.
    let quiet = of(&agents[1]);
    assert_eq!(
        (quiet["running"].as_i64(), quiet["queued"].as_i64()),
        (Some(0), Some(0))
    );
    assert!(quiet["live_session_id"].is_null() && quiet["last_active_at"].is_null());

    // A follow-up queued behind a turn still streaming: the session says what
    // the reader can see, which is the turn running, not the one waiting.
    let turn_of = |id: String| {
        let h = &h;
        let admin = &admin;
        async move {
            let (status, body) = h.get("/v1/agent-sessions", Some(admin)).await;
            assert_eq!(status, StatusCode::OK, "body: {body}");
            let page: serde_json::Value = serde_json::from_str(&body).expect("page");
            page["items"]
                .as_array()
                .expect("items")
                .iter()
                .find(|s| s["id"] == id.as_str())
                .expect("listed")["turn"]
                .clone()
        }
    };
    let mut running = None;
    for id in &busy_sessions {
        if turn_of(id.clone()).await == "running" {
            running = Some(id.clone());
        }
    }
    let running = running.expect("one session is running");
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{running}/messages"),
            Some(&admin),
            r#"{"content":"and another thing"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(turn_of(running).await, "running");

    finish!(h);
}
