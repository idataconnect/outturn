//! Auto-approval policies through the API: who may set one, what is recorded
//! of who did, and the count the approval prompt reads. See
//! docs/auto-approval.md.

use super::*;
use outturn::api::role::RoleStore as _;

fn charge_policy() -> String {
    serde_json::json!({
        "selector": "act",
        "requires": "charge",
        "conditions": [{"field": "amount_pence", "op": "le", "value": 5000}],
        "per_turn": 2,
        "per_day": 20,
    })
    .to_string()
}

async fn put(h: &Harness, uri: &str, token: &str, body: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("PUT")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    h.send(req).await
}

#[tokio::test]
async fn an_admin_sets_a_policy_that_starts_in_trial_and_names_them() {
    let h = harness_or_skip!();
    let ws = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("alice@example.com", None, Some((ws, "admin")))
        .await;

    let (status, body) = h
        .post("/v1/approval-policies", Some(&admin), &charge_policy())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let created: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(created["mode"], "trial");
    assert_eq!(created["state"], "trial");
    assert_eq!(created["created_by_name"], "Test User");
    assert_eq!(created["version"], 1);
    // Ninety days unless somebody says otherwise; never for ever.
    let expires: chrono::DateTime<chrono::Utc> =
        created["expires_at"].as_str().unwrap().parse().unwrap();
    assert!(expires > chrono::Utc::now() + chrono::Duration::days(89));
    let id = created["id"].as_str().unwrap().to_string();

    // Switched on by somebody choosing to, which is an edit and moves the
    // version on.
    let mut on: Value = serde_json::from_str(&charge_policy()).unwrap();
    on["mode"] = "on".into();
    let (status, body) = put(
        &h,
        &format!("/v1/approval-policies/{id}"),
        &admin,
        &on.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let edited: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(edited["state"], "on");
    assert_eq!(edited["version"], 2);

    let (status, body) = h
        .post(
            &format!("/v1/approval-policies/{id}/revoke"),
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let revoked: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(revoked["state"], "revoked");
    assert_eq!(revoked["revoked_by_name"], "Test User");

    // A revoked policy is not edited back to life.
    let (status, _) = put(
        &h,
        &format!("/v1/approval-policies/{id}"),
        &admin,
        &charge_policy(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    finish!(h);
}

#[tokio::test]
async fn a_policy_needs_both_authorities_and_caps() {
    let h = harness_or_skip!();
    let ws = h.make_workspace("Acme", "acme").await;
    let viewer = h
        .login_as("vic@example.com", None, Some((ws, "viewer")))
        .await;
    let (status, _) = h
        .post("/v1/approval-policies", Some(&viewer), &charge_policy())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = h.get("/v1/approval-policies", Some(&viewer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Delegating without being able to answer is not enough.
    h.roles
        .create(
            ws,
            outturn::api::role::CreateRole {
                name: "delegate-only".into(),
                description: String::new(),
                authorities: vec!["approvals:delegate".into()],
            },
        )
        .await
        .expect("role");
    let lone = h
        .login_as("dee@example.com", None, Some((ws, "delegate-only")))
        .await;
    let (status, _) = h
        .post("/v1/approval-policies", Some(&lone), &charge_policy())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let admin = h
        .login_as("alice@example.com", None, Some((ws, "admin")))
        .await;
    let uncapped = serde_json::json!({"selector": "act", "requires": "charge"}).to_string();
    let (status, _) = h
        .post("/v1/approval-policies", Some(&admin), &uncapped)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    finish!(h);
}

#[tokio::test]
async fn a_policy_is_its_workspaces_alone() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let other = h.make_workspace("Other", "other").await;
    let alice = h
        .login_as("alice@example.com", None, Some((acme, "admin")))
        .await;
    let bob = h
        .login_as("bob@example.com", None, Some((other, "admin")))
        .await;
    let (_, body) = h
        .post("/v1/approval-policies", Some(&alice), &charge_policy())
        .await;
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = h.get("/v1/approval-policies", Some(&bob)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(items(&body).is_empty());
    let (status, _) = h
        .get(&format!("/v1/approval-policies/{id}"), Some(&bob))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h
        .post(
            &format!("/v1/approval-policies/{id}/revoke"),
            Some(&bob),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    finish!(h);
}

#[tokio::test]
async fn the_prompt_count_is_this_workspaces_yeses_to_one_act() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let other = h.make_workspace("Other", "other").await;
    let alice = h
        .login_as("alice@example.com", None, Some((acme, "admin")))
        .await;
    for (ws, kind, state) in [
        (acme, "approval.charge", "resolved"),
        (acme, "approval.charge", "resolved"),
        (acme, "approval.charge", "resolved"),
        (acme, "approval.charge", "cancelled"),
        (acme, "approval.refund", "resolved"),
        (other, "approval.charge", "resolved"),
    ] {
        sqlx::query(
            "insert into action_items (workspace_id, id, kind, state) values ($1, $2, $3, $4)",
        )
        .bind(ws)
        .bind(Uuid::now_v7())
        .bind(kind)
        .bind(state)
        .execute(&h.db.pool)
        .await
        .expect("item");
    }

    let (status, body) = h
        .get("/v1/approvals/answered?requires=charge", Some(&alice))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answered: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(answered["approved"], 3);
    assert_eq!(answered["may_delegate"], true);

    let viewer = h
        .login_as("vic@example.com", None, Some((acme, "viewer")))
        .await;
    let (status, _) = h
        .get("/v1/approvals/answered?requires=charge", Some(&viewer))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    finish!(h);
}
