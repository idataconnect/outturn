//! Settings and how they cascade.

use super::*;

// -- Settings cascade -----------------------------------------------------------

/// A value walks down from the operator until a level overrides it, and a
/// cleared override falls back to whatever is above.
#[tokio::test]
async fn settings_cascade_from_operator_to_workspace_to_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    // Nothing set anywhere: the catalog default applies.
    let (status, body) = h.get("/v1/settings", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
    // Low rather than none: thinking off leaves a tool turn silent on some
    // models, so the catalog default was raised.
    assert_eq!(effort["value"], "low");
    assert_eq!(effort["source"], "default");

    // The operator sets a platform default.
    let req = |method: &str, uri: String, token: &str, body: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    };
    let (status, _) = h
        .send(req(
            "PUT",
            "/v1/platform/settings/reasoning_effort".into(),
            &root,
            r#"{"value":"low"}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // A workspace admin may not.
    let (status, _) = h
        .send(req(
            "PUT",
            "/v1/platform/settings/reasoning_effort".into(),
            &admin,
            r#"{"value":"high"}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The workspace now inherits it.
    let (_, body) = h.get("/v1/settings", Some(&admin)).await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
    assert_eq!(effort["value"], "low");
    assert_eq!(effort["source"], "operator");
    assert!(
        effort["override_value"].is_null(),
        "no row at the workspace level yet"
    );

    // The workspace overrides; an agent inherits the workspace's value.
    let (status, _) = h
        .send(req(
            "PUT",
            "/v1/settings/reasoning_effort".into(),
            &admin,
            r#"{"value":"high"}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");
    let (_, body) = h
        .get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin))
        .await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
    assert_eq!(effort["value"], "high");
    assert_eq!(effort["source"], "workspace");
    assert_eq!(effort["inherited"], "high");

    // Bad values are refused by the catalog, not stored.
    let (status, body) = h
        .send(req(
            "PUT",
            format!("/v1/agents/{agent_id}/settings/temperature"),
            &admin,
            r#"{"value":9}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // Clearing the workspace's override falls back to the operator's value.
    let (status, _) = h
        .send(req(
            "DELETE",
            "/v1/settings/reasoning_effort".into(),
            &admin,
            "",
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h
        .get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin))
        .await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
    assert_eq!(effort["value"], "low");
    assert_eq!(effort["source"], "operator");

    finish!(h);
}
