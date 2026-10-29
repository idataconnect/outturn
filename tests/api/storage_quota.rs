//! A workspace's storage quota through the API: what is counted as files are
//! written and deleted, what a full workspace is refused, who sets it, and
//! the weekly listing that corrects the count. See docs/storage-quota.md.

use super::*;

/// A workspace with an admin, the platform's operator, an agent and a
/// conversation with it.
struct Space {
    acme: Uuid,
    admin: String,
    root: String,
    agent: Uuid,
    session: Uuid,
}

async fn a_space(h: &Harness) -> Space {
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &serde_json::json!({ "agent_id": agent, "title": "" }).to_string(),
        )
        .await;
    let session: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    Space {
        acme,
        admin,
        root,
        agent,
        session,
    }
}

async fn send(
    h: &Harness,
    method: &str,
    uri: &str,
    token: &str,
    body: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    h.send(req).await
}

async fn upload(h: &Harness, s: &Space, name: &str, bytes: usize) -> (StatusCode, String) {
    send(
        h,
        "PUT",
        &format!("/v1/agent-sessions/{}/files/session/{name}", s.session),
        &s.admin,
        &"x".repeat(bytes),
    )
    .await
}

async fn storage(h: &Harness, s: &Space) -> Value {
    let (status, body) = h.get("/v1/storage", Some(&s.admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// Files count as they are uploaded and stop counting when deleted. A quota
/// applies once the workspace has been listed from the store, and then a
/// write past it is refused while replacing a file with a smaller one is not.
#[tokio::test]
async fn a_workspace_holds_what_it_wrote_and_no_more_than_its_quota() {
    let h = harness_or_skip!();
    let s = a_space(&h).await;
    assert_eq!(upload(&h, &s, "a.txt", 100).await.0, StatusCode::CREATED);
    let now = storage(&h, &s).await;
    assert_eq!(now["stored_bytes"], 100);
    assert_eq!(now["quota"], 0);

    let (status, body) = send(
        &h,
        "PUT",
        &format!("/v1/workspaces/{}/settings/max_stored_bytes", s.acme),
        &s.root,
        r#"{"value":150}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // Not yet listed from the store: a count nobody has checked refuses
    // nothing.
    assert_eq!(upload(&h, &s, "b.txt", 100).await.0, StatusCode::CREATED);
    assert_eq!(storage(&h, &s).await["counted"], false);

    outturn::api::storage_sweep::sweep(&h.db.pool, &h.storage, 30).await;
    let now = storage(&h, &s).await;
    assert_eq!(now["counted"], true);
    assert_eq!(now["stored_bytes"], 200);
    assert_eq!(now["full"], true);

    let (status, body) = upload(&h, &s, "c.txt", 10).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["code"],
        "too_large"
    );
    assert!(
        serde_json::from_str::<Value>(&body).unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("full"),
        "{body}"
    );

    // A smaller file in the place of a larger one, and a delete, still go.
    assert_eq!(upload(&h, &s, "b.txt", 30).await.0, StatusCode::CREATED);
    assert_eq!(storage(&h, &s).await["stored_bytes"], 130);
    let (status, _) = send(
        &h,
        "DELETE",
        &format!("/v1/agent-sessions/{}/files/session/a.txt", s.session),
        &s.admin,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(storage(&h, &s).await["stored_bytes"], 30);

    // A turn is handed what is left.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{}/messages", s.session),
            Some(&s.admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, _, assignment) = take_assignment(&h).await;
    assert_eq!(assignment["remaining_stored_bytes"], 120);
    finish!(h);
}

/// The quota is the operator's to set unless the operator hands it to
/// workspaces, and then it is set per workspace, never per agent.
#[tokio::test]
async fn who_sets_the_quota_is_the_operators_choice() {
    let h = harness_or_skip!();
    let s = a_space(&h).await;
    let set = |token: String, uri: String| {
        let h = &h;
        async move { send(h, "PUT", &uri, &token, r#"{"value":1000}"#).await }
    };
    let own = "/v1/settings/max_stored_bytes".to_string();
    assert_eq!(
        set(s.admin.clone(), own.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = set(
        s.admin.clone(),
        format!("/v1/workspaces/{}/settings/max_stored_bytes", s.acme),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only the operator, from there"
    );

    let (status, body) = send(
        &h,
        "PUT",
        "/v1/platform/settings/stored_bytes_set_by",
        &s.root,
        r#"{"value":"workspace"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = set(s.admin.clone(), own).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "handed to workspaces: {body}"
    );
    assert_eq!(storage(&h, &s).await["quota"], 1000);
    let (status, _) = set(
        s.admin.clone(),
        format!("/v1/agents/{}/settings/max_stored_bytes", s.agent),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "never per agent");
    finish!(h);
}

/// What a runtime reports writing and deleting is counted for the workspace
/// the turn is in, and a key in another workspace is not counted at all.
#[tokio::test]
async fn a_runtime_reports_what_it_wrote_and_deleted() {
    let h = harness_or_skip!();
    let s = a_space(&h).await;
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{}/messages", s.session),
            Some(&s.admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (job, lease, _) = take_assignment(&h).await;
    let root = format!("sessions/{}/{}/{}", s.acme, s.agent, s.session);
    let elsewhere = format!("sessions/{}/{}/{}", Uuid::now_v7(), s.agent, s.session);
    let stream = [
        serde_json::json!({"kind": "wrote", "path": "session/a.md", "key": format!("{root}/a.md"), "bytes": 70}),
        serde_json::json!({"kind": "wrote", "path": "session/b.md", "key": format!("{root}/b.md"), "bytes": 30}),
        serde_json::json!({"kind": "deleted", "key": format!("{root}/b.md")}),
        serde_json::json!({"kind": "wrote", "path": "session/c.md", "key": format!("{elsewhere}/c.md"), "bytes": 999}),
        serde_json::json!({"kind": "done", "content": "ok"}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    assert_eq!(storage(&h, &s).await["stored_bytes"], 70);
    let counted: i64 = sqlx::query_scalar("select count(*) from stored_objects")
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(
        counted, 1,
        "nothing counted for a key outside the workspace"
    );
    finish!(h);
}

/// The weekly listing replaces the count with what the store holds, which is
/// how a write no runtime reported comes to count.
#[tokio::test]
async fn the_listing_counts_what_nobody_reported() {
    let h = harness_or_skip!();
    let s = a_space(&h).await;
    assert_eq!(upload(&h, &s, "a.txt", 100).await.0, StatusCode::CREATED);
    let unreported = format!("agents/{}/{}/notes.md", s.acme, s.agent);
    h.storage.write(&unreported, &[0; 40]).await.unwrap();
    assert_eq!(storage(&h, &s).await["stored_bytes"], 100);

    outturn::api::storage_sweep::sweep(&h.db.pool, &h.storage, 30).await;
    let now = storage(&h, &s).await;
    assert_eq!(now["stored_bytes"], 140, "{now}");
    assert_eq!(now["counted"], true);

    // Listed again within the week, nothing changes.
    h.storage.write(&unreported, &[0; 400]).await.unwrap();
    outturn::api::storage_sweep::sweep(&h.db.pool, &h.storage, 30).await;
    assert_eq!(storage(&h, &s).await["stored_bytes"], 140);
    finish!(h);
}
