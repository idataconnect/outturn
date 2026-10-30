//! Narrowing a person to some agents, and what that reaches.

use super::*;

/// A person narrowed to one agent cannot start a conversation with another.
///
/// The roster stays workspace-public -- an administrator has to see what is
/// running -- so what a scope narrows is what an agent has done, and who may
/// talk to it. See docs/authorities.md.
#[tokio::test]
async fn a_narrowed_person_reaches_only_the_agents_they_were_given() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["accounting", "support"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&admin),
                &format!(r#"{{"name":"{slug}","slug":"{slug}","system_prompt":"x"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (accounting, support) = (ids[0], ids[1]);

    // Before anybody narrows them, both are reachable: absence of a scope is
    // the authority behaving as it always did.
    for agent in [accounting, support] {
        let (status, body) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &format!(r#"{{"agent_id":"{agent}","title":"t"}}"#),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "unnarrowed access refused: {body}"
        );
    }

    // Narrowed to accounting.
    let scopes = &h.scopes;
    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("admin@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    scopes
        .set(acme, me, &[accounting])
        .await
        .expect("set scope");

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{accounting}","title":"t"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the named agent was refused: {body}"
    );

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{support}","title":"t"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an agent nobody granted was still reachable: {body}"
    );

    // The roster is not narrowed: an administrator still sees what runs here.
    let (status, body) = h.get("/v1/agents", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let agents = items(&body);
    assert_eq!(agents.len(), 2, "the roster was narrowed too: {body}");

    // And removing the narrowing puts everything back. The notification that
    // drops the cache travels through Postgres, so a moment passes between the
    // write and every pod knowing -- including this one. Polled rather than
    // slept through: what is asserted is that it arrives, not how long it takes.
    scopes.set(acme, me, &[]).await.expect("clear scope");
    // Until it arrives, with no deadline: a drop that never comes hangs here,
    // which is unmistakable, where "within a second" failed a loaded machine.
    // The pause only keeps the loop from hammering the API.
    loop {
        let (status, _) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &format!(r#"{{"agent_id":"{support}","title":"t"}}"#),
            )
            .await;
        if status == StatusCode::CREATED {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// An agent's files are narrowed with its conversations, and your own are
/// still yours.
///
/// A transcript says what was said; an agent's files are what somebody
/// uploaded. Both are what an agent has done, so a scope that hides one has to
/// hide the other -- and the session a person started themselves is theirs to
/// read whoever else may not. See docs/authorities.md.
#[tokio::test]
async fn an_agents_files_are_narrowed_with_it_but_your_own_stay_yours() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Two sessions: one the operator started, one the admin did.
    let body = format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#);
    let (_, made) = h.post("/v1/agent-sessions", Some(&admin), &body).await;
    let theirs = serde_json::from_str::<serde_json::Value>(&made).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, made) = h.post("/v1/agent-sessions", Some(&operator), &body).await;
    let mine = serde_json::from_str::<serde_json::Value>(&made).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let put = |token: &str, session: &str, bytes: &str| {
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/v1/agent-sessions/{session}/files/session/note.txt"
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes.to_string()))
            .expect("request")
    };
    let (status, _) = h.send(put(&admin, &theirs, "theirs")).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = h.send(put(&operator, &mine, "mine")).await;
    assert_eq!(status, StatusCode::CREATED);

    // Narrow the operator to an agent that is not this one.
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"B","slug":"b"}"#)
        .await;
    let other: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let op_id: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("op@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the operator's account");
    h.scopes
        .set(acme, op_id, &[other])
        .await
        .expect("set scope");

    // Somebody else's session with the narrowed-away agent is closed to them.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{theirs}/files/session/note.txt"),
            Some(&operator),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a narrowed person read another session's files: {body}"
    );

    // Their own is not. They put it there.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{mine}/files/session/note.txt"),
            Some(&operator),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "their own files were withheld: {body}"
    );
    assert_eq!(body, "mine");

    // And the admin, narrowed by nobody, still reads both.
    for session in [&theirs, &mine] {
        let (status, body) = h
            .get(
                &format!("/v1/agent-sessions/{session}/files/session/note.txt"),
                Some(&admin),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an unnarrowed reader was refused: {body}"
        );
    }
}

/// Narrowing somebody is an act of administering people, so it needs the
/// authority that grants roles -- and only inside your own workspace.
#[tokio::test]
async fn setting_a_scope_needs_the_authority_that_assigns_roles() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let op_id: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("op@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the operator's account");

    let put = |token: &str, user: Uuid, body: String| {
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/scopes/{user}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("request")
    };

    // An operator builds agents; deciding who may reach them is not theirs.
    let (status, body) = h
        .send(put(
            &operator,
            op_id,
            format!(r#"{{"agents":["{agent_id}"]}}"#),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator narrowed somebody: {body}"
    );

    let (status, body) = h
        .send(put(
            &admin,
            op_id,
            format!(r#"{{"agents":["{agent_id}"]}}"#),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an admin could not narrow: {body}"
    );

    // It reads back, and only the narrowed are listed.
    let (status, body) = h.get("/v1/scopes", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).expect("scopes");
    assert_eq!(rows.len(), 1, "the unnarrowed were listed too: {body}");
    assert_eq!(rows[0]["user_id"], op_id.to_string());
    assert_eq!(rows[0]["agents"][0], agent_id);

    // Somebody outside the workspace cannot be narrowed into it.
    let stranger = Uuid::now_v7();
    let (status, body) = h
        .send(put(
            &admin,
            stranger,
            format!(r#"{{"agents":["{agent_id}"]}}"#),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger was given a scope here: {body}"
    );

    // An agent in no workspace of theirs is refused rather than stored.
    let (status, body) = h
        .send(put(
            &admin,
            op_id,
            format!(r#"{{"agents":["{}"]}}"#, Uuid::now_v7()),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a scope named an agent that does not exist: {body}"
    );

    // And clearing it empties the listing.
    let (status, _) = h
        .send(put(&admin, op_id, r#"{"agents":[]}"#.to_string()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h.get("/v1/scopes", Some(&admin)).await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).expect("scopes");
    assert!(rows.is_empty(), "a cleared scope was still listed: {body}");
}

/// Narrowing holds on writes, not only on reads.
///
/// `scope::is_narrowed` names all four session authorities, and the module doc
/// says a write is narrowed wherever its read is -- but only the reads went
/// through `require_for_agent`, so somebody narrowed to one agent could delete,
/// rename, post into and stop another agent's conversations. Session ids travel
/// in URLs, so "they would have to guess the id" was never the guarantee.
#[tokio::test]
async fn a_narrowed_person_cannot_write_to_an_agent_they_were_not_given() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let owner = h
        .login_as("owner@acme.example", None, Some((acme, "admin")))
        .await;
    let narrowed = h
        .login_as("narrowed@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["accounting", "support"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&owner),
                &format!(r#"{{"name":"{slug}","slug":"{slug}","system_prompt":"x"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (accounting, support) = (ids[0], ids[1]);

    // Somebody else's conversation, with the agent the narrowing excludes.
    // Theirs rather than the caller's, because a person's own session is their
    // own whichever agent it is with -- that exemption is deliberate, and
    // testing against it would prove nothing.
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&owner),
            &format!(r#"{{"agent_id":"{support}","title":"theirs"}}"#),
        )
        .await;
    let theirs: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&owner),
            r#"{"content":"something private"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "owner could not post: {body}");

    // And one of the caller's own, so the feed below has something to return
    // rather than parking for the full long-poll timeout.
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&narrowed),
            &format!(r#"{{"agent_id":"{accounting}","title":"mine"}}"#),
        )
        .await;
    let mine: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.post(
        &format!("/v1/agent-sessions/{mine}/messages"),
        Some(&narrowed),
        r#"{"content":"mine"}"#,
    )
    .await;

    // And one of the caller's own with the agent they are about to lose,
    // started while they still could.
    let mine_with_support = h
        .session_with_a_message(&narrowed, support, "started early")
        .await;

    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("narrowed@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    h.scopes
        .set(acme, me, &[accounting])
        .await
        .expect("set scope");

    // The read was already refused. Stated here so a regression that loosens
    // it fails beside the writes rather than silently.
    let (status, _) = h
        .get(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&narrowed),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the read stopped being narrowed"
    );

    let (status, body) = h
        .send(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/agent-sessions/{theirs}"))
                .header("authorization", format!("Bearer {narrowed}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"renamed by somebody else"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "rename was not narrowed: {body}"
    );

    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&narrowed),
            r#"{"content":"posting into a conversation I cannot read"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "send was not narrowed: {body}"
    );

    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{theirs}/cancel"),
            Some(&narrowed),
            "",
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "cancel was not narrowed: {body}"
    );

    let (status, body) = h
        .send(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/agent-sessions/{theirs}"))
                .header("authorization", format!("Bearer {narrowed}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "delete was not narrowed: {body}"
    );

    // The conversation is still there, and still named what its owner named it.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&owner),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the owner lost their session: {body}"
    );

    // Sending into their own session with the excluded agent is refused too.
    // Having started a thread is not permission to keep using an agent
    // somebody has since narrowed away: the tools, the skills and the
    // workspace's allowance are all still reachable through it, and an
    // exemption here would narrow the roster and nothing else.
    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{mine_with_support}/messages"),
            Some(&narrowed),
            r#"{"content":"still talking to the agent I lost"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an old thread kept an excluded agent reachable: {body}"
    );

    // Reading and stopping that same thread are still theirs. Stopping is the
    // safe direction, and refusing it would leave somebody watching an agent
    // they cannot reach spend the allowance.
    let (status, _) = h
        .get(
            &format!("/v1/agent-sessions/{mine_with_support}/messages"),
            Some(&narrowed),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "their own history stopped being theirs"
    );
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{mine_with_support}/cancel"),
            Some(&narrowed),
            "",
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stopping their own turn was refused"
    );
}

/// The event feed is narrowed the same way the transcript is.
///
/// The feed carries the conversation as it is written -- the same words, a
/// little earlier -- so a narrowing that stopped at the transcript would
/// refuse the history and stream the present. See docs/authorities.md.
#[tokio::test]
async fn the_event_feed_is_narrowed_the_same_way_the_transcript_is() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let owner = h
        .login_as("owner@acme.example", None, Some((acme, "admin")))
        .await;
    let narrowed = h
        .login_as("narrowed@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["accounting", "support"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&owner),
                &format!(r#"{{"name":"{slug}","slug":"{slug}","system_prompt":"x"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (accounting, support) = (ids[0], ids[1]);

    let theirs = h
        .session_with_a_message(&owner, support, "something private")
        .await;
    let mine = h
        .session_with_a_message(&narrowed, accounting, "mine")
        .await;

    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("narrowed@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    h.scopes
        .set(acme, me, &[accounting])
        .await
        .expect("set scope");

    let (status, body) = h
        .get(
            "/v1/events?after=00000000-0000-0000-0000-000000000000&limit=500",
            Some(&narrowed),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let feed: serde_json::Value = serde_json::from_str(&body).expect("events");
    let events = feed["events"].as_array().expect("events array");
    assert!(
        events
            .iter()
            .any(|e| e["session_id"] == serde_json::json!(mine.to_string())),
        "the caller's own events were filtered out too: {body}"
    );
    assert!(
        !events
            .iter()
            .any(|e| e["session_id"] == serde_json::json!(theirs.to_string())),
        "an agent the caller was never given streamed through the feed: {body}"
    );

    // The cursor moves over what was filtered out. Left where it was, the next
    // poll rescans the same span and the one after a longer one -- a busy
    // agent the caller cannot see turning an idle reader into a scan of the
    // day's events on every notification.
    let cursor = feed["cursor"].as_str().expect("cursor");
    let newest: Uuid = sqlx::query_scalar(
        "select id from events where workspace_id = $1 order by id desc limit 1",
    )
    .bind(acme)
    .fetch_one(&h.db.pool)
    .await
    .expect("newest event");
    assert_eq!(
        cursor,
        newest.to_string(),
        "the cursor stopped at the last visible event instead of the end of the window: {body}"
    );
}
