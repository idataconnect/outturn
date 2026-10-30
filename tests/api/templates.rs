//! Agent templates and the agents a workspace is owed.

use super::*;

// Agent templates --------------------------------------------------------------

async fn make_template(h: &Harness, operator: &str, body: &str) -> Value {
    let (status, body) = h
        .post("/v1/platform/agent-templates", Some(operator), body)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// The workspace's agents made from templates, by template id.
async fn template_agents(h: &Harness, token: &str) -> Vec<Value> {
    let (status, body) = h.get("/v1/agent-templates", Some(token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    items(&body)
}

/// A required template reaches every workspace there is, and every one made
/// after it, and cannot be removed from any.
#[tokio::test]
async fn a_required_template_is_in_every_workspace_and_stays() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    let t = make_template(
        &h,
        &operator,
        r#"{"slug":"invoicer","name":"Invoicer","availability":"required",
            "requirements":"Quote only listed prices."}"#,
    )
    .await;

    let catalog = template_agents(&h, &admin).await;
    let agent_id = catalog[0]["agent_id"]
        .as_str()
        .expect("made in an existing workspace");
    assert_eq!(catalog[0]["name"], "Invoicer");

    let (status, body) = send_as(&h, "DELETE", &format!("/v1/agents/{agent_id}"), &admin, "").await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a required agent was removed: {body}"
    );

    // A workspace made afterwards has it from the start.
    let (status, body) = h
        .post(
            "/v1/workspaces",
            Some(&operator),
            r#"{"name":"Initech","slug":"initech"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let initech: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let theirs = h
        .login_as("admin@initech.example", None, Some((initech, "admin")))
        .await;
    assert!(template_agents(&h, &theirs).await[0]["agent_id"].is_string());
    let _ = t;
    finish!(h);
}

/// A default template's agent a workspace removed is not put back by the next
/// publish, and comes back when the workspace adds it.
#[tokio::test]
async fn a_removed_default_agent_stays_removed_until_added_again() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    let t = make_template(
        &h,
        &operator,
        r#"{"slug":"books","name":"Bookkeeper","availability":"default"}"#,
    )
    .await;
    let id = t["id"].as_str().unwrap();
    let agent_id = template_agents(&h, &admin).await[0]["agent_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = send_as(&h, "DELETE", &format!("/v1/agents/{agent_id}"), &admin, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = h
        .post(
            &format!("/v1/platform/agent-templates/{id}/versions"),
            Some(&operator),
            r#"{"name":"Bookkeeper","description":"now with more books"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        template_agents(&h, &admin).await[0]["agent_id"].is_null(),
        "a publish put back an agent the workspace removed"
    );

    let (status, body) = h
        .post(
            &format!("/v1/agent-templates/{id}/install"),
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let catalog = template_agents(&h, &admin).await;
    assert!(catalog[0]["agent_id"].is_string());
    assert_eq!(catalog[0]["description"], "now with more books");
    finish!(h);
}

/// An optional template is offered, not made, and a workspace adds it.
#[tokio::test]
async fn an_optional_template_is_offered_and_added_on_request() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    let t = make_template(
        &h,
        &operator,
        r#"{"slug":"inventory","name":"Inventory","availability":"optional"}"#,
    )
    .await;
    assert!(template_agents(&h, &admin).await[0]["agent_id"].is_null());

    let (status, body) = h
        .post(
            &format!("/v1/agent-templates/{}/install", t["id"].as_str().unwrap()),
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let agent_id = serde_json::from_str::<Value>(&body).unwrap()["agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h.get(&format!("/v1/agents/{agent_id}"), Some(&admin)).await;
    let agent: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(agent["slug"], "inventory");
    assert_eq!(agent["template_id"], t["id"]);
    finish!(h);
}

/// Only the operator makes templates.
#[tokio::test]
async fn a_workspace_admin_cannot_make_templates() {
    let h = harness().await;
    let (_, _, admin) = template_people(&h).await;
    let (status, _) = h
        .post(
            "/v1/platform/agent-templates",
            Some(&admin),
            r#"{"slug":"mine","name":"Mine"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    finish!(h);
}

/// A template agent's instructions are the operator's; what the workspace
/// writes is its own section, where the template allows one.
#[tokio::test]
async fn a_workspace_writes_only_its_own_section_of_a_template_agent() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    let t = make_template(
        &h,
        &operator,
        r#"{"slug":"invoicer","name":"Invoicer","availability":"required"}"#,
    )
    .await;
    let agent_id = template_agents(&h, &admin).await[0]["agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let uri = format!("/v1/agents/{agent_id}");

    let (status, _) = send_as(&h, "PATCH", &uri, &admin, r#"{"system_prompt":"anything"}"#).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the template's prompt was overwritten"
    );
    let (status, body) = send_as(
        &h,
        "PATCH",
        &uri,
        &admin,
        r#"{"workspace_addition":"Void duplicates instead of raising them."}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = send_as(
        &h,
        "PATCH",
        &format!("/v1/platform/agent-templates/{}", t["id"].as_str().unwrap()),
        &operator,
        r#"{"allow_additions":false}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = send_as(
        &h,
        "PATCH",
        &uri,
        &admin,
        r#"{"workspace_addition":"More."}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an addition the operator turned off was kept"
    );
    finish!(h);
}

/// A template agent's turns get the template's skills, ahead of its own.
#[tokio::test]
async fn a_template_agent_is_given_its_templates_skills() {
    use outturn::api::skill::SkillStore as _;
    let h = harness().await;
    let (acme, operator, admin) = template_people(&h).await;
    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"ledger","name":"Ledger","body":"Post to the ledger."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill_id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    make_template(
        &h,
        &operator,
        &format!(
            r#"{{"slug":"invoicer","name":"Invoicer","availability":"required",
                 "skills":[{{"skill_id":"{skill_id}"}}]}}"#
        ),
    )
    .await;
    let agent_id: Uuid = template_agents(&h, &admin).await[0]["agent_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let skills = outturn::api::skill::PostgresSkillStore::new(h.db.pool.clone());
    let resolved = skills
        .resolve_for_agent(acme, agent_id)
        .await
        .expect("resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].slug, "ledger");
    finish!(h);
}

/// A template's skills must be the operator's: one workspace's skill in every
/// workspace would be that workspace's in all the others.
#[tokio::test]
async fn a_template_refuses_a_workspaces_own_skill() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"ours","name":"Ours","body":"x"}"#,
        )
        .await;
    let skill_id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = h
        .post(
            "/v1/platform/agent-templates",
            Some(&operator),
            &format!(r#"{{"slug":"t","name":"T","skills":[{{"skill_id":"{skill_id}"}}]}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    finish!(h);
}

/// A template agent's turn is sent the template's prompt -- requirements,
/// defaults, then the workspace's section -- and the template's eager tools.
#[tokio::test]
async fn a_template_agents_turn_carries_the_templates_prompt_and_tools() {
    let h = harness().await;
    let (acme, operator, admin) = template_people(&h).await;
    make_template(
        &h,
        &operator,
        r#"{"slug":"invoicer","name":"Invoicer","availability":"required",
            "requirements":"Quote only listed prices.",
            "defaults":"Raise suspected duplicates.",
            "reminder":"listed prices only",
            "policy":{"model":"test-model"},
            "eager_tools":["read_object"]}"#,
    )
    .await;
    let agent_id = template_agents(&h, &admin).await[0]["agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = send_as(
        &h,
        "PATCH",
        &format!("/v1/agents/{agent_id}"),
        &admin,
        r#"{"workspace_addition":"Void duplicates instead of raising them."}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session}/messages"),
            Some(&admin),
            r#"{"content":"Invoice Acme."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = h.post("/v1/work", Some(&h.runtime_token(acme)), "{}").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn: Value = serde_json::from_str(&body).unwrap();

    let prompt = turn["system_prompt"].as_str().expect("a system prompt");
    let at = |s: &str| {
        prompt
            .find(s)
            .unwrap_or_else(|| panic!("{s:?} missing: {prompt}"))
    };
    assert!(at("Quote only listed prices.") < at("Raise suspected duplicates."));
    assert!(at("Raise suspected duplicates.") < at("Void duplicates instead"));
    assert!(at("Void duplicates instead") < at("Requirements still apply: listed prices only"));
    assert_eq!(turn["model"], "test-model");
    assert_eq!(turn["eager_tools"], serde_json::json!(["read_object"]));
    finish!(h);
}

/// A setting a template fixes wins over the workspace's and the agent's own,
/// cannot be overridden on the agent, and says where it came from.
#[tokio::test]
async fn a_setting_a_template_fixes_wins_and_cannot_be_overridden() {
    use outturn::api::settings::SettingsStore as _;
    let h = harness().await;
    let (acme, operator, admin) = template_people(&h).await;
    make_template(
        &h,
        &operator,
        r#"{"slug":"invoicer","name":"Invoicer","availability":"required",
            "settings":{"temperature":0.2}}"#,
    )
    .await;
    let agent_id = template_agents(&h, &admin).await[0]["agent_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The workspace's own value is beaten by the template's.
    let (status, body) = send_as(
        &h,
        "PUT",
        "/v1/settings/temperature",
        &admin,
        r#"{"value":0.9}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    // The agent's own is refused outright.
    let (status, body) = send_as(
        &h,
        "PUT",
        &format!("/v1/agents/{agent_id}/settings/temperature"),
        &admin,
        r#"{"value":0.9}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an agent overrode its template: {body}"
    );
    assert!(body.contains("fixed by the template"), "{body}");

    let (_, body) = h
        .get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin))
        .await;
    let view: Vec<Value> = serde_json::from_str(&body).unwrap();
    let temperature = view.iter().find(|s| s["key"] == "temperature").unwrap();
    assert_eq!(temperature["value"], 0.2);
    assert_eq!(temperature["source"], "template");

    let settings = outturn::api::settings::PostgresSettingsStore::new(h.db.pool.clone());
    let resolved = settings
        .resolve(acme, agent_id.parse().unwrap())
        .await
        .expect("resolved");
    assert_eq!(resolved.temperature, Some(0.2));
    finish!(h);
}

/// A template cannot fix a value no setting can take.
#[tokio::test]
async fn a_template_refuses_a_setting_the_catalog_would() {
    let h = harness().await;
    let (_, operator, _) = template_people(&h).await;
    for body in [
        r#"{"slug":"a","name":"A","settings":{"temperature":9}}"#,
        r#"{"slug":"b","name":"B","settings":{"no_such_setting":1}}"#,
    ] {
        let (status, reply) = h
            .post("/v1/platform/agent-templates", Some(&operator), body)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {reply}");
    }
    finish!(h);
}

/// The prompt a template agent's turn is sent, as the runtime receives it.
async fn turn_prompt(h: &Harness, acme: Uuid, admin: &str, agent_id: &str) -> String {
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
        )
        .await;
    let session = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session}/messages"),
            Some(admin),
            r#"{"content":"Go."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, body) = h.post("/v1/work", Some(&h.runtime_token(acme)), "{}").await;
    serde_json::from_str::<Value>(&body).unwrap()["system_prompt"]
        .as_str()
        .expect("a turn")
        .to_string()
}

/// A workspace may keep its agent on a version while the template allows it,
/// and every agent follows the newest again once the template stops allowing it.
#[tokio::test]
async fn a_pinned_agent_stays_on_its_version_while_the_template_allows() {
    let h = harness().await;
    let (acme, operator, admin) = template_people(&h).await;
    let t = make_template(
        &h,
        &operator,
        r#"{"slug":"invoicer","name":"Invoicer","availability":"required",
            "allow_pinning":true,"requirements":"Version one rules.",
            "policy":{"model":"test-model"}}"#,
    )
    .await;
    let tid = t["id"].as_str().unwrap().to_string();
    let first = t["current"]["id"].as_str().unwrap().to_string();
    let agent_id = template_agents(&h, &admin).await[0]["agent_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = send_as(
        &h,
        "PUT",
        &format!("/v1/agents/{agent_id}/template-version"),
        &admin,
        &format!(r#"{{"version_id":"{first}"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = h
        .post(
            &format!("/v1/platform/agent-templates/{tid}/versions"),
            Some(&operator),
            r#"{"name":"Invoicer","requirements":"Version two rules.","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let prompt = turn_prompt(&h, acme, &admin, &agent_id).await;
    assert!(
        prompt.contains("Version one rules."),
        "the pin was not kept: {prompt}"
    );
    let (_, body) = h.get(&format!("/v1/agents/{agent_id}"), Some(&admin)).await;
    let view: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(view["template"]["version"], 1);
    assert_eq!(view["template"]["latest"], 2);
    assert_eq!(view["template"]["pinned"], true);

    // The operator stops allowing pins: the agent follows the newest.
    let (status, _) = send_as(
        &h,
        "PATCH",
        &format!("/v1/platform/agent-templates/{tid}"),
        &operator,
        r#"{"allow_pinning":false}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = h.get(&format!("/v1/agents/{agent_id}"), Some(&admin)).await;
    let view: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(view["template"]["version"], 2);
    assert_eq!(view["template"]["pinned"], false);
    finish!(h);
}

/// Pinning is refused where the template does not allow it, and for a version
/// of some other template.
#[tokio::test]
async fn a_pin_is_refused_where_it_is_not_allowed_or_not_this_templates() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    let fixed = make_template(
        &h,
        &operator,
        r#"{"slug":"fixed","name":"Fixed","availability":"required"}"#,
    )
    .await;
    let other = make_template(
        &h,
        &operator,
        r#"{"slug":"other","name":"Other","availability":"required","allow_pinning":true}"#,
    )
    .await;
    let catalog = template_agents(&h, &admin).await;
    let agent_of = |slug: &str| {
        catalog.iter().find(|e| e["slug"] == slug).unwrap()["agent_id"]
            .as_str()
            .unwrap()
            .to_string()
    };

    let (status, _) = send_as(
        &h,
        "PUT",
        &format!("/v1/agents/{}/template-version", agent_of("fixed")),
        &admin,
        &format!(
            r#"{{"version_id":"{}"}}"#,
            fixed["current"]["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "pinned where the template does not allow it"
    );

    let (status, _) = send_as(
        &h,
        "PUT",
        &format!("/v1/agents/{}/template-version", agent_of("other")),
        &admin,
        &format!(
            r#"{{"version_id":"{}"}}"#,
            fixed["current"]["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "pinned to another template's version"
    );
    let _ = other;
    finish!(h);
}

/// A workspace whose agents were never made -- its provisioning failed, or it
/// was made by some path that skipped it -- gets them when its catalog is read.
#[tokio::test]
async fn reading_the_catalog_makes_the_agents_a_workspace_is_owed() {
    let h = harness().await;
    let (_, operator, _) = template_people(&h).await;
    make_template(
        &h,
        &operator,
        r#"{"slug":"invoicer","name":"Invoicer","availability":"required"}"#,
    )
    .await;
    // Made directly, so the API's provisioning never ran for it.
    let late = h.make_workspace("Late", "late").await;
    let admin = h
        .login_as("admin@late.example", None, Some((late, "admin")))
        .await;
    assert!(
        template_agents(&h, &admin).await[0]["agent_id"].is_string(),
        "a workspace was left without its required agent"
    );
    finish!(h);
}

/// An optional agent a workspace removed stays removed when the operator later
/// makes its template default.
#[tokio::test]
async fn a_removed_optional_agent_stays_removed_when_made_default() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    let t = make_template(
        &h,
        &operator,
        r#"{"slug":"inventory","name":"Inventory","availability":"optional"}"#,
    )
    .await;
    let id = t["id"].as_str().unwrap();
    let (_, body) = h
        .post(
            &format!("/v1/agent-templates/{id}/install"),
            Some(&admin),
            "",
        )
        .await;
    let agent_id = serde_json::from_str::<Value>(&body).unwrap()["agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = send_as(&h, "DELETE", &format!("/v1/agents/{agent_id}"), &admin, "").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send_as(
        &h,
        "PATCH",
        &format!("/v1/platform/agent-templates/{id}"),
        &operator,
        r#"{"availability":"default"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        template_agents(&h, &admin).await[0]["agent_id"].is_null(),
        "an agent the workspace removed came back"
    );
    finish!(h);
}

/// A template agent takes its template's slug, or the next free number after
/// it where the workspace already has an agent by that name.
#[tokio::test]
async fn a_template_agent_takes_the_next_free_slug() {
    let h = harness().await;
    let (_, operator, admin) = template_people(&h).await;
    for slug in ["invoicer", "invoicer-2"] {
        let (status, body) = h
            .post(
                "/v1/agents",
                Some(&admin),
                &format!(r#"{{"name":"{slug}","slug":"{slug}"}}"#),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    make_template(
        &h,
        &operator,
        r#"{"slug":"invoicer","name":"Invoicer","availability":"required"}"#,
    )
    .await;
    let agent_id = template_agents(&h, &admin).await[0]["agent_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h.get(&format!("/v1/agents/{agent_id}"), Some(&admin)).await;
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["slug"],
        "invoicer-3"
    );
    finish!(h);
}
