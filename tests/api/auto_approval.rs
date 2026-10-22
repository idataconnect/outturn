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

/// A literal public address, so the gateway asks no resolver about it.
const CHARGE_HOST: &str = "93.184.215.14";

fn charge_file(note: &str) -> String {
    format!(
        "---\napproval:\n  requires: charge\n  matches: POST /charges\n  \
         binds: [booking_id, amount_pence]\n  risk: low\n  auto: allowed\n  \
         numbers: [amount_pence]\n  free: [description]\n---\n\n# charge\n\n{note}\n"
    )
}

/// What a turn a policy speaks for starts from.
struct Covered {
    acme: Uuid,
    admin: String,
    skill: String,
    agent: Uuid,
    policy: String,
}

/// A deployment that lets policies cover low risk, a charging skill whose
/// operation may be auto-approved, an agent with it, and a policy on for
/// charges of fifty pounds or less -- set after the skill was published, so it
/// covers the operation without anybody accepting it.
async fn a_policy_on_for_charges(h: &Harness) -> Covered {
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("alice@example.com", None, Some((acme, "admin")))
        .await;
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = put(
        h,
        "/v1/platform/settings/auto_approve_max_risk",
        &root,
        r#"{"value":"medium"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            &serde_json::json!({
                "slug": "charging",
                "name": "Charging",
                "body": "Read charge.md before charging anybody.",
                "hosts": [CHARGE_HOST],
                "files": [{ "path": "charge.md", "content": charge_file("") }],
            })
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = h
        .post(
            &format!("/v1/skills/{skill}/hosts/approve"),
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"Desk","slug":"desk","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let agent: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = put(
        h,
        &format!("/v1/agents/{agent}/skills"),
        &admin,
        &format!(r#"[{{"skill_id":"{skill}"}}]"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = h
        .post("/v1/approval-policies", Some(&admin), &charge_policy())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let policy = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut on: Value = serde_json::from_str(&charge_policy()).unwrap();
    on["mode"] = "on".into();
    let (status, body) = put(
        h,
        &format!("/v1/approval-policies/{policy}"),
        &admin,
        &on.to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    Covered {
        acme,
        admin,
        skill,
        agent,
        policy,
    }
}

/// A conversation with the agent and its prompt's turn taken by a runtime.
async fn a_turn(h: &Harness, c: &Covered) -> (Uuid, Uuid, Value) {
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&c.admin),
            &format!(r#"{{"agent_id":"{}","title":""}}"#, c.agent),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session}/messages"),
            Some(&c.admin),
            r#"{"content":"Charge the deposit for bk_1."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    take_assignment(h).await
}

/// The gateway's real handler, with this test's database and the app's key,
/// sending nothing anywhere: what it would have sent is counted instead.
struct Gateway {
    routes: axum::Router,
    sent: Arc<std::sync::atomic::AtomicUsize>,
}

impl Gateway {
    fn new(h: &Harness) -> Self {
        use outturn::gateway::egress::transport::{
            EgressTransport, TransportError, TransportResponse, VettedRequest,
        };
        struct Counter(Arc<std::sync::atomic::AtomicUsize>);
        #[async_trait::async_trait]
        impl EgressTransport for Counter {
            fn name(&self) -> &'static str {
                "test"
            }
            async fn send(&self, _: VettedRequest) -> Result<TransportResponse, TransportError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(TransportResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: "{}".into(),
                    truncated: false,
                })
            }
        }
        let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let validator =
            TokenValidator::new(&h.public_key, outturn::auth::AUDIENCE_GATEWAY).unwrap();
        let routes = outturn::gateway::routes(Arc::new(
            outturn::gateway::GatewayState::new(Vec::new(), validator)
                .with_egress_transport(Arc::new(Counter(sent.clone())))
                .with_health(h.db.pool.clone()),
        ));
        Self { routes, sent }
    }

    fn sent(&self) -> usize {
        self.sent.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A charge, as the runtime relays one: under this token and these gates,
    /// with the rule it matched and the API's proof of it.
    async fn charge(
        &self,
        acme: Uuid,
        assignment: &Value,
        token: &str,
        gates: &Value,
        amount: i64,
    ) -> (StatusCode, String) {
        let rules: Vec<outturn::egress::rule::EgressRule> =
            serde_json::from_value(assignment["egress"].clone()).expect("rules");
        let rule = rules
            .iter()
            .find(|r| r.host == CHARGE_HOST)
            .expect("the skill's host is allowed")
            .clone();
        let body = serde_json::json!({
            "method": "POST",
            "url": format!("https://{CHARGE_HOST}/charges"),
            "gates": gates,
            "headers": [["content-type", "application/json"]],
            "body": serde_json::json!({
                "booking_id": "bk_1", "amount_pence": amount, "description": "deposit",
            })
            .to_string(),
            "proof": outturn::egress::commit::prove(acme, &rules, &rule).unwrap(),
        });
        let response = self
            .routes
            .clone()
            .oneshot(
                Request::post("/v1/egress")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }
}

async fn auto_approved(h: &Harness) -> i64 {
    sqlx::query_scalar("select count(*) from auto_approvals")
        .fetch_one(&h.db.pool)
        .await
        .unwrap()
}

/// The gate the policy lookup builds for an operation is the gate the turn
/// commits to, so the policy committed against it is found by the gateway --
/// and a charge within the policy goes out unasked while one above its limit
/// is refused for a person to answer.
///
/// Were the two gates ever built differently, the lookup would commit a policy
/// against a key no gate has, every request would ask a person, and nothing
/// else would fail: this is the test that notices the policy covering nothing.
#[tokio::test]
async fn a_policy_on_is_committed_against_the_gate_the_turn_carries() {
    use outturn::api::skill::SkillStore as _;
    let h = harness_or_skip!();
    let c = a_policy_on_for_charges(&h).await;
    let (_, _, assignment) = a_turn(&h, &c).await;

    let gates: outturn::egress::gate::Gates =
        serde_json::from_value(assignment["gates"].clone()).expect("gates");
    let token = assignment["gateway_token"].as_str().expect("token");
    let claims = h.gateway_validator.validate(token).expect("turn token");
    assert!(
        gates.matches(c.acme, &claims.gate_commitment.expect("gate commitment")),
        "the gates handed over are the ones the token commits to"
    );

    let skills = outturn::api::skill::PostgresSkillStore::new(h.db.pool.clone())
        .resolve_for_agent(c.acme, c.agent)
        .await
        .expect("skills");
    let declared = outturn::api::auto_approval::declared_for(&h.db.pool, &skills)
        .await
        .expect("declared");
    assert_eq!(declared.len(), 1, "one gated operation");
    let looked_up = &declared[0].gate;
    assert_eq!(
        gates.covering(CHARGE_HOST, "POST", "/charges"),
        Some(looked_up),
        "the gate the lookup builds is the one committed"
    );
    let committed = gates
        .policy_for(c.acme, looked_up)
        .expect("a policy is committed against that gate");
    assert_eq!(committed.policy_id.to_string(), c.policy);
    assert_eq!(gates.policies().len(), 1);

    let gateway = Gateway::new(&h);
    let (status, body) = gateway
        .charge(c.acme, &assignment, token, &assignment["gates"], 1200)
        .await;
    assert_eq!(status, StatusCode::OK, "within the policy: {body}");
    assert_eq!(gateway.sent(), 1);
    assert_eq!(auto_approved(&h).await, 1, "and recorded as the policy's");

    let (status, body) = gateway
        .charge(c.acme, &assignment, token, &assignment["gates"], 78000)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("needs approval"), "{body}");
    assert_eq!(gateway.sent(), 1, "above the limit, nothing went out");
    assert_eq!(auto_approved(&h).await, 1);
    finish!(h);
}

/// A policy revoked while a turn runs is gone from the token the turn trades
/// in for, and from the gates it is handed with it -- not merely refused by the
/// gateway's record on the next request.
#[tokio::test]
async fn a_refreshed_turn_token_drops_a_revoked_policy() {
    let h = harness_or_skip!();
    let c = a_policy_on_for_charges(&h).await;
    let (job, lease, assignment) = a_turn(&h, &c).await;
    let original = assignment["gateway_token"].as_str().unwrap().to_string();

    let refresh = |gates: Option<Value>, token: String| {
        let mut body = serde_json::json!({ "gateway_token": token });
        if let Some(gates) = gates {
            body["gates"] = gates;
        }
        h.send(
            Request::post(format!("/v1/work/{job}/token"))
                .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
                .header(outturn::api::work::LEASE_HEADER, lease.to_string())
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
    };

    // While it stands, nothing changes and no new set is handed back.
    let (status, body) = refresh(Some(assignment["gates"].clone()), original.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let same: Value = serde_json::from_str(&body).unwrap();
    assert!(same["gates"].is_null(), "{body}");

    let (status, body) = h
        .post(
            &format!("/v1/approval-policies/{}/revoke", c.policy),
            Some(&c.admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = refresh(Some(assignment["gates"].clone()), original.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refreshed: Value = serde_json::from_str(&body).unwrap();
    let narrowed: outturn::egress::gate::Gates =
        serde_json::from_value(refreshed["gates"].clone()).expect("a new set is handed back");
    assert!(
        narrowed.policies().is_empty(),
        "the revoked policy is gone: {body}"
    );
    let token = refreshed["gateway_token"].as_str().unwrap();
    let before = h.gateway_validator.validate(&original).unwrap();
    let after = h.gateway_validator.validate(token).unwrap();
    assert!(narrowed.matches(c.acme, &after.gate_commitment.unwrap()));
    assert_ne!(after.gate_commitment, before.gate_commitment);
    assert_eq!(after.egress_commitment, before.egress_commitment);

    // Under the new token a charge the policy would have let out asks a person,
    // as it would with no policy at all.
    let gateway = Gateway::new(&h);
    let (status, body) = gateway
        .charge(c.acme, &assignment, token, &refreshed["gates"], 1200)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("needs approval"), "{body}");
    assert_eq!(gateway.sent(), 0);

    // A set that is not the token's is refused rather than read.
    let (status, _) = refresh(Some(refreshed["gates"].clone()), original.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // And an older runtime, sending none, has its commitment copied forward.
    let (status, body) = refresh(None, original.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let copied: Value = serde_json::from_str(&body).unwrap();
    let copied = h
        .gateway_validator
        .validate(copied["gateway_token"].as_str().unwrap())
        .unwrap();
    assert_eq!(copied.gate_commitment, before.gate_commitment);
    finish!(h);
}
