//! Sealed credentials and where they go.

use super::*;

/// A sealed credential goes to the workspace and host it was sealed for, in its
/// header, through the gateway's real handler -- and to nobody else, and not at
/// all once revoked. The API never holds the plaintext: the test seals it as
/// the browser or `outturn-seal` would.
#[tokio::test]
async fn a_sealed_credential_goes_where_it_was_bound_and_nowhere_else() {
    use outturn::egress::seal;
    use outturn::gateway::egress::transport::{
        EgressTransport, TransportError, TransportResponse, VettedRequest,
    };
    use std::sync::Mutex;
    use tower::ServiceExt as _;

    struct Recorder(Mutex<Vec<(String, Option<String>)>>);
    #[async_trait::async_trait]
    impl EgressTransport for Recorder {
        fn name(&self) -> &'static str {
            "test"
        }
        async fn send(&self, request: VettedRequest) -> Result<TransportResponse, TransportError> {
            let auth = request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            self.0.lock().unwrap().push((request.url.to_string(), auth));
            Ok(TransportResponse {
                status: 200,
                headers: Vec::new(),
                body: "{}".into(),
                truncated: false,
            })
        }
    }

    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let other = h.make_workspace("Other", "other").await;

    let (status, body) = h.get("/v1/credentials/seal-key", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key: serde_json::Value = serde_json::from_str(&body).unwrap();
    let public = hex::decode(key["public_key"].as_str().unwrap()).unwrap();
    let key_id = key["key_id"].as_str().unwrap().to_string();

    // Literal public addresses, so nothing asks a resolver.
    let host = "93.184.215.14";
    let id = Uuid::now_v7();
    let binding_for = |workspace: Uuid| {
        serde_json::to_vec(&serde_json::json!({
            "credential": id, "kind": "static", "workspaces": [workspace],
            "hosts": [host], "header": "authorization",
        }))
        .unwrap()
    };
    let b64 = |b: &[u8]| {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(b)
    };
    let body_for = |binding: &[u8], key_id: &str| {
        let sealed = seal::seal(&public, binding, b"sk_test_acme").unwrap();
        serde_json::json!({
            "id": id, "name": "Books", "binding": b64(binding),
            "sealed": b64(&sealed), "key_id": key_id,
        })
        .to_string()
    };

    // Refused while somebody is looking: bound to another workspace, or
    // sealed to a key this deployment does not publish.
    let (status, _) = h
        .post(
            "/v1/credentials",
            Some(&admin),
            &body_for(&binding_for(other), &key_id),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a binding to another workspace"
    );
    let (status, _) = h
        .post(
            "/v1/credentials",
            Some(&admin),
            &body_for(&binding_for(acme), "0000"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a seal to another key");

    let (status, body) = h
        .post(
            "/v1/credentials",
            Some(&admin),
            &body_for(&binding_for(acme), &key_id),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        !body.contains("sealed"),
        "the ciphertext is not returned: {body}"
    );

    // A rule may name it only for the host and header it was sealed for.
    let (status, _) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": "93.184.215.66", "header": "authorization", "credential": id}).to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a rule for another host");
    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": host, "header": "authorization", "credential": id})
                .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Taking it off the rule and putting it back, as connecting a key to a
    // host somebody already allowed does.
    let rule_id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let patch = |body: serde_json::Value| {
        Request::patch(format!("/v1/egress-rules/{rule_id}"))
            .header("authorization", format!("Bearer {admin}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (status, body) = h.send(patch(serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(r#""credential":null"#), "{body}");
    let (status, _) = h
        .send(patch(
            serde_json::json!({"header": "x-api-key", "credential": id}),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a header the seal does not name"
    );
    let (status, body) = h
        .send(patch(
            serde_json::json!({"header": "authorization", "credential": id}),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The gateway, with its own key and this database.
    let seed = [13u8; 32];
    let minter = TokenMinter::new(&seed).unwrap();
    let validator = TokenValidator::new(
        &TokenMinter::public_key_of(&seed),
        outturn::auth::AUDIENCE_GATEWAY,
    )
    .unwrap();
    let sent = Arc::new(Recorder(Mutex::new(Vec::new())));
    let gateway = outturn::gateway::routes(Arc::new(
        outturn::gateway::GatewayState::new(Vec::new(), validator)
            .with_egress_transport(sent.clone())
            .with_sealed(
                outturn::gateway::egress::sealed::Credentials::default()
                    .with_keys(seal::Keys::parse(SEAL_KEY).unwrap())
                    .with_pool(h.db.pool.clone()),
            ),
    ));
    let fetch = |workspace: Uuid, rule: outturn::egress::rule::EgressRule| {
        let rules = vec![rule.clone()];
        let token = minter
            .mint_turn(
                Uuid::now_v7(),
                workspace,
                outturn::egress::commit::root(workspace, &rules),
                outturn::egress::gate::Gates::none().root(workspace),
            )
            .unwrap();
        let body = serde_json::json!({
            "method": "GET",
            "url": format!("https://{host}/api/items"),
            "proof": outturn::egress::commit::prove(workspace, &rules, &rule).unwrap(),
        });
        let gateway = gateway.clone();
        async move {
            let response = gateway
                .oneshot(
                    Request::post("/v1/egress")
                        .header("authorization", format!("Bearer {token}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            response.status()
        }
    };
    let rule = outturn::api::egress::rules_for(&h.db.pool, acme)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.credential == Some(id))
        .expect("the rule names the credential");

    assert_eq!(fetch(acme, rule.clone()).await, StatusCode::OK);
    assert_eq!(
        sent.0.lock().unwrap().last().cloned(),
        Some((
            format!("https://{host}/api/items"),
            Some("sk_test_acme".to_string())
        )),
        "the key went to its host, in its header"
    );

    // Another workspace naming it, in a rule of its own, honestly committed.
    let before = sent.0.lock().unwrap().len();
    assert_eq!(fetch(other, rule.clone()).await, StatusCode::FORBIDDEN);
    assert_eq!(
        sent.0.lock().unwrap().len(),
        before,
        "nothing left for another workspace"
    );

    // Revoked: within the moment a notification takes to arrive, nothing is
    // attached. Polled rather than asserted on the next request, because the
    // gateway hears of a revocation over LISTEN/NOTIFY, not in the same breath.
    let (status, _) = h
        .send(
            Request::delete(format!("/v1/credentials/{id}"))
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // Until it arrives, with no deadline, as for a cleared scope above.
    while fetch(acme, rule.clone()).await != StatusCode::FORBIDDEN {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let after = sent.0.lock().unwrap().len();
    assert_eq!(fetch(acme, rule).await, StatusCode::FORBIDDEN);
    assert_eq!(sent.0.lock().unwrap().len(), after, "and stays refused");
}
