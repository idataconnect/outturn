//! Sealed credentials and where they go.

use super::*;

use outturn::egress::rule::EgressRule;
use outturn::egress::seal;
use outturn::gateway::egress::transport::{
    EgressTransport, TransportError, TransportResponse, VettedRequest,
};
use std::sync::Mutex;

/// Answers 200 to everything, keeping each request's URL and Authorization.
struct Recorder(Mutex<Vec<(String, Option<String>)>>);

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Vec::new())))
    }

    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    fn last(&self) -> Option<(String, Option<String>)> {
        self.0.lock().unwrap().last().cloned()
    }
}

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

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The deployment's seal key as the page asks for it: the public half, and
/// its id.
async fn seal_key(h: &Harness, token: &str) -> (Vec<u8>, String) {
    let (status, body) = h.get("/v1/credentials/seal-key", Some(token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key: serde_json::Value = serde_json::from_str(&body).unwrap();
    (
        hex::decode(key["public_key"].as_str().unwrap()).unwrap(),
        key["key_id"].as_str().unwrap().to_string(),
    )
}

/// The gateway's real handler, with its own seal key and this database,
/// sending through a transport the test holds -- and the minter of the turn
/// tokens it trusts.
struct Gateway {
    routes: axum::Router,
    minter: TokenMinter,
}

impl Gateway {
    fn new(h: &Harness, transport: Arc<dyn EgressTransport>) -> Self {
        let seed = [13u8; 32];
        let validator = TokenValidator::new(
            &TokenMinter::public_key_of(&seed),
            outturn::auth::AUDIENCE_GATEWAY,
        )
        .unwrap();
        let routes = outturn::gateway::routes(Arc::new(
            outturn::gateway::GatewayState::new(Vec::new(), validator)
                .with_egress_transport(transport)
                .with_sealed(
                    outturn::gateway::egress::sealed::Credentials::default()
                        .with_keys(seal::Keys::parse(SEAL_KEY).unwrap())
                        .with_pool(h.db.pool.clone()),
                ),
        ));
        Self {
            routes,
            minter: TokenMinter::new(&seed).unwrap(),
        }
    }

    /// A GET of `url` by a turn in `workspace` that `asker` asked, carrying
    /// `rule` as the API would commit to it: what the gateway answered.
    async fn fetch(
        &self,
        workspace: Uuid,
        asker: Option<Uuid>,
        rule: &EgressRule,
        url: &str,
    ) -> (StatusCode, String) {
        let rules = vec![rule.clone()];
        let token = self
            .minter
            .mint_turn(
                Uuid::now_v7(),
                workspace,
                outturn::egress::commit::root(workspace, &rules),
                outturn::egress::gate::Gates::none().root(workspace),
                asker,
            )
            .unwrap();
        let body = serde_json::json!({
            "method": "GET",
            "url": url,
            "proof": outturn::egress::commit::prove(workspace, &rules, rule).unwrap(),
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
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }
}

/// A sealed credential goes to the workspace and host it was sealed for, in its
/// header, through the gateway's real handler -- and to nobody else, and not at
/// all once revoked. The API never holds the plaintext: the test seals it as
/// the browser or `outturn-seal` would.
#[tokio::test]
async fn a_sealed_credential_goes_where_it_was_bound_and_nowhere_else() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let other = h.make_workspace("Other", "other").await;

    let (public, key_id) = seal_key(&h, &admin).await;

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
    let sent = Recorder::new();
    let gateway = Gateway::new(&h, sent.clone());
    let url = format!("https://{host}/api/items");
    let fetch = |workspace: Uuid, rule: EgressRule| {
        let (gateway, url) = (&gateway, &url);
        async move { gateway.fetch(workspace, None, &rule, url).await.0 }
    };
    let rule = outturn::api::egress::rules_for(&h.db.pool, acme)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.credential == Some(id))
        .expect("the rule names the credential");

    assert_eq!(fetch(acme, rule.clone()).await, StatusCode::OK);
    assert_eq!(
        sent.last(),
        Some((
            format!("https://{host}/api/items"),
            Some("sk_test_acme".to_string())
        )),
        "the key went to its host, in its header"
    );

    // Another workspace naming it, in a rule of its own, honestly committed.
    let before = sent.len();
    assert_eq!(fetch(other, rule.clone()).await, StatusCode::FORBIDDEN);
    assert_eq!(sent.len(), before, "nothing left for another workspace");

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
    let after = sent.len();
    assert_eq!(fetch(acme, rule).await, StatusCode::FORBIDDEN);
    assert_eq!(sent.len(), after, "and stays refused");
}

/// A person's own credential is attached on their turns and nobody else's,
/// through the gateway's real handler: not on another person's turn, not as
/// the agent's on a turn nobody asked, and a person with nothing connected is
/// refused and nothing is sent. docs/user-credentials.md.
#[tokio::test]
async fn a_persons_credential_goes_only_with_their_turns() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (public, key_id) = seal_key(&h, &admin).await;

    let host = "93.184.215.14";
    let dana = Uuid::now_v7();
    let sam = Uuid::now_v7();
    let id = Uuid::now_v7();
    let binding = serde_json::to_vec(&serde_json::json!({
        "credential": id, "kind": "static", "workspaces": [acme],
        "hosts": [host], "header": "authorization", "user": dana,
    }))
    .unwrap();
    let sealed = seal::seal(&public, &binding, b"Bearer dana").unwrap();

    // An administrator cannot store it for her: that is acting as her.
    let (status, _) = h
        .post(
            "/v1/credentials",
            Some(&admin),
            &serde_json::json!({
                "id": id, "name": "Books", "binding": b64(&binding),
                "sealed": b64(&sealed), "key_id": key_id,
            })
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "stored for a person");

    // Stored as her own connection would store it.
    sqlx::query(
        "insert into credentials (id, workspace_id, name, binding, sealed, key_id, created_by)
         values ($1, $2, 'Books', $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(acme)
    .bind(&binding)
    .bind(&sealed)
    .bind(&key_id)
    .bind(dana)
    .execute(&h.db.pool)
    .await
    .unwrap();

    // Nor may an administrator write an agent's rule naming it: refused while
    // they are looking, as the gateway would refuse every request on it.
    let (status, _) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": host, "header": "authorization", "credential": id})
                .to_string(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "her key as the agent's rule"
    );

    let sent = Recorder::new();
    let gateway = Gateway::new(&h, sent.clone());
    let url = format!("https://{host}/api/invoices");
    let fetch = |asker: Option<Uuid>, rule: EgressRule| {
        let (gateway, url) = (&gateway, &url);
        async move { gateway.fetch(acme, asker, &rule, url).await }
    };
    let rule = |asker: Option<Uuid>, credential: Option<Uuid>| EgressRule {
        host: host.into(),
        header: Some("authorization".into()),
        credential_env: None,
        client: None,
        credential,
        asker,
    };

    let (status, body) = fetch(Some(dana), rule(Some(dana), Some(id))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        sent.last().and_then(|(_, auth)| auth),
        Some("Bearer dana".to_string()),
        "her key went with her turn"
    );
    let before = sent.len();

    // Each honestly committed by the API, and each refused.
    let (status, _) = fetch(Some(sam), rule(Some(dana), Some(id))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "her rule on his turn");
    let (status, _) = fetch(Some(sam), rule(Some(sam), Some(id))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "her key resolved for him");
    let (status, _) = fetch(None, rule(None, Some(id))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "her key as the agent's");
    let (status, body) = fetch(Some(sam), rule(Some(sam), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "nothing connected");
    assert!(body.contains("has not connected"), "and says so: {body}");
    assert_eq!(sent.len(), before, "nothing was sent, bare or otherwise");
}

/// A service account key is stored sealed through the API, and the gateway's
/// real handler signs with it, trades the assertion at the bound endpoint, and
/// sends the host the token it bought -- never the key, and nothing for a
/// workspace it was not sealed for. docs/service-accounts.md.
#[tokio::test]
async fn a_service_account_key_buys_a_token_and_is_never_sent() {
    use aws_lc_rs::encoding::AsDer as _;

    // Literal public addresses, so nothing asks a resolver.
    let host = "93.184.215.14";
    let token_url = "https://93.184.215.20/token";

    /// Answers the token endpoint with a token, and the API with 200, keeping
    /// every request's URL, Authorization and body.
    struct Google(Mutex<Vec<(String, Option<String>, String)>>);
    #[async_trait::async_trait]
    impl EgressTransport for Google {
        fn name(&self) -> &'static str {
            "test"
        }
        async fn send(&self, request: VettedRequest) -> Result<TransportResponse, TransportError> {
            let auth = request
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let token = request.url.path() == "/token";
            self.0.lock().unwrap().push((
                request.url.to_string(),
                auth,
                request.body.unwrap_or_default(),
            ));
            Ok(TransportResponse {
                status: 200,
                headers: Vec::new(),
                body: if token {
                    r#"{"access_token":"ya29.from-the-exchange","token_type":"Bearer","expires_in":3599}"#
                        .into()
                } else {
                    "{}".into()
                },
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

    let (public, key_id) = seal_key(&h, &admin).await;

    let pair =
        aws_lc_rs::signature::RsaKeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        b64(pair.as_der().unwrap().as_ref())
    );

    let id = Uuid::now_v7();
    let binding = serde_json::to_vec(&serde_json::json!({
        "credential": id, "kind": "service_account", "workspaces": [acme],
        "hosts": [host], "header": "authorization", "token_url": token_url,
        "account": "bills@example-project.iam.gserviceaccount.com",
        "scope": "https://www.googleapis.com/auth/spreadsheets",
    }))
    .unwrap();
    let sealed = seal::seal(&public, &binding, pem.as_bytes()).unwrap();
    let (status, body) = h
        .post(
            "/v1/credentials",
            Some(&admin),
            &serde_json::json!({
                "id": id, "name": "Water bills", "binding": b64(&binding),
                "sealed": b64(&sealed), "key_id": key_id,
            })
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, _) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": host, "header": "x-goog-api-key", "credential": id})
                .to_string(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "only in the header it was bound to"
    );
    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": host, "header": "authorization", "credential": id})
                .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let sent = Arc::new(Google(Mutex::new(Vec::new())));
    let gateway = Gateway::new(&h, sent.clone());
    let url = format!("https://{host}/v4/spreadsheets/abc/values/A1:B2");
    let fetch = |workspace: Uuid, rule: EgressRule| {
        let (gateway, url) = (&gateway, &url);
        async move { gateway.fetch(workspace, None, &rule, url).await.0 }
    };
    let rule = outturn::api::egress::rules_for(&h.db.pool, acme)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.credential == Some(id))
        .expect("the rule names the credential");

    assert_eq!(fetch(acme, rule.clone()).await, StatusCode::OK);
    assert_eq!(fetch(acme, rule.clone()).await, StatusCode::OK);
    let requests = sent.0.lock().unwrap().clone();
    let exchanges: Vec<_> = requests.iter().filter(|r| r.0 == token_url).collect();
    assert_eq!(exchanges.len(), 1, "one exchange, then held: {requests:?}");
    assert!(
        exchanges[0]
            .2
            .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"),
        "{}",
        exchanges[0].2
    );
    let to_host: Vec<_> = requests.iter().filter(|r| r.0 != token_url).collect();
    assert_eq!(to_host.len(), 2);
    for (_, auth, _) in &to_host {
        assert_eq!(auth.as_deref(), Some("Bearer ya29.from-the-exchange"));
    }
    assert!(
        requests
            .iter()
            .all(|(_, auth, body)| !body.contains("PRIVATE KEY")
                && !auth.as_deref().unwrap_or_default().contains("PRIVATE KEY")),
        "the key itself goes nowhere"
    );

    let before = sent.0.lock().unwrap().len();
    assert_eq!(fetch(other, rule).await, StatusCode::FORBIDDEN);
    assert_eq!(
        sent.0.lock().unwrap().len(),
        before,
        "no exchange and no request for another workspace"
    );
}

/// The whole of a connection that is each person's own, through the routes a
/// page would use: an administrator makes the rule, a viewer connects their
/// own key, a turn they send carries it, a turn somebody else sends is told
/// they have nothing connected, a turn nobody sent cannot reach the host, and
/// leaving the workspace takes the key away. docs/user-credentials.md.
#[tokio::test]
async fn a_person_connects_their_own_key_and_their_turns_carry_it() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let dana_token = h
        .login_as("dana@acme.example", None, Some((acme, "viewer")))
        .await;
    let sam_token = h
        .login_as("sam@acme.example", None, Some((acme, "viewer")))
        .await;
    let id_of = |email: &'static str| {
        let pool = h.db.pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>(
                "select user_id from user_identities \
                 where provider = 'password' and provider_subject = $1",
            )
            .bind(email)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let (dana, sam) = (
        id_of("dana@acme.example").await,
        id_of("sam@acme.example").await,
    );
    let host = "93.184.215.14";

    // An asker's rule carries each person's own key in a header, and nothing
    // the operator or an exchange supplies.
    let (status, _) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": host, "header": "authorization",
                "credential_env": "OUTTURN_EGRESS_BOOKS", "owner": "asker"})
            .to_string(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an asker's rule from the environment"
    );
    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": host, "header": "authorization", "owner": "asker"})
                .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let rule_id: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Dana sees it, unconnected, with what to seal to -- holding no authority
    // over the workspace's credentials.
    let (status, body) = h.get("/v1/connections", Some(&dana_token)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(listed["connections"][0]["host"], host);
    assert!(listed["connections"][0]["connected"].is_null());
    let public = hex::decode(listed["seal_key"]["public_key"].as_str().unwrap()).unwrap();
    let key_id = listed["seal_key"]["key_id"].as_str().unwrap().to_string();

    let connect = |token: &str, user: Uuid, secret: &[u8]| {
        let id = Uuid::now_v7();
        let binding = serde_json::to_vec(&serde_json::json!({
            "credential": id, "kind": "static", "workspaces": [acme],
            "hosts": [host], "header": "authorization", "user": user,
        }))
        .unwrap();
        let sealed = seal::seal(&public, &binding, secret).unwrap();
        let request = Request::put(format!("/v1/connections/{rule_id}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "id": id, "binding": b64(&binding),
                    "sealed": b64(&sealed), "key_id": key_id,
                })
                .to_string(),
            ))
            .unwrap();
        let h = &h;
        async move { (h.send(request).await, id) }
    };

    let ((status, _), _) = connect(&dana_token, sam, b"Bearer sam?").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a key sealed as somebody else"
    );
    let ((status, body), first) = connect(&dana_token, dana, b"Bearer dana-1").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let ((status, body), credential) = connect(&dana_token, dana, b"Bearer dana").await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "connecting again replaces: {body}"
    );
    let first_revoked: bool =
        sqlx::query_scalar("select revoked_at is not null from credentials where id = $1")
            .bind(first)
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    assert!(first_revoked, "the key it replaced is revoked");

    // What each turn is given, as the API mints it.
    let rules = |asker: Option<Uuid>| {
        let pool = h.db.pool.clone();
        async move {
            outturn::api::egress::rules_for_turn(&pool, acme, asker)
                .await
                .unwrap()
        }
    };
    let hers = rules(Some(dana)).await;
    assert_eq!(hers.len(), 1);
    assert_eq!(
        (hers[0].asker, hers[0].credential),
        (Some(dana), Some(credential))
    );
    let his = rules(Some(sam)).await;
    assert_eq!((his[0].asker, his[0].credential), (Some(sam), None));
    assert!(
        rules(None).await.is_empty(),
        "a turn nobody sent cannot reach a host whose connection is each person's"
    );

    let sent = Recorder::new();
    let gateway = Gateway::new(&h, sent.clone());
    let url = format!("https://{host}/api/invoices");
    let (status, body) = gateway.fetch(acme, Some(dana), &hers[0], &url).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        sent.last().and_then(|(_, auth)| auth),
        Some("Bearer dana".to_string())
    );
    let before = sent.len();
    let (status, body) = gateway.fetch(acme, Some(sam), &his[0], &url).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("has not connected"), "{body}");
    assert_eq!(sent.len(), before, "nothing sent for Sam");

    // Leaving the workspace takes her key with her.
    h.users
        .revoke_workspace_role(dana, acme, "viewer")
        .await
        .unwrap();
    let live: bool = sqlx::query_scalar("select revoked_at is null from credentials where id = $1")
        .bind(credential)
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert!(!live, "a key outliving its owner's membership");
    let (status, _) = h.get("/v1/connections", Some(&dana_token)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "and she connects nothing there"
    );
    drop(sam_token);
}

/// A host's connection made each person's own, and back to one key for
/// everyone, which revokes the keys people connected for it.
#[tokio::test]
async fn a_hosts_connection_switches_between_one_key_and_each_persons_own() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let host = "93.184.215.14";
    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({ "host": host }).to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let rule: Value = serde_json::from_str(&body).unwrap();
    let rule_id = rule["id"].as_str().unwrap().to_string();
    assert_eq!(rule["owner"], "agent");

    let owner = |body: serde_json::Value| {
        let request = Request::put(format!("/v1/egress-rules/{rule_id}/owner"))
            .header("authorization", format!("Bearer {admin}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let h = &h;
        async move { h.send(request).await }
    };
    let (status, _) = owner(serde_json::json!({ "owner": "asker" })).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "no header for people's keys"
    );
    let (status, body) =
        owner(serde_json::json!({ "owner": "asker", "header": "Authorization" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rule: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (&rule["owner"], &rule["header"]),
        (&"asker".into(), &"authorization".into())
    );

    // Somebody's own key for it, as their connection stores one.
    let dana = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, 'Dana')")
        .bind(dana)
        .execute(&h.db.pool)
        .await
        .unwrap();
    let theirs = Uuid::now_v7();
    sqlx::query(
        "insert into credentials \
             (id, workspace_id, name, binding, sealed, key_id, user_id, rule_id) \
         values ($1, $2, $3, '\\x7b7d', '\\x00', 'k', $4, $5::uuid)",
    )
    .bind(theirs)
    .bind(acme)
    .bind(host)
    .bind(dana)
    .bind(&rule_id)
    .execute(&h.db.pool)
    .await
    .unwrap();

    let (status, body) = owner(serde_json::json!({ "owner": "agent" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rule: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rule["owner"], "agent");
    assert!(rule["header"].is_null(), "no key, so no header: {rule}");
    let revoked: bool =
        sqlx::query_scalar("select revoked_at is not null from credentials where id = $1")
            .bind(theirs)
            .fetch_one(&h.db.pool)
            .await
            .unwrap();
    assert!(revoked, "the keys people connected go with it");
}

/// A machine user holds the keys for the turns nobody sent: an administrator
/// makes one, connects its key, and names it on an agent, whose turns nobody
/// sent then carry it as their asker. Nobody signs in as it, it holds no
/// role, and deleting it takes its keys. docs/user-credentials.md, *Nobody
/// asked*.
#[tokio::test]
async fn a_machine_user_holds_the_keys_for_turns_nobody_sent() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let host = "93.184.215.14";
    let send = |method: &str, uri: String, body: serde_json::Value| {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {admin}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let h = &h;
        async move { h.send(request).await }
    };

    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&admin),
            &serde_json::json!({"host": host, "header": "authorization", "owner": "asker"})
                .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let rule_id: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"Reports","slug":"reports","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let agent: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (status, body) = send(
        "POST",
        "/v1/machine-users".into(),
        serde_json::json!({"display_name": "Nightly reports"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let machine: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Nobody signs in as it, and it holds no role, whoever asks.
    let roled = sqlx::query(
        "insert into user_workspace_roles (user_id, workspace_id, role_id) \
         select $1, $2, id from roles where workspace_id = $2 and name = 'viewer'",
    )
    .bind(machine)
    .bind(acme)
    .execute(&h.db.pool)
    .await;
    assert!(roled.is_err(), "a machine user was given a role");
    let signed = sqlx::query(
        "insert into user_identities (id, user_id, provider, provider_subject) \
         values ($1, $2, 'password', 'nightly@acme.example')",
    )
    .bind(Uuid::now_v7())
    .bind(machine)
    .execute(&h.db.pool)
    .await;
    assert!(signed.is_err(), "a machine user was given a way to sign in");

    // Its key, connected by the administrator, sealed as its own.
    let (public, key_id) = seal_key(&h, &admin).await;
    let sealed_as = |user: Uuid| {
        let id = Uuid::now_v7();
        let binding = serde_json::to_vec(&serde_json::json!({
            "credential": id, "kind": "static", "workspaces": [acme],
            "hosts": [host], "header": "authorization", "user": user,
        }))
        .unwrap();
        let sealed = seal::seal(&public, &binding, b"Bearer nightly").unwrap();
        (
            id,
            serde_json::json!({"id": id, "binding": b64(&binding),
                "sealed": b64(&sealed), "key_id": key_id}),
        )
    };
    let (_, as_somebody_else) = sealed_as(Uuid::now_v7());
    let (status, _) = send(
        "PUT",
        format!("/v1/machine-users/{machine}/connections/{rule_id}"),
        as_somebody_else,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "sealed as somebody else");
    let (credential, its_own) = sealed_as(machine);
    let (status, body) = send(
        "PUT",
        format!("/v1/machine-users/{machine}/connections/{rule_id}"),
        its_own,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, body) = send(
        "PUT",
        format!("/v1/agents/{agent}/machine-user"),
        serde_json::json!({"machine_user_id": machine}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.get("/v1/machine-users", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        listed["machine_users"][0]["agents"],
        serde_json::json!([agent])
    );
    assert_eq!(
        listed["machine_users"][0]["connections"][0]["connected"]["credential_id"],
        credential.to_string()
    );

    // A turn nobody sent carries it; a person's turn carries the person.
    let payload = |user_id: Option<Uuid>| outturn::api::worker::ChatTurnPayload {
        workspace_id: acme,
        session_id: Uuid::now_v7(),
        agent_id: agent,
        message_id: Uuid::now_v7(),
        timezone: None,
        user_id,
    };
    let asker = outturn::api::worker::asker_for(&h.db.pool, &payload(None))
        .await
        .unwrap();
    assert_eq!(asker, Some(machine));
    let dana = Uuid::now_v7();
    assert_eq!(
        outturn::api::worker::asker_for(&h.db.pool, &payload(Some(dana)))
            .await
            .unwrap(),
        Some(dana)
    );
    let rules = outturn::api::egress::rules_for_turn(&h.db.pool, acme, asker)
        .await
        .unwrap();
    assert_eq!(
        (rules[0].asker, rules[0].credential),
        (Some(machine), Some(credential))
    );
    let sent = Recorder::new();
    let gateway = Gateway::new(&h, sent.clone());
    let (status, body) = gateway
        .fetch(acme, asker, &rules[0], &format!("https://{host}/reports"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        sent.last().and_then(|(_, auth)| auth),
        Some("Bearer nightly".to_string())
    );

    // Deleting it takes its keys, and the agent goes back to having nobody.
    let (status, _) = send(
        "DELETE",
        format!("/v1/machine-users/{machine}"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        outturn::api::worker::asker_for(&h.db.pool, &payload(None))
            .await
            .unwrap(),
        None
    );
    let gone: i64 = sqlx::query_scalar("select count(*) from credentials where id = $1")
        .bind(credential)
        .fetch_one(&h.db.pool)
        .await
        .unwrap();
    assert_eq!(gone, 0, "its key outlived it");
}
