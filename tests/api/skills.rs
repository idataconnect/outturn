//! Skills: versions, files, overrides, derivation and invocation.

use super::*;

// Skills -----------------------------------------------------------------------

/// A workspace reads the operator's skills and cannot write them.
///
/// This is the line the whole feature stands on: an operator ships an
/// integration to every customer, and a customer who could edit it in place
/// would be editing everyone's.
#[tokio::test]
async fn a_workspace_reads_the_operators_skills_but_cannot_edit_them() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "operator could not ship a skill: {body}"
    );
    let shipped: serde_json::Value = serde_json::from_str(&body).expect("json");
    let skill_id = shipped["id"].as_str().expect("id").to_string();

    // A workspace admin, who is not the operator.
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h.get("/v1/skills", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("\"crm\""),
        "the operator's skill was not visible: {body}"
    );

    // Editing it is not forbidden but absent: the workspace's own id is what
    // the write matches on, so there is nothing there to change.
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/skills/{skill_id}"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"Ours now"}"#))
        .unwrap();
    let (status, _) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a workspace edited the operator's skill"
    );

    // And the platform route is closed to them.
    let (status, _) = h
        .post(
            "/v1/platform/skills",
            Some(&admin),
            r#"{"slug":"sneaky","name":"Sneaky","body":"x"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a workspace shipped a platform skill"
    );
}

/// The history names who wrote each version: a person in the reader's own
/// workspace by name, and the operator's staff as the operator.
#[tokio::test]
async fn skill_history_names_its_authors_as_the_reader_may_know_them() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let theirs: serde_json::Value = serde_json::from_str(&body).unwrap();

    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"inn","name":"Inn","body":"x"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let ours: serde_json::Value = serde_json::from_str(&body).unwrap();

    let (_, body) = h
        .get(
            &format!("/v1/skills/{}/versions", ours["id"].as_str().unwrap()),
            Some(&admin),
        )
        .await;
    let author = &items(&body)[0]["author"];
    assert_eq!(author["kind"], "person", "{body}");
    assert_eq!(author["name"], "Test User", "{body}");

    let (_, body) = h
        .get(
            &format!("/v1/skills/{}/versions", theirs["id"].as_str().unwrap()),
            Some(&admin),
        )
        .await;
    let author = &items(&body)[0]["author"];
    assert_eq!(author["kind"], "operator", "{body}");
    assert!(
        author.get("name").is_none(),
        "the operator's staff was named: {body}"
    );
    finish!(h);
}

/// A version read back lists the hosts it declared, not an empty list.
#[tokio::test]
async fn a_version_read_back_names_its_hosts() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"inn","name":"Inn","body":"x","hosts":["api.inn.example"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = skill["id"].as_str().unwrap();
    let v = skill["version_id"].as_str().unwrap();

    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions"), Some(&admin))
        .await;
    let list = items(&body);
    assert_eq!(list[0]["hosts"][0], "api.inn.example", "{body}");
    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions/{v}"), Some(&admin))
        .await;
    let one: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(one["hosts"][0], "api.inn.example", "{body}");
}

/// A version's files are part of it: listed with it, readable as published, and
/// unchanged by later versions.
#[tokio::test]
async fn a_skill_versions_its_files_with_its_body() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"inn","name":"Inn","body":"Read inn/book.md first.",
                "files":[{"path":"book.md","content":"POST /bookings, v1"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = skill["id"].as_str().unwrap().to_string();
    let v1 = skill["version_id"].as_str().unwrap().to_string();

    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions/{v1}"), Some(&admin))
        .await;
    let version: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(version["files"][0]["path"], "book.md", "{body}");

    // A body edit that says nothing about files keeps them.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"Read book.md before booking."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body edit: {body}");
    let v2: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v2["files"][0]["path"], "book.md",
        "files were dropped: {body}"
    );

    // Changing a file appends a version, and the old one still reads as it was.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"Read book.md before booking.",
                "files":[{"path":"book.md","content":"POST /bookings, v2"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "file edit: {body}");
    let v3: serde_json::Value = serde_json::from_str(&body).unwrap();
    let v3_id = v3["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .get(
            &format!("/v1/skills/{id}/versions/{v1}/files/book.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "POST /bookings, v1");
    let (_, body) = h
        .get(
            &format!("/v1/skills/{id}/versions/{v3_id}/files/book.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(body, "POST /bookings, v2");

    // Sending the live version again appends nothing.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"Read book.md before booking.",
                "files":[{"path":"book.md","content":"POST /bookings, v2"}]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an identical version was appended: {body}"
    );
    let same: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(same["id"], v3["id"]);

    // A path that climbs is refused rather than cleaned up.
    let (status, _) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"x","files":[{"path":"../other/x.md","content":"x"}]}"#,
        )
        .await;
    assert!(
        status.is_client_error(),
        "a climbing path was accepted: {status}"
    );
}

/// A version says which of its files nothing leads an agent to, following
/// names from the body through the files it names -- and works that out for a
/// version published before links were recorded, from the stored content.
#[tokio::test]
async fn a_version_says_which_files_nothing_names() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"inn","name":"Inn","body":"Read `skill/inn/rooms.md` first.",
                "files":[{"path":"rooms.md","content":"Detail: `skill/inn/book.md`"},
                         {"path":"book.md","content":"POST /bookings"},
                         {"path":"stray.md","content":"nothing names this"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = skill["id"].as_str().unwrap().to_string();
    let v1 = skill["version_id"].as_str().unwrap().to_string();
    let read = || async {
        let (_, body) = h
            .get(&format!("/v1/skills/{id}/versions/{v1}"), Some(&admin))
            .await;
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    };
    assert_eq!(read().await["unreached"], serde_json::json!(["stray.md"]));

    // As if published before links were recorded.
    sqlx::query("update skill_version_files set links = null where version_id = $1::uuid")
        .bind(&v1)
        .execute(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(
        read().await["unreached"],
        serde_json::json!(["stray.md"]),
        "not worked out again from the stored content"
    );

    // A body-only edit carries the files' links with them.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"Read stray.md."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body edit: {body}");
    let v2: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v2["unreached"], serde_json::json!(["book.md", "rooms.md"]));

    // And the history says the same of each version.
    let (status, body) = h
        .get(&format!("/v1/skills/{id}/versions"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK, "history: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        page["items"][0]["unreached"],
        serde_json::json!(["book.md", "rooms.md"])
    );
    assert_eq!(
        page["items"][1]["unreached"],
        serde_json::json!(["stray.md"])
    );
    // A summary, not the version: no prose, no file list, but what it changed.
    assert!(page["items"][0].get("body").is_none());
    assert!(page["items"][0].get("files").is_none());
    assert_eq!(page["items"][0]["file_count"], 3);
    assert_eq!(page["items"][0]["changed"], serde_json::json!([]));
}

/// An operator's skill reaches a workspace with its files, including through a
/// fork, and a workspace's override cannot carry files of its own.
#[tokio::test]
async fn an_operators_skill_files_reach_the_workspace() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op3@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"See crm/call.md.",
                "files":[{"path":"call.md","content":"GET /v1/accounts"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let base: serde_json::Value = serde_json::from_str(&body).unwrap();
    let base_id = base["id"].as_str().unwrap().to_string();
    let base_v = base["version_id"].as_str().unwrap().to_string();

    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, body) = h
        .get(
            &format!("/v1/skills/{base_id}/versions/{base_v}/files/call.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "GET /v1/accounts");

    let (status, body) = h
        .post(
            &format!("/v1/skills/{base_id}/fork"),
            Some(&admin),
            r#"{"slug":"crm-mine","name":"CRM (mine)"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "fork: {body}");
    let fork: serde_json::Value = serde_json::from_str(&body).unwrap();
    let fork_id = fork["id"].as_str().unwrap().to_string();
    let fork_v = fork["version_id"].as_str().unwrap().to_string();
    let (status, body) = h
        .get(
            &format!("/v1/skills/{fork_id}/versions/{fork_v}/files/call.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the fork lost its file: {body}");
    assert_eq!(body, "GET /v1/accounts");

    let (status, _) = h
        .post(
            "/v1/skills",
            Some(&admin),
            &format!(
                r#"{{"slug":"crm-ours","name":"Ours","body":"v2 here.","base_skill_id":"{base_id}",
                    "files":[{{"path":"call.md","content":"GET /v2/accounts"}}]}}"#
            ),
        )
        .await;
    assert!(
        status.is_client_error(),
        "an override carried files: {status}"
    );
}

/// An override composes after the prose it speaks about, and applies wherever
/// its base is used without being bound itself.
#[tokio::test]
async fn an_override_composes_after_its_base_and_needs_no_binding() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op2@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (_, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    let base: serde_json::Value = serde_json::from_str(&body).expect("json");
    let base_id = base["id"].as_str().expect("id").to_string();

    // The workspace writes its variation once, against the operator's skill.
    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(
                r#"{{"slug":"crm-ours","name":"CRM (ours)","body":"Our region is on v2.","base_skill_id":"{base_id}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "override refused: {body}");

    // An agent binds the base alone.
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&operator),
            r#"{"name":"Helper","slug":"helper"}"#,
        )
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("json");
    let agent_id = agent["id"].as_str().expect("id").to_string();

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {operator}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{base_id}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "binding refused: {body}");

    let composed = h
        .skills
        .resolve_for_agent(acme, agent_id.parse().unwrap())
        .await
        .expect("resolve");

    assert_eq!(
        composed.len(),
        2,
        "expected base and override: {composed:?}"
    );
    assert_eq!(composed[0].body, "Call the v1 endpoint.");
    assert_eq!(
        composed[1].body, "Our region is on v2.",
        "the override must come last"
    );
}

/// An override is not a thing an agent is given on its own.
#[tokio::test]
async fn an_override_cannot_be_bound_by_itself() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op3@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            r#"{"slug":"base","name":"Base","body":"b"}"#,
        )
        .await;
    let base_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(r#"{{"slug":"ov","name":"Ov","body":"o","base_skill_id":"{base_id}"}}"#),
        )
        .await;
    let override_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A second override of the same base has no defined order against the first.
    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(r#"{{"slug":"ov2","name":"Ov2","body":"o2","base_skill_id":"{base_id}"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a second override was allowed: {body}"
    );

    let (_, body) = h
        .post("/v1/agents", Some(&operator), r#"{"name":"H","slug":"h"}"#)
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {operator}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{override_id}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an override was bound directly: {body}"
    );
}

/// When the operator edits a skill, an override written against the old version
/// is reported as stale rather than left to rot quietly.
#[tokio::test]
async fn editing_a_base_marks_the_overrides_written_against_it() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op4@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (_, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"v1 endpoint"}"#,
        )
        .await;
    let base_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(
                r#"{{"slug":"ours","name":"Ours","body":"stay on v1","base_skill_id":"{base_id}"}}"#
            ),
        )
        .await;
    let override_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = h
        .get(&format!("/v1/skills/{override_id}"), Some(&operator))
        .await;
    let before: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        before["base_moved"], false,
        "fresh override reported as stale"
    );

    // The operator ships a new version of the base.
    let (status, body) = h
        .post(
            &format!("/v1/platform/skills/{base_id}/versions"),
            Some(&operator),
            r#"{"body":"v2 endpoint","note":"v2 migration"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "publish refused: {body}");

    let (_, body) = h
        .get(&format!("/v1/skills/{override_id}"), Some(&operator))
        .await;
    let after: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        after["base_moved"], true,
        "the override was not reported stale after its base moved: {body}"
    );
}

/// Rolling back writes the old body forward, so what was live on any day stays
/// answerable from the history alone.
#[tokio::test]
async fn a_rollback_appends_rather_than_moving_backwards() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("a@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"s","name":"S","body":"one"}"#,
        )
        .await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    h.post(
        &format!("/v1/skills/{id}/versions"),
        Some(&admin),
        r#"{"body":"two","note":"second"}"#,
    )
    .await;
    // The rollback: version one's body, sent forward as version three.
    h.post(
        &format!("/v1/skills/{id}/versions"),
        Some(&admin),
        r#"{"body":"one","note":"rolled back to v1"}"#,
    )
    .await;

    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions"), Some(&admin))
        .await;
    let versions = items(&body);
    assert_eq!(versions.len(), 3, "a rollback lost history: {body}");
    assert_eq!(versions[0]["ordinal"], 3, "newest first");
    // The history lists versions without their prose; the version itself has it.
    assert!(versions[0].get("body").is_none(), "{body}");
    let v3 = versions[0]["id"].as_str().unwrap();
    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions/{v3}"), Some(&admin))
        .await;
    let v3: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v3["body"], "one", "the rollback did not carry the old body");

    let (_, body) = h.get(&format!("/v1/skills/{id}"), Some(&admin)).await;
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        skill["ordinal"], 3,
        "the live version is the newest, not the oldest"
    );
}

/// A turn is given its skills, and what it was given is written down.
///
/// This is the end the whole feature exists for: prose the operator shipped,
/// varied by the workspace, reaching the model in an order that makes the
/// variation the one that stands -- and a record afterwards of exactly which
/// versions did it, since an eval cannot measure what it cannot name.
#[tokio::test]
async fn a_turn_is_composed_from_its_skills_and_the_versions_are_recorded() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op5@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (_, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    let base_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, _) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(
                r#"{{"slug":"ours","name":"CRM (ours)","body":"Our region is on v2.","base_skill_id":"{base_id}"}}"#
            ),
        )
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&operator),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Only the base is bound. The override rides along because it is the
    // workspace's standing variation on it.
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {operator}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{base_id}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "binding refused: {body}");

    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&operator),
            &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&operator),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "no work handed out: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let prompt = assignment["system_prompt"].as_str().expect("system_prompt");

    // The platform's preamble leads, and the agent's own prompt follows it --
    // see `platform_preamble`. Ordering is what matters here, so the prompt is
    // located rather than required to come first.
    let own_at = prompt
        .find("Be brief.")
        .expect("the agent's own prompt went missing");
    let base_at = prompt
        .find("Call the v1 endpoint.")
        .expect("base prose missing");
    assert!(
        own_at < base_at,
        "the agent's prompt should lead its skills:\n{prompt}"
    );
    let over_at = prompt
        .find("Our region is on v2.")
        .expect("override prose missing");
    assert!(
        base_at < over_at,
        "the override did not come last:\n{prompt}"
    );

    // And the turn knows what it was built from, before it has even run.
    let reply: Uuid = assignment["reply_id"]
        .as_str()
        .expect("reply")
        .parse()
        .unwrap();
    let recorded: Vec<(Uuid, i32)> = sqlx::query_as(
        "select skill_id, position from turn_skills where reply_id = $1 order by position",
    )
    .bind(reply)
    .fetch_all(&h.db.pool)
    .await
    .expect("turn_skills");
    assert_eq!(
        recorded.len(),
        2,
        "the turn did not record both skills: {recorded:?}"
    );
    assert_eq!(
        recorded[0].0.to_string(),
        base_id,
        "the base should be recorded first"
    );
}

/// A turn is handed the files of the version its agent is bound to -- the
/// pinned one, not the live one -- under the workspace that owns them.
#[tokio::test]
async fn a_turn_is_handed_its_pinned_skill_files() {
    use sha2::Digest;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op5@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Read skill/crm/call.md.",
                "files":[{"path":"call.md","content":"GET /v1/accounts"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let skill_id = skill["id"].as_str().unwrap().to_string();
    let v1 = skill["version_id"].as_str().unwrap().to_string();
    let (status, body) = h
        .post(
            &format!("/v1/platform/skills/{skill_id}/versions"),
            Some(&operator),
            r#"{"body":"Read skill/crm/call.md.",
                "files":[{"path":"call.md","content":"GET /v2/accounts"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&operator),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {operator}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"[{{"skill_id":"{skill_id}","version_id":"{v1}"}}]"#
        )))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "binding refused: {body}");

    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&operator),
            &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&operator),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "no work handed out: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let files = assignment["skill_files"].as_array().expect("skill_files");
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0]["path"], "skill/crm/call.md");
    assert_eq!(
        files[0]["sha256"],
        hex::encode(sha2::Sha256::digest(b"GET /v1/accounts")),
        "the turn was handed the live version's file, not the pinned one"
    );
    assert_eq!(
        files[0]["workspace_id"],
        outturn::api::usage::PLATFORM_WORKSPACE.to_string(),
        "an operator's file lives under the operator's prefix"
    );
}

/// A generated skill keeps what people added to it when its specification
/// changes: a note at the skill level reaches the body, an operation's gate
/// becomes its frontmatter, regenerating is a proposal until published, and an
/// annotation whose operation disappeared is reported rather than dropped.
#[tokio::test]
async fn a_derived_skill_keeps_its_annotations_across_a_new_specification() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let spec = |extra_op: bool| {
        let mut paths = serde_json::json!({
            "/invoices": {"post": {"operationId": "createInvoice", "summary": "Create a sale invoice",
                "tags": ["Sale Invoices"], "responses": {"201": {"description": "ok"}}}},
            "/items": {"get": {"operationId": "listItems", "summary": "List items",
                "tags": ["Items"], "responses": {"200": {"description": "ok"}}}},
        });
        if extra_op {
            paths["/bills"] = serde_json::json!({"get": {"operationId": "listBills",
                "summary": "List bills", "tags": ["Bills"], "responses": {"200": {"description": "ok"}}}});
        }
        serde_json::json!({"openapi": "3.0.0", "info": {"title": "Books", "version": "1"}, "paths": paths})
    };

    let (status, body) = h
        .post(
            "/v1/platform/skills/from-openapi",
            Some(&operator),
            &serde_json::json!({"slug": "books", "name": "Books", "base_url": "https://books.example.com",
                "auth_header": "Authorization", "spec": spec(false)})
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = h
        .get(&format!("/v1/skills/{id}/source"), Some(&operator))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["base_url"],
        "https://books.example.com"
    );

    let annotate = |a: serde_json::Value| {
        let h = &h;
        let operator = &operator;
        let id = id.clone();
        async move {
            h.post(
                &format!("/v1/platform/skills/{id}/annotations"),
                Some(operator),
                &a.to_string(),
            )
            .await
        }
    };
    let (status, body) = annotate(serde_json::json!({"level": "skill", "kind": "note",
        "value": "Invoices are called sale invoices here."}))
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = annotate(serde_json::json!({"level": "operation", "target": "create_invoice",
        "kind": "approval", "value": "approval:\n  requires: invoice\n  matches: POST /invoices\n  binds: [customer_id]"}))
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, _) = annotate(serde_json::json!({"level": "category", "target": "Items",
        "kind": "hidden"}))
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "hiding is about one operation"
    );
    let (status, _) = annotate(
        serde_json::json!({"level": "operation", "target": "create_invoice",
        "kind": "approval", "value": "approval:\n  requires: invoice"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a rule the parser refuses is refused now"
    );
    let (status, body) = annotate(
        serde_json::json!({"level": "operation", "target": "list_items",
        "kind": "note", "value": "Page through them."}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let regenerate = |r: serde_json::Value| {
        let h = &h;
        let operator = &operator;
        let id = id.clone();
        async move {
            let (status, body) = h
                .post(
                    &format!("/v1/platform/skills/{id}/regenerate"),
                    Some(operator),
                    &r.to_string(),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            serde_json::from_str::<Value>(&body).unwrap()
        }
    };

    // A proposal: says what would change, publishes nothing.
    let proposed = regenerate(serde_json::json!({})).await;
    assert_eq!(proposed["body_changed"], true, "{proposed}");
    assert!(proposed["version"].is_null());
    let (_, body) = h.get(&format!("/v1/skills/{id}"), Some(&operator)).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["ordinal"], 1);

    // Published: the note is in the body, the gate is the file's frontmatter.
    let published = regenerate(serde_json::json!({"publish": true})).await;
    assert_eq!(published["version"]["ordinal"], 2, "{published}");
    let v2 = published["version"]["id"].as_str().unwrap();
    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions/{v2}"), Some(&operator))
        .await;
    assert!(
        body.contains("Invoices are called sale invoices here."),
        "{body}"
    );
    let (_, file) = h
        .get(
            &format!("/v1/skills/{id}/versions/{v2}/files/create_invoice.md"),
            Some(&operator),
        )
        .await;
    assert!(file.starts_with("---\napproval:"), "{file}");

    // A new specification without listItems: the note on it is reported, and
    // everything else survives into the next version.
    let mut next = spec(true);
    next["paths"].as_object_mut().unwrap().remove("/items");
    let renewed = regenerate(serde_json::json!({"spec": next, "publish": true})).await;
    assert_eq!(
        renewed["unmatched"].as_array().unwrap().len(),
        1,
        "{renewed}"
    );
    assert_eq!(renewed["unmatched"][0]["target"], "list_items");
    let v3 = renewed["version"]["id"].as_str().unwrap();
    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions/{v3}"), Some(&operator))
        .await;
    assert!(body.contains("Invoices are called sale invoices here."));
    let (_, file) = h
        .get(
            &format!("/v1/skills/{id}/versions/{v3}/files/create_invoice.md"),
            Some(&operator),
        )
        .await;
    assert!(
        file.starts_with("---\napproval:"),
        "the gate survived: {file}"
    );

    // Hiding the gated operation is refused rather than half-done.
    let (status, body) = annotate(serde_json::json!({"level": "operation",
        "target": "create_invoice", "kind": "hidden"}))
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Publishing what is already live publishes nothing, and leaves the live
    // version's record of what it was made from alone.
    let again = regenerate(serde_json::json!({"publish": true})).await;
    assert!(again["version"].is_null(), "{again}");
    let (_, body) = h.get(&format!("/v1/skills/{id}"), Some(&operator)).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["ordinal"], 3);

    // Retiring the gate's annotation does not quietly remove the gate.
    let (_, body) = h
        .get(&format!("/v1/skills/{id}/source"), Some(&operator))
        .await;
    let source = serde_json::from_str::<Value>(&body).unwrap();
    let gate = source["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "approval")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/platform/skills/{id}/annotations/{gate}"))
        .header("authorization", format!("Bearer {operator}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let proposed = regenerate(serde_json::json!({})).await;
    assert_eq!(proposed["lost_gates"][0], "POST /invoices", "{proposed}");
    let (status, body) = h
        .post(
            &format!("/v1/platform/skills/{id}/regenerate"),
            Some(&operator),
            &serde_json::json!({"publish": true}).to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let removed = regenerate(serde_json::json!({"publish": true, "remove_gates": true})).await;
    assert_eq!(removed["version"]["ordinal"], 4, "{removed}");
}

/// A skill generated before specifications were kept, then gated by hand, keeps
/// its gate when it is adopted: the rule becomes an annotation rather than
/// being regenerated away. And a later base_url sent alone keeps the header.
#[tokio::test]
async fn adopting_a_hand_gated_skill_keeps_its_gate_and_its_header() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let gate = "approval:\n  requires: invoice\n  matches: POST /invoices\n  binds: [customer_id]";
    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            &serde_json::json!({"slug": "books", "name": "Books", "body": "Books.",
                "hosts": ["books.example.com"],
                "files": [{"path": "create_invoice.md",
                    "content": format!("---\n{gate}\n---\n\n# create_invoice\n")}]})
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let spec = serde_json::json!({"openapi": "3.0.0", "info": {"title": "Books", "version": "1"},
        "paths": {"/invoices": {"post": {"operationId": "createInvoice", "summary": "Create",
            "responses": {"201": {"description": "ok"}}}}}});
    let regenerate = |r: serde_json::Value| {
        let h = &h;
        let operator = &operator;
        let id = id.clone();
        async move {
            let (status, body) = h
                .post(
                    &format!("/v1/platform/skills/{id}/regenerate"),
                    Some(operator),
                    &r.to_string(),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            serde_json::from_str::<Value>(&body).unwrap()
        }
    };
    let adopted = regenerate(
        serde_json::json!({"spec": spec, "base_url": "https://books.example.com",
        "auth_header": "Authorization", "publish": true}),
    )
    .await;
    assert_eq!(adopted["lost_gates"], serde_json::json!([]), "{adopted}");
    assert_eq!(adopted["version"]["ordinal"], 2, "{adopted}");

    let source = || {
        let h = &h;
        let operator = operator.clone();
        let id = id.clone();
        async move {
            let (_, body) = h
                .get(&format!("/v1/skills/{id}/source"), Some(&operator))
                .await;
            serde_json::from_str::<Value>(&body).unwrap()
        }
    };
    let kept = source().await;
    let annotations = kept["annotations"].as_array().unwrap();
    assert_eq!(annotations.len(), 1, "{kept}");
    assert_eq!(annotations[0]["target"], "create_invoice");
    assert_eq!(annotations[0]["value"], gate);

    // The base_url again, without the header: the stored one stays.
    regenerate(serde_json::json!({"spec": spec, "base_url": "https://books.example.com/"})).await;
    assert_eq!(source().await["auth_header"], "Authorization");
}

/// An operator's skill nothing has used can be deleted; one given to an agent
/// is kept, since deleting it would quietly unbind it.
#[tokio::test]
async fn an_unused_platform_skill_can_be_deleted_and_a_bound_one_cannot() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let make = |slug: &'static str| {
        let h = &h;
        let operator = &operator;
        async move {
            let (status, body) = h
                .post(
                    "/v1/platform/skills",
                    Some(operator),
                    &serde_json::json!({"slug": slug, "name": slug, "body": "x"}).to_string(),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            serde_json::from_str::<Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let delete = |id: String, token: String| {
        let h = &h;
        async move {
            let req = Request::builder()
                .method("DELETE")
                .uri(format!("/v1/platform/skills/{id}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request");
            h.send(req).await
        }
    };

    let unused = make("unused").await;
    let (status, _) = delete(unused.clone(), admin.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only the operator");
    let (status, body) = delete(unused.clone(), operator.clone()).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = delete(unused, operator.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let bound = make("bound").await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"Desk","slug":"desk"}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{bound}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = delete(bound, operator).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("retire it instead"), "{body}");
}
/// A skill picked from the composer's menu travels beside the message, not
/// inside it: the API refuses one the agent was not given, keeps the pick on
/// the message for the transcript to draw, and the turn answering it is told
/// what was picked and handed the skill's main reference file.
#[tokio::test]
async fn an_invoked_skill_is_stored_beside_the_message_and_brought_into_the_turn() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"inn","name":"Inn","body":"Book rooms at the inn.",
                "files":[{"path":"index.md","content":"Doubles are 120 a night."},
                         {"path":"rates.md","content":"Winter rates."}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let skill_id = skill["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let agent: serde_json::Value = serde_json::from_str(&body).unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{skill_id}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "binding refused: {body}");

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).unwrap();
    let session_id: Uuid = session["id"].as_str().unwrap().parse().unwrap();
    let messages = format!("/v1/agent-sessions/{session_id}/messages");

    // One the agent was not given is refused, and says which.
    let (status, body) = h
        .post(
            &messages,
            Some(&admin),
            r#"{"content":"go","skill":"elsewhere"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("elsewhere"), "{body}");
    // Nothing typed and nothing picked is still nothing.
    let (status, _) = h.post(&messages, Some(&admin), r#"{"content":" "}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A pick alone is a message.
    let (status, body) = h
        .post(&messages, Some(&admin), r#"{"content":"","skill":"inn"}"#)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(sent["content"], "");
    assert_eq!(sent["metadata"]["skill"]["slug"], "inn", "{body}");
    assert_eq!(sent["metadata"]["skill"]["name"], "Inn", "{body}");

    // The transcript read carries it, so a reload draws the same mark.
    let (status, body) = h.get(&messages, Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let history: serde_json::Value = serde_json::from_str(&body).unwrap();
    let stored = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .expect("the message");
    assert_eq!(stored["metadata"]["skill"]["slug"], "inn", "{body}");
    assert!(stored["metadata"].get("skill_note").is_none(), "{body}");

    // The turn is told, and given the main reference file.
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let conversation = assignment["conversation"].to_string();
    assert!(
        conversation.contains("invoked the skill \\\"Inn\\\" with no further instructions"),
        "{conversation}"
    );
    assert!(
        conversation.contains("Doubles are 120 a night."),
        "{conversation}"
    );
    assert!(
        conversation.contains("skill/inn/rates.md"),
        "{conversation}"
    );
    assert!(!conversation.contains("run skill"), "{conversation}");
    let reply: Uuid = assignment["reply_id"].as_str().unwrap().parse().unwrap();

    // Sent mid-turn, an invocation waits for its own turn rather than being
    // handed over as a steer -- and so does what follows it.
    let (status, _) = h
        .post(
            &messages,
            Some(&admin),
            r#"{"content":"again","skill":"inn"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = h
        .post(&messages, Some(&admin), r#"{"content":"and this"}"#)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let taken = outturn::gateway::routing::take_pending(&h.db.pool, session_id, reply)
        .await
        .expect("take");
    assert!(taken.is_empty(), "{taken:?}");
}
