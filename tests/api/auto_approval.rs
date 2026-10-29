//! Auto-approval policies through the API: who may set one, what is recorded
//! of who did, and the count the approval prompt reads. See
//! docs/auto-approval.md.

use super::*;

fn charge_policy() -> String {
    serde_json::json!({
        "selector": "risk",
        "max_risk": "low",
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
    let uncapped = serde_json::json!({"selector": "risk", "max_risk": "low"}).to_string();
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
    a_policy_on(h, &charge_file(""), &charge_policy()).await
}

/// The same, for any operation file and any policy.
async fn a_policy_on(h: &Harness, file: &str, written: &str) -> Covered {
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
                "files": [{ "path": "charge.md", "content": file }],
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

    let (status, body) = h.post("/v1/approval-policies", Some(&admin), written).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let policy = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut on: Value = serde_json::from_str(written).unwrap();
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
        let body = serde_json::json!({
            "booking_id": "bk_1", "amount_pence": amount, "description": "deposit",
        });
        self.send(
            acme,
            assignment,
            token,
            gates,
            "POST",
            "/charges",
            Some(body),
        )
        .await
    }

    /// Any request to the skill's host, as the runtime relays one.
    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        acme: Uuid,
        assignment: &Value,
        token: &str,
        gates: &Value,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        let rules: Vec<outturn::egress::rule::EgressRule> =
            serde_json::from_value(assignment["egress"].clone()).expect("rules");
        let rule = rules
            .iter()
            .find(|r| r.host == CHARGE_HOST)
            .expect("the skill's host is allowed")
            .clone();
        let mut body = serde_json::json!({
            "method": method,
            "url": format!("https://{CHARGE_HOST}{path}"),
            "gates": gates,
            "headers": [["content-type", "application/json"]],
            "body": body.map(|b| b.to_string()),
            "proof": outturn::egress::commit::prove(acme, &rules, &rule).unwrap(),
        });
        if body["body"].is_null() {
            body.as_object_mut().unwrap().remove("body");
        }
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

/// The open `policy.accept` items, as (policy, operations).
async fn waiting_items(h: &Harness) -> Vec<(String, Vec<String>)> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "select payload from action_items \
         where kind = 'policy.accept' and state = 'pending' order by id",
    )
    .fetch_all(&h.db.pool)
    .await
    .unwrap();
    rows.iter()
        .map(|p| {
            (
                p["policy_id"].as_str().unwrap().to_string(),
                p["operations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|o| o.as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect()
}

async fn publish(h: &Harness, c: &Covered, note: &str) {
    let (status, body) = h
        .post(
            &format!("/v1/skills/{}/versions", c.skill),
            Some(&c.admin),
            &serde_json::json!({
                "body": "Read charge.md before charging anybody.",
                "hosts": [CHARGE_HOST],
                "files": [{ "path": "charge.md", "content": charge_file(note) }],
            })
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// Publishing an operation a broad policy would cover says so in the queue of
/// whoever may set policies: once per policy however often it is published,
/// naming the operation, until it is accepted -- or the policy is revoked.
#[tokio::test]
async fn the_queue_says_when_operations_wait_on_a_broad_policy() {
    let h = harness_or_skip!();
    let c = a_policy_on_for_charges(&h).await;
    assert!(
        waiting_items(&h).await.is_empty(),
        "accepted with the operation already published"
    );

    publish(&h, &c, "Version two.").await;
    assert_eq!(
        waiting_items(&h).await,
        vec![(
            c.policy.clone(),
            vec!["POST /charges in Charging".to_string()]
        )]
    );
    // In the queue of the person who may accept it, saying so.
    let (status, body) = h.get("/v1/action-items", Some(&c.admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let queue: Value = serde_json::from_str(&body).unwrap();
    let item = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "policy.accept")
        .expect("in the admin's queue")
        .clone();
    assert!(
        item["payload"]["summary"].as_str().unwrap().contains("low"),
        "{item}"
    );

    // Published again: still one item, the same one.
    publish(&h, &c, "Version three.").await;
    let (status, body) = h.get("/v1/action-items", Some(&c.admin)).await;
    assert_eq!(status, StatusCode::OK);
    let queue: Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<&Value> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "policy.accept")
        .map(|i| &i["id"])
        .collect();
    assert_eq!(ids, vec![&item["id"]], "one item per policy");

    // Accepted: settled as done.
    let (status, body) = h
        .post(
            &format!("/v1/approval-policies/{}/accept", c.policy),
            Some(&c.admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(waiting_items(&h).await.is_empty());
    let state: String = sqlx::query_scalar("select state from action_items where id = $1::uuid")
        .bind(item["id"].as_str().unwrap())
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(state, "resolved");

    // Waiting again, then revoked: nobody's to accept any more.
    publish(&h, &c, "Version four.").await;
    assert_eq!(waiting_items(&h).await.len(), 1);
    let (status, _) = h
        .post(
            &format!("/v1/approval-policies/{}/revoke", c.policy),
            Some(&c.admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(waiting_items(&h).await.is_empty());
    let cancelled: i64 = sqlx::query_scalar(
        "select count(*) from action_items where kind = 'policy.accept' and state = 'cancelled'",
    )
    .fetch_one(&h.db.pool)
    .await
    .unwrap();
    assert_eq!(cancelled, 1);
    finish!(h);
}

/// A charge whose base risk is high, with the operator's bands: low below
/// £50, medium below £200, and high from £5,000 whatever a workspace says.
fn money_charge_file() -> String {
    "---\napproval:\n  requires: charge\n  matches: POST /charges\n  \
     binds: [booking_id, amount_pence]\n  risk: high\n  auto: allowed\n  \
     numbers: [amount_pence]\n  free: [description]\n  \
     amount: { field: amount_pence, currency: GBP, minor_units: 2 }\n  \
     bands: { low: 5000, medium: 20000 }\n  floor: { high: 500000 }\n---\n\n# charge\n"
        .to_string()
}

/// "Charges up to low go through, up to £150 a day": the one line, as the API
/// takes it.
fn low_charges_policy() -> String {
    serde_json::json!({
        "selector": "risk",
        "max_risk": "low",
        "daily_totals": {"GBP": 15000},
        "per_turn": 10,
        "per_day": 50,
    })
    .to_string()
}

/// A charge's risk is worked out at the gateway from the bands the token
/// commits: below the low band it goes out, above it a person is asked, and the
/// day's total in pounds stops what four small charges add up to. A
/// workspace's own band moves where a charge falls from the next turn on.
#[tokio::test]
async fn a_charge_is_as_risky_as_its_amount() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    let (_, _, assignment) = a_turn(&h, &c).await;
    let token = assignment["gateway_token"].as_str().unwrap();
    let gateway = Gateway::new(&h);
    let charge = |amount| gateway.charge(c.acme, &assignment, token, &assignment["gates"], amount);

    let (status, body) = charge(4000).await;
    assert_eq!(status, StatusCode::OK, "£40 is low: {body}");
    let (status, body) = charge(6000).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "£60 is medium: {body}");
    assert!(body.contains("needs approval"), "{body}");
    assert_eq!(charge(4000).await.0, StatusCode::OK);
    assert_eq!(charge(4000).await.0, StatusCode::OK);
    let (status, body) = charge(4000).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "£160 is over the day's £150: {body}"
    );
    assert_eq!(gateway.sent(), 3);
    let recorded: (i64, Option<String>) =
        sqlx::query_as("select sum(currency_amount)::bigint, min(currency) from auto_approvals")
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    assert_eq!(recorded, (12000, Some("GBP".to_string())));

    finish!(h);
}

/// The guesthouse's charge as it is published: an operation the skill calls,
/// keyed on the booking, carrying an idempotency key nobody binds.
fn guesthouse_charge_file() -> String {
    format!(
        "---\napproval:\n  requires: charge\n  matches: POST /charges\n  \
         binds: [payment_account_id, booking_id, amount_pence]\n  covers: booking\n  \
         identified_by: booking_id\n  risk: high\n  auto: allowed\n  \
         numbers: [amount_pence]\n  \
         amount: {{ field: amount_pence, currency: GBP, minor_units: 2 }}\n  \
         bands: {{ low: 5000, medium: 20000 }}\n  free: [idempotency_key]\n\
         operation:\n  method: POST\n  url: https://{CHARGE_HOST}/charges\n  \
         body: [payment_account_id, booking_id, amount_pence]\n  \
         integer: [amount_pence]\n  identity: [booking_id, amount_pence]\n  \
         key: field idempotency_key\n---\n\n# charge_payment_account\n"
    )
}

/// A rule on the charge for one agent -- under £100, with a day's total in
/// pounds -- over a broad policy for low risk. Every request of
/// the turn after -- the ungated look at the rooms as much as the charge --
/// reaches the gateway with the gates as the runtime carries them: the bytes
/// the API wrote, relayed untouched.
#[tokio::test]
async fn a_turn_under_a_rule_reaches_its_gates() {
    let h = harness_or_skip!();
    let (c, assignment) = a_turn_under_a_rule(&h).await;
    let token = assignment["gateway_token"].as_str().unwrap();
    // Read as the runtime reads an assignment: the request from the same
    // bytes, and its gates parsed for the runtime's own questions.
    let taken: outturn::protocol::work::ExecuteRequest =
        serde_json::from_str(&assignment.to_string()).expect("assignment");
    let parsed = taken.gates.parse().expect("gates");
    assert_eq!(parsed.policies().len(), 1, "the policy is committed");
    assert!(parsed.policies()[0].banding.is_some(), "with its bands");
    let carried = serde_json::to_value(&taken.gates).unwrap();

    let gateway = Gateway::new(&h);
    let (status, body) = gateway
        .send(c.acme, &assignment, token, &carried, "GET", "/rooms", None)
        .await;
    assert_eq!(status, StatusCode::OK, "nothing gates a look: {body}");
    let charge = |amount: i64| {
        gateway.send(
            c.acme,
            &assignment,
            token,
            &carried,
            "POST",
            "/charges",
            Some(serde_json::json!({
                "payment_account_id": "pa_1", "booking_id": "bk_1",
                "amount_pence": amount, "idempotency_key": format!("k{amount}"),
            })),
        )
    };
    let (status, body) = charge(8000).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "£80 is under the rule's £100: {body}"
    );
    let (status, body) = charge(15000).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "£150 is not: {body}");
    finish!(h);
}

/// A rule set on the charge for the agent, and a turn under it taken as a
/// runtime takes one.
async fn a_turn_under_a_rule(h: &Harness) -> (Covered, Value) {
    let c = a_policy_on(h, &guesthouse_charge_file(), &low_charges_policy()).await;
    let (status, body) = put(
        h,
        "/v1/approval-rules",
        &c.admin,
        &serde_json::json!({
            "operations": [{"skill_id": c.skill, "requires": "charge", "matches": "POST /charges"}],
            "agent_id": c.agent, "automatic": true, "under": 10000, "daily_total": 50000,
            "mode": "on",
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, _, assignment) = a_turn(h, &c).await;
    (c, assignment)
}

/// A runtime built before a field was added to the gate set relays the set
/// the API wrote, not the set its own types can hold. Stood in for here by a
/// field no build knows, put into the policy's grant where the API would have
/// written it: the request carries it to the gateway untouched and is let
/// through. The set an older runtime used to send instead -- read into its
/// types, written out without what they lacked -- is every request refused,
/// and so is one byte changed in what the API wrote.
#[tokio::test]
async fn an_older_runtime_relays_gates_it_cannot_read() {
    let h = harness_or_skip!();
    let (c, mut assignment) = a_turn_under_a_rule(&h).await;
    let token = assignment["gateway_token"].as_str().unwrap().to_string();
    assignment["gates"]["policies"][0]["added_later"] = serde_json::json!({"kept": true});
    let wrote = assignment.to_string();

    // What the runtime does with it: the request read from those bytes, and
    // its gates written into the body it posts to the gateway.
    let taken: outturn::protocol::work::ExecuteRequest =
        serde_json::from_str(&wrote).expect("assignment");
    assert!(taken.gates.parse().is_ok(), "the runtime can still read it");
    let rules: Vec<outturn::egress::rule::EgressRule> =
        serde_json::from_value(assignment["egress"].clone()).expect("rules");
    let rule = rules
        .iter()
        .find(|r| r.host == CHARGE_HOST)
        .unwrap()
        .clone();
    let posted = serde_json::to_string(&outturn::runtime::fetch::GatewayFetch {
        method: "GET".into(),
        url: format!("https://{CHARGE_HOST}/rooms"),
        headers: Vec::new(),
        body: None,
        proof: outturn::egress::commit::prove(c.acme, &rules, &rule).unwrap(),
        gates: taken.gates,
    })
    .unwrap();
    let sent = serde_json::to_string(&assignment["gates"]).unwrap();
    assert!(posted.contains(&sent), "relayed as written: {posted}");
    let relayed: Value = serde_json::from_str(&posted).unwrap();

    let gateway = Gateway::new(&h);
    let look = |gates: Value| {
        let assignment = &assignment;
        let token = &token;
        let gateway = &gateway;
        async move {
            gateway
                .send(c.acme, assignment, token, &gates, "GET", "/rooms", None)
                .await
        }
    };
    let (status, body) = look(relayed["gates"].clone()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the relayed set is the API's: {body}"
    );

    // Read into types that predate the bands and written out again.
    let mut reencoded = relayed["gates"].clone();
    let grant = reencoded["policies"][0].as_object_mut().unwrap();
    for later in ["added_later", "banding", "max_risk", "currency_per_day"] {
        grant.remove(later);
    }
    let (status, body) = look(reencoded).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("not the ones this turn was given"), "{body}");

    let mut altered = relayed["gates"].clone();
    let per_day = altered["policies"][0]["per_day"].as_i64().unwrap();
    altered["policies"][0]["per_day"] = (per_day + 1).into();
    let (status, body) = look(altered).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    finish!(h);
}

/// The page's record of past charges is replayed under the bands as they
/// stand: a policy for low-risk charges does not say it would have approved
/// £290 because no test of its own said otherwise, and says why not.
#[tokio::test]
async fn the_record_says_what_risk_stopped() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    // The history is of one operation, which is what it can replay.
    sqlx::query(
        "update approval_policies set selector = 'operation', requires = 'charge', \
                skill_id = $2, matches = 'POST /charges' where id = $1",
    )
    .bind(c.policy.parse::<Uuid>().unwrap())
    .bind(c.skill.parse::<Uuid>().unwrap())
    .execute(&h.db.pool)
    .await
    .unwrap();
    for pence in [29000, 4000] {
        sqlx::query(
            "insert into action_items (workspace_id, id, kind, state, payload) \
             values ($1, $2, 'approval.charge', 'resolved', $3)",
        )
        .bind(c.acme)
        .bind(Uuid::now_v7())
        .bind(serde_json::json!({
            "requires": "charge", "method": "POST", "host": CHARGE_HOST, "path": "/charges",
            "binds": [
                {"field": "booking_id", "value": "bk_1"},
                {"field": "amount_pence", "value": pence},
            ],
        }))
        .execute(&h.db.pool)
        .await
        .expect("item");
    }
    let (status, body) = h
        .get(
            &format!("/v1/approval-policies/{}/evidence", c.policy),
            Some(&c.admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let evidence: Value = serde_json::from_str(&body).unwrap();
    let history = evidence["history"].as_array().expect("a history");
    assert_eq!(history.len(), 2, "{body}");
    assert_eq!(history[0]["would"], true, "£40 is low: {body}");
    assert_eq!(history[1]["would"], false, "{body}");
    assert_eq!(history[1]["why"], "£290.00 is high risk");
    finish!(h);
}

/// Rules are set as policies are: by a person holding both authorities, from
/// their own session, on operations their own workspace's agents reach.
#[tokio::test]
async fn rules_are_set_by_whoever_may_set_a_policy() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    let rule = serde_json::json!({
        "operations": [{"skill_id": c.skill, "requires": "charge", "matches": "POST /charges"}],
        "automatic": true, "under": 1000,
    })
    .to_string();
    let viewer = h
        .login_as("vic@example.com", None, Some((c.acme, "viewer")))
        .await;
    let (status, _) = h.get("/v1/approval-rules", Some(&viewer)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = put(&h, "/v1/approval-rules", &viewer, &rule).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let other = h.make_workspace("Other", "other").await;
    let bob = h
        .login_as("bob@example.com", None, Some((other, "admin")))
        .await;
    let (status, body) = put(&h, "/v1/approval-rules", &bob, &rule).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "no agent of theirs reaches it: {body}"
    );
    let (status, body) = h.get("/v1/approval-rules", Some(&bob)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: Value = serde_json::from_str(&body).unwrap();
    assert!(page["operations"].as_array().unwrap().is_empty());
    finish!(h);
}

/// A policy over every charge and a workspace band become a rule on the
/// charge that lets through what they did and no more: the band said low
/// below £30, under the operator's £50, so the rule says under £30.
#[tokio::test]
async fn act_policies_and_bands_become_rules_without_widening() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    // As a workspace had them before rules: the policy over the act, and its
    // own band on the charge.
    sqlx::query("update approval_policies set selector = 'act', requires = 'charge' where id = $1")
        .bind(c.policy.parse::<Uuid>().unwrap())
        .execute(&h.db.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into approval_bands (workspace_id, id, currency, skill_id, requires, matches, low)          values ($1, $2, 'GBP', $3, 'charge', 'POST /charges', 3000)",
    )
    .bind(c.acme)
    .bind(Uuid::now_v7())
    .bind(c.skill.parse::<Uuid>().unwrap())
    .execute(&h.db.pool)
    .await
    .unwrap();

    outturn::api::auto_approval::convert::run(&h.state).await;
    // Again, which finds nothing left to do.
    outturn::api::auto_approval::convert::run(&h.state).await;

    let (status, body) = h.get("/v1/approval-rules", Some(&c.admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: Value = serde_json::from_str(&body).unwrap();
    let rules = page["operations"][0]["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1, "{body}");
    assert_eq!(rules[0]["selector"], "operation");
    assert_eq!(rules[0]["under"]["amount"], 3000, "the band's £30, not £50");
    assert_eq!(rules[0]["daily_total"]["amount"], 15000, "its total kept");
    assert_eq!(rules[0]["state"], "on");
    let (acts, bands): (i64, i64) = sqlx::query_as(
        "select (select count(*) from approval_policies \
                 where selector = 'act' and revoked_at is null), \
                (select count(*) from approval_bands)",
    )
    .fetch_one(&h.db.pool)
    .await
    .unwrap();
    assert_eq!((acts, bands), (0, 0));

    let (_, _, assignment) = a_turn(&h, &c).await;
    let token = assignment["gateway_token"].as_str().unwrap();
    let gateway = Gateway::new(&h);
    let charge = |amount| gateway.charge(c.acme, &assignment, token, &assignment["gates"], amount);
    assert_eq!(
        charge(2500).await.0,
        StatusCode::OK,
        "£25 went before and goes now"
    );
    assert_eq!(
        charge(4000).await.0,
        StatusCode::FORBIDDEN,
        "£40 asked before and asks now"
    );
    finish!(h);
}

/// The approval waiting on a conversation, by id.
async fn pending_on(h: &Harness, session: &str) -> Value {
    let id: Uuid = sqlx::query_scalar(
        "select id from action_items where payload->>'session_id' = $1 and state = 'pending'",
    )
    .bind(session)
    .fetch_one(&h.db.pool)
    .await
    .unwrap();
    Value::String(id.to_string())
}

/// Relays a refused charge of this many pence, as the runtime does.
async fn raise_charge(
    h: &Harness,
    job: Uuid,
    lease: Uuid,
    session: Uuid,
    pence: i64,
) -> (StatusCode, String) {
    let body = serde_json::json!({"booking_id": "bk_1", "amount_pence": pence}).to_string();
    h.send(
        Request::post("/v1/work/gated")
            .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
            .header(outturn::api::work::LEASE_HEADER, lease.to_string())
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "session_id": session, "job_id": job, "method": "POST",
                    "host": CHARGE_HOST, "path": "/charges", "body": body,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await
}

/// Raises an approval for a refused charge of this many pence, as the runtime
/// relays one, and returns the queue item's payload and targets.
async fn refuse_charge(h: &Harness, job: Uuid, lease: Uuid, pence: i64) -> (Value, Vec<Uuid>) {
    let session: Uuid =
        sqlx::query_scalar("select (payload->>'session_id')::uuid from jobs where id = $1")
            .bind(job)
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    let (status, raised) = raise_charge(h, job, lease, session, pence).await;
    assert_eq!(status, StatusCode::OK, "{raised}");
    let item: Uuid = serde_json::from_str::<Value>(&raised).unwrap()["item_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let payload: Value = sqlx::query_scalar("select payload from action_items where id = $1")
        .bind(item)
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    let targets: Vec<Uuid> =
        sqlx::query_scalar("select role_id from action_targets where item_id = $1 order by 1")
            .bind(item)
            .fetch_all(&h.db.pool)
            .await
            .unwrap();
    (payload, targets)
}

/// A question says how risky the request worked out to and what it costs, as
/// money; a high one goes to the roles the workspace named for it, and the
/// offer of a policy is drawn from the amounts people approved.
#[tokio::test]
async fn a_high_charge_asks_whoever_the_workspace_named() {
    use outturn::api::role::CreateRole;
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    let admin_role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(c.acme)
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    let manager = h
        .roles
        .create(
            c.acme,
            CreateRole {
                name: "manager".into(),
                description: String::new(),
                authorities: vec!["approvals:answer".into()],
            },
        )
        .await
        .expect("role")
        .id;
    let bystander = h
        .roles
        .create(
            c.acme,
            CreateRole {
                name: "bystander".into(),
                description: String::new(),
                authorities: vec!["agents:read".into()],
            },
        )
        .await
        .expect("role")
        .id;

    // Only a role that may answer may be named.
    let (status, _) = put(
        &h,
        "/v1/approval-routes/high",
        &c.admin,
        &serde_json::json!({ "role_ids": [bystander] }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = put(
        &h,
        "/v1/approval-routes/high",
        &c.admin,
        &serde_json::json!({ "role_ids": [manager] }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let routes: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(routes["high"][0]["name"], "manager");

    // £300 is high: the manager is asked, and only the manager.
    let (job, lease, _) = a_turn(&h, &c).await;
    let (payload, targets) = refuse_charge(&h, job, lease, 30000).await;
    assert_eq!(payload["risk"], "high");
    assert_eq!(payload["money"]["display"], "£300.00");
    assert_eq!(payload["money"]["field"], "amount_pence");
    assert_eq!(targets, vec![manager]);

    // And the conversation's own prompt says what the inbox says: the risk, and
    // the amount as money beside the field it came from.
    let session = payload["session_id"].as_str().unwrap();
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session}/messages"),
            Some(&c.admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let history: Value = serde_json::from_str(&body).unwrap();
    let awaiting = &history["awaiting"];
    assert_eq!(awaiting["item_id"], pending_on(&h, session).await, "{body}");
    assert_eq!(awaiting["risk"], "high", "{body}");
    assert_eq!(awaiting["money"]["display"], "£300.00", "{body}");
    assert_eq!(awaiting["money"]["field"], "amount_pence", "{body}");
    assert_eq!(
        awaiting["money"], payload["money"],
        "one money, both places"
    );

    // £60 is medium: everyone who answers approvals, as before.
    let (job, lease, _) = a_turn(&h, &c).await;
    let (payload, targets) = refuse_charge(&h, job, lease, 6000).await;
    assert_eq!(payload["risk"], "medium");
    let mut everyone = vec![admin_role, manager];
    everyone.sort();
    let mut targets = targets;
    targets.sort();
    assert_eq!(targets, everyone);

    // The offer: approved amounts, per currency, with a figure above them.
    sqlx::query("update action_items set state = 'resolved' where kind = 'approval.charge'")
        .execute(&h.db.pool)
        .await
        .unwrap();
    let (status, body) = h
        .get("/v1/approvals/amounts?requires=charge", Some(&c.admin))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let amounts: Value = serde_json::from_str(&body).unwrap();
    let pounds = &amounts["currencies"][0];
    assert_eq!(pounds["currency"], "GBP");
    assert_eq!(pounds["approved"], 2);
    assert_eq!(pounds["max"]["display"], "£300.00");
    assert_eq!(pounds["suggested_low"]["display"], "£500.00");
    // And the round figure every one was under, which the offer says.
    assert_eq!(pounds["all_under"]["display"], "£400.00");
    assert_eq!(pounds["suggested_low"]["figure"], "500");
    assert_eq!(amounts["may_delegate"], true);

    // Nothing named: high asks everyone again.
    let (status, _) = put(
        &h,
        "/v1/approval-routes/high",
        &c.admin,
        r#"{"role_ids":[]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (job, lease, _) = a_turn(&h, &c).await;
    let (_, targets) = refuse_charge(&h, job, lease, 30000).await;
    assert_eq!(targets.len(), 2);
    finish!(h);
}

/// A question whose risk cannot be worked out is asked as high -- of the
/// roles named for it, never everyone -- and one whose named roles cannot be
/// read is not raised at all.
#[tokio::test]
async fn a_risk_that_cannot_be_worked_out_never_asks_more_people() {
    use outturn::api::role::CreateRole;
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    let manager = h
        .roles
        .create(
            c.acme,
            CreateRole {
                name: "manager".into(),
                description: String::new(),
                authorities: vec!["approvals:answer".into()],
            },
        )
        .await
        .expect("role")
        .id;
    let (status, body) = put(
        &h,
        "/v1/approval-routes/high",
        &c.admin,
        &serde_json::json!({ "role_ids": [manager] }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The declarations unreadable once the turn has them: £60, medium when
    // they can be read, is asked of the manager alone, and the question claims
    // no risk it could not work out.
    let (job, lease, _) = a_turn(&h, &c).await;
    sqlx::query("alter table skill_version_gates rename column free to free_away")
        .execute(&h.db.pool)
        .await
        .unwrap();
    let (payload, targets) = refuse_charge(&h, job, lease, 6000).await;
    assert_eq!(targets, vec![manager]);
    assert!(payload.get("risk").is_none(), "{payload}");
    sqlx::query("alter table skill_version_gates rename column free_away to free")
        .execute(&h.db.pool)
        .await
        .unwrap();

    // Who is named for high unreadable: £300 is not asked of everyone instead.
    sqlx::query("alter table approval_routes rename to approval_routes_away")
        .execute(&h.db.pool)
        .await
        .unwrap();
    let (job, lease, _) = a_turn(&h, &c).await;
    let session: Uuid =
        sqlx::query_scalar("select (payload->>'session_id')::uuid from jobs where id = $1")
            .bind(job)
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    let (status, _) = raise_charge(&h, job, lease, session, 30000).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let items: i64 =
        sqlx::query_scalar("select count(*) from action_items where payload->>'session_id' = $1")
            .bind(session.to_string())
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    assert_eq!(items, 0, "nothing was raised");
    let held: i64 = sqlx::query_scalar("select count(*) from inhibitors where session_id = $1")
        .bind(session)
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(held, 0, "the hold was given back");
    sqlx::query("alter table approval_routes_away rename to approval_routes")
        .execute(&h.db.pool)
        .await
        .unwrap();
    finish!(h);
}

/// A delete by path: nothing in the body, and the invoice's id the whole of
/// what is approved. See docs/approvals.md, "Path parameters".
fn delete_invoice_file() -> String {
    "---\napproval:\n  requires: delete\n  matches: DELETE /invoices/{id}\n  binds: [id]\n  \
     risk: low\n  auto: allowed\n  free: []\n---\n\n# delete_invoice\n"
        .to_string()
}

/// A policy for deleting one invoice, named by the id in its path.
fn delete_one_invoice_policy() -> String {
    serde_json::json!({
        "selector": "risk",
        "max_risk": "low",
        "conditions": [{"field": "id", "op": "in", "value": ["inv_9"]}],
        "per_turn": 2,
        "per_day": 20,
    })
    .to_string()
}

/// A gate bound by its path parameter, all the way through: a policy tests
/// the id, a slash smuggled into it is refused rather than matched, a person
/// is asked about one invoice and shown its id, and what they approve is sent
/// as it was held -- while the same delete of another invoice, under that very
/// grant, is refused.
#[tokio::test]
async fn a_delete_by_path_is_gated_by_its_id() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &delete_invoice_file(), &delete_one_invoice_policy()).await;
    let (job, lease, assignment) = a_turn(&h, &c).await;
    let token = assignment["gateway_token"].as_str().expect("token");
    let gates = &assignment["gates"];
    let gateway = Gateway::new(&h);
    let delete = |path: &'static str, token: &str, gates: &Value| {
        let token = token.to_string();
        let gates = gates.clone();
        let gateway = &gateway;
        let assignment = &assignment;
        async move {
            gateway
                .send(c.acme, assignment, &token, &gates, "DELETE", path, None)
                .await
        }
    };

    // The policy names inv_9, read from the path, and the request goes out
    // with no body, as it came.
    let (status, body) = delete("/invoices/inv_9", token, gates).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(gateway.sent(), 1);
    assert_eq!(auto_approved(&h).await, 1);

    // A value that decodes to a slash is refused, not approved or asked about.
    let (status, body) = delete("/invoices/inv_9%2F..%2Finv_1", token, gates).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("cannot be approved"), "{body}");

    // Another invoice is not the policy's, so somebody is asked.
    let (status, body) = delete("/invoices/inv_1", token, gates).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("needs approval"), "{body}");
    assert_eq!(gateway.sent(), 1);

    let session: Uuid =
        sqlx::query_scalar("select (payload->>'session_id')::uuid from jobs where id = $1")
            .bind(job)
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    let url = format!("https://{CHARGE_HOST}/invoices/inv_1");
    let (status, raised) = h
        .send(
            Request::post("/v1/work/gated")
                .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
                .header(outturn::api::work::LEASE_HEADER, lease.to_string())
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "session_id": session, "job_id": job, "method": "DELETE",
                        "host": CHARGE_HOST, "path": "/invoices/inv_1", "url": url,
                        "headers": [],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{raised}");
    let item = serde_json::from_str::<Value>(&raised).unwrap()["item_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The person deciding is shown which invoice.
    let payload: Value = sqlx::query_scalar("select payload from action_items where id = $1::uuid")
        .bind(&item)
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(
        payload["binds"],
        serde_json::json!([{"field": "id", "value": "inv_1"}]),
        "{payload}"
    );

    outturn::jobs::park(&h.db.pool, job, Some(lease))
        .await
        .expect("park");
    let (status, body) = h
        .post(
            &format!("/v1/approvals/{item}/answer"),
            Some(&c.admin),
            r#"{"approved":true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The resumed turn is handed the delete as it was held, and a token to
    // send it under.
    let (_, _, resumed) = take_assignment(&h).await;
    let held = &resumed["held"];
    assert_eq!(held.as_array().map(Vec::len), Some(1), "{resumed}");
    assert_eq!(held[0]["method"], "DELETE");
    assert_eq!(held[0]["url"], url.as_str());
    let held_token = resumed["held_token"].as_str().expect("held token");
    let held_gates = &resumed["held_gates"];

    // Sent with the held request, it goes out.
    let (status, body) = delete("/invoices/inv_1", held_token, held_gates).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(gateway.sent(), 2);

    // The same grant does not reach another invoice.
    let (status, body) = delete("/invoices/inv_2", held_token, held_gates).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("needs approval"), "{body}");
    // Nor does the guest's own token carry it.
    let (status, body) = delete(
        "/invoices/inv_1",
        resumed["gateway_token"].as_str().unwrap(),
        &resumed["gates"],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(gateway.sent(), 2);
    finish!(h);
}

/// The rules page lists the operation an agent reaches, with the rule each
/// agent is under said as an amount: a policy up to low risk is one under the
/// low band, and the deployment's cap of medium is where no rule may reach.
#[tokio::test]
async fn the_rules_page_says_each_operations_rule_in_money() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;

    let (status, body) = h.get("/v1/approval-rules", Some(&c.admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["may_write"], true);
    let ops = page["operations"].as_array().unwrap();
    assert_eq!(ops.len(), 1, "{body}");
    let op = &ops[0];
    assert_eq!(op["skill_name"], "Charging");
    assert_eq!(op["matches"], "POST /charges");
    assert_eq!(op["name"], "Charge");
    assert_eq!(op["agents"][0]["name"], "Desk");
    assert_eq!(op["amount"]["currency"], "GBP");
    assert_eq!(op["fixed"]["never"], false);
    assert_eq!(op["fixed"]["most"]["amount"], 20000, "medium ends at £200");

    let rule = &op["rules"][0];
    assert_eq!(rule["policy_id"], c.policy);
    assert_eq!(rule["state"], "on");
    assert_eq!(rule["agents"][0]["id"], c.agent.to_string());
    assert_eq!(rule["under"]["amount"], 5000, "low ends at £50");
    assert_eq!(rule["daily_total"]["amount"], 15000);
    assert_eq!(op["asked"], 0);
}

/// A rule set on an operation is that operation's own, and is what the
/// gateway holds a charge to: under £30 goes, though the broader policy for
/// charges says under £50.
#[tokio::test]
async fn an_operations_own_rule_is_what_the_gateway_enforces() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    let rule = |body: Value| {
        let h = &h;
        let admin = c.admin.clone();
        async move { put(h, "/v1/approval-rules", &admin, &body.to_string()).await }
    };
    let op = serde_json::json!({
        "skill_id": c.skill, "requires": "charge", "matches": "POST /charges"
    });

    // Past where medium ends, which is all this deployment lets through.
    let (status, body) = rule(serde_json::json!({
        "operations": [op], "automatic": true, "under": 30000, "mode": "on"
    }))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("£200.00"), "{body}");

    let (status, body) = rule(serde_json::json!({
        "operations": [op], "automatic": true, "under": 3000,
        "daily_total": 10000, "mode": "on"
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: Value = serde_json::from_str(&body).unwrap();
    let r = &page["operations"][0]["rules"][0];
    assert_eq!(r["selector"], "operation");
    assert_eq!(r["under"]["amount"], 3000);
    assert_eq!(r["daily_total"]["amount"], 10000);

    let (_, _, assignment) = a_turn(&h, &c).await;
    let token = assignment["gateway_token"].as_str().unwrap();
    let gateway = Gateway::new(&h);
    let charge = |amount| gateway.charge(c.acme, &assignment, token, &assignment["gates"], amount);
    assert_eq!(charge(2500).await.0, StatusCode::OK, "£25 is under £30");
    let (status, body) = charge(4000).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "£40 is not: {body}");

    // Setting it again replaces it rather than adding a second.
    let (status, body) = rule(serde_json::json!({
        "operations": [op], "automatic": true, "under": 2000, "mode": "on"
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let live: i64 = sqlx::query_scalar(
        "select count(*) from approval_policies where selector = 'operation' and revoked_at is null",
    )
    .fetch_one(&h.db.pool)
    .await
    .unwrap();
    assert_eq!(live, 1);
}

/// "Always asks" on an operation a broader policy reaches excludes it, so a
/// person is asked whatever the broader policy says; and lifting it for one
/// agent leaves every other still asking.
#[tokio::test]
async fn always_asks_wins_over_a_broader_policy() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    let op = serde_json::json!({
        "skill_id": c.skill, "requires": "charge", "matches": "POST /charges"
    });
    let (status, body) = put(
        &h,
        "/v1/approval-rules",
        &c.admin,
        &serde_json::json!({ "operations": [op], "automatic": false }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        page["operations"][0]["rules"],
        serde_json::json!([]),
        "{body}"
    );

    let (_, _, assignment) = a_turn(&h, &c).await;
    let token = assignment["gateway_token"].as_str().unwrap();
    let gateway = Gateway::new(&h);
    let (status, body) = gateway
        .charge(c.acme, &assignment, token, &assignment["gates"], 1000)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "£10 asks now: {body}");

    // For one agent, automatic again; the exclusion for everyone goes.
    let (status, body) = put(
        &h,
        "/v1/approval-rules",
        &c.admin,
        &serde_json::json!({
            "operations": [op], "agent_id": c.agent, "automatic": true,
            "under": 1500, "mode": "on"
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        page["operations"][0]["rules"][0]["under"]["amount"], 1500,
        "{body}"
    );
}

/// The preview replays a rule against what its operation asked, and refuses
/// what the rule itself would be refused for.
#[tokio::test]
async fn a_rule_is_previewed_before_it_is_set() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    let q = format!(
        "/v1/approval-rules/preview?skill_id={}&requires=charge&matches=POST%20/charges",
        c.skill
    );
    let (status, body) = h.get(&format!("{q}&under=3000"), Some(&c.admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let preview: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(preview["days"], 30);
    assert_eq!(preview["would"], 0);
    let (status, _) = h.get(&format!("{q}&under=900000"), Some(&c.admin)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// An act policy another rule had already overridden is not brought back by
/// the conversion: the agent stays under the rule that was deciding for it.
#[tokio::test]
async fn an_overridden_act_policy_converts_to_nothing() {
    let h = harness_or_skip!();
    let c = a_policy_on(&h, &money_charge_file(), &low_charges_policy()).await;
    sqlx::query("update approval_policies set selector = 'act', requires = 'charge' where id = $1")
        .bind(c.policy.parse::<Uuid>().unwrap())
        .execute(&h.db.pool)
        .await
        .unwrap();
    let (status, body) = put(
        &h,
        "/v1/approval-rules",
        &c.admin,
        &serde_json::json!({
            "operations": [{"skill_id": c.skill, "requires": "charge", "matches": "POST /charges"}],
            "automatic": true, "under": 1000, "mode": "trial",
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    outturn::api::auto_approval::convert::run(&h.state).await;

    let (_, body) = h.get("/v1/approval-rules", Some(&c.admin)).await;
    let page: Value = serde_json::from_str(&body).unwrap();
    let rules = page["operations"][0]["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1, "{body}");
    assert_eq!(rules[0]["state"], "trial");
    assert_eq!(rules[0]["under"]["amount"], 1000);
    finish!(h);
}
