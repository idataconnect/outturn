//! The model catalog, accounts, models, chains and recipes.

use super::*;

/// A deployment holds a catalog from its first start, with nothing outside
/// reachable: the one the release shipped. Starting again keeps no second
/// copy of it, and only the operator sees what is held.
#[tokio::test]
async fn a_deployment_starts_with_the_shipped_catalog() {
    let h = harness_or_skip!();
    outturn::api::catalog::at_startup(&h.db.pool).await;
    outturn::api::catalog::at_startup(&h.db.pool).await;

    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h.get("/v1/platform/catalog", Some(&operator)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let held: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(held["source"], "shipped");
    assert_eq!(held["snapshots"], 1, "started twice, kept twice: {body}");
    assert!(held["models"].as_u64().unwrap() > 1000, "{body}");

    let admin = h
        .login_as("a@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, _) = h.get("/v1/platform/catalog", Some(&admin)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A catalog arriving as a file is kept as a snapshot like any other, and
/// one that says nothing new is not kept again.
#[tokio::test]
async fn a_catalog_is_kept_only_when_it_says_something_new() {
    let h = harness_or_skip!();
    let pool = &h.db.pool;
    let one = outturn::catalog::read(
        br#"{"acme-ai": {"name": "Acme AI", "models": {"m1": {"name": "M1", "cost": {"input": 1}}}}}"#,
    )
    .unwrap();
    assert!(
        outturn::api::catalog::keep(pool, "file:/etc/catalog.json", None, &one)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        outturn::api::catalog::keep(pool, "https://example.com/api.json", Some("\"e1\""), &one)
            .await
            .unwrap()
            .is_none(),
        "the same catalog was kept twice"
    );
    let two = outturn::catalog::read(
        br#"{"acme-ai": {"name": "Acme AI", "models": {"m1": {"name": "M1"}, "m2": {"name": "M2"}}}}"#,
    )
    .unwrap();
    assert!(
        outturn::api::catalog::keep(pool, "https://example.com/api.json", Some("\"e2\""), &two)
            .await
            .unwrap()
            .is_some()
    );
    let stored: Value =
        sqlx::query_scalar("select models from catalog_snapshots order by fetched_at desc limit 1")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(
        stored["acme-ai"]["models"]["m1"].get("cost").is_none(),
        "price was kept: {stored}"
    );
}

/// A catalog is checked conditionally: the first check keeps what came and
/// remembers the source's validator, and the next asks with it -- so an
/// unchanged catalog costs the source a "not modified" and is not kept again.
#[tokio::test]
async fn a_catalog_check_asks_whether_anything_changed() {
    use axum::{Json, Router, routing::post};
    let h = harness_or_skip!();
    let pool = &h.db.pool;
    outturn::api::catalog::at_startup(pool).await;

    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let log = asked.clone();
    let app = Router::new().route(
        "/v1/egress",
        post(move |Json(request): Json<Value>| {
            let log = log.clone();
            async move {
                let conditional = request["headers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|h| h[0] == "if-none-match" && h[1] == "\"v1\"");
                log.lock().unwrap().push(request);
                Json(if conditional {
                    serde_json::json!({"status": 304, "headers": [], "body": "", "truncated": false})
                } else {
                    serde_json::json!({
                        "status": 200,
                        "headers": [["ETag", "\"v1\""]],
                        "body": r#"{"acme-ai": {"name": "Acme AI", "models": {"m1": {"name": "M1"}}}}"#,
                        "truncated": false,
                    })
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let minter = outturn::auth::TokenMinter::new(&[5u8; 32]).unwrap();

    let first = outturn::api::catalog::check(pool, &minter, &gateway, Uuid::nil()).await;
    assert_eq!(first, Ok(outturn::api::catalog::Checked::Kept));
    let second = outturn::api::catalog::check(pool, &minter, &gateway, Uuid::nil()).await;
    assert_eq!(second, Ok(outturn::api::catalog::Checked::Unchanged));

    assert_eq!(asked.lock().unwrap().len(), 2);
    let kept: i64 = sqlx::query_scalar("select count(*) from catalog_snapshots")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(
        kept, 2,
        "the shipped catalog and the fetched one, and nothing for the 304"
    );
    let (next, checked): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("select next_check_at, checked_at from catalog_checks")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(
        next - checked >= chrono::Duration::days(7),
        "checked again before the week is up"
    );
}

/// A catalog carried in by hand is read in whatever shape it comes, and
/// something that is not one is refused rather than kept.
#[tokio::test]
async fn an_operator_imports_a_catalog_file() {
    let h = harness_or_skip!();
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
            "/v1/platform/catalog",
            Some(&operator),
            r#"{"acme-ai": {"name": "Acme AI", "models": {"m1": {"name": "M1"}}}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("kept"), "{body}");
    let (status, _) = h
        .post("/v1/platform/catalog", Some(&operator), r#"{"nope": 1}"#)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let admin = h
        .login_as("a@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, _) = h
        .post(
            "/v1/platform/catalog",
            Some(&admin),
            r#"{"acme-ai": {"models": {}}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// The operator configures where the platform's traffic goes: an account,
/// a model the catalog describes and one it does not, a chain of both, and
/// the class that walks it. A model the catalog lacks is refused until it is
/// described, a model in use is not deleted, and nobody else may do any of it.
#[tokio::test]
async fn the_operator_configures_accounts_models_and_chains() {
    let h = harness_or_skip!();
    outturn::api::catalog::at_startup(&h.db.pool).await;
    let (_acme, operator, admin) = template_people(&h).await;
    let json = |body: &str| -> Value { serde_json::from_str(body).unwrap() };

    let (status, _) = h.get("/v1/platform/models", Some(&admin)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            r#"{"name":"Anthropic","protocol":"anthropic","base_url":"https://api.anthropic.com","credential_env":"ANTHROPIC_API_KEY"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let account = json(&body)["id"].as_str().unwrap().to_string();
    let (status, _) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            r#"{"name":"x","protocol":"smoke-signals","base_url":"https://x.example.com"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let model = |name: &str, facts: Option<Value>| {
        let mut body = serde_json::json!({"account_id": account, "model": name});
        if let Some(facts) = facts {
            body["facts"] = facts;
        }
        body.to_string()
    };
    let (status, body) = h
        .post(
            "/v1/platform/models",
            Some(&operator),
            &model("claude-sonnet-5-5", None),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let sonnet = json(&body);
    assert_eq!(sonnet["name"], "Claude Sonnet 5.5");
    assert_eq!(sonnet["catalog_provider"], "anthropic");
    assert_eq!(sonnet["facts"]["temperature"], false);

    let (status, body) = h
        .post(
            "/v1/platform/models",
            Some(&operator),
            &model("claude-unheard-of", None),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let described = serde_json::json!({"limit": {"output": 16000}, "temperature": true});
    let (status, body) = h
        .post(
            "/v1/platform/models",
            Some(&operator),
            &model("claude-unheard-of", Some(described)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let unheard = json(&body);
    assert!(unheard["catalog_provider"].is_null());

    let chain_body = serde_json::json!({
        "name": "assistant",
        "models": [sonnet["id"], unheard["id"]],
    })
    .to_string();
    let (status, body) = h
        .post("/v1/platform/chains", Some(&operator), &chain_body)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let chain = json(&body)["id"].as_str().unwrap().to_string();
    let (status, body) = send_as(
        &h,
        "PUT",
        "/v1/platform/chain-assignments/assistant",
        &operator,
        &serde_json::json!({"chain_id": chain}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = h.get("/v1/platform/models", Some(&operator)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let held = json(&body);
    assert_eq!(
        held["chains"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == chain.as_str())
            .unwrap()["models"],
        serde_json::json!([sonnet["id"], unheard["id"]])
    );
    assert_eq!(held["assignments"]["assistant"], chain.as_str());

    let (status, _) = send_as(
        &h,
        "DELETE",
        &format!("/v1/platform/models/{}", sonnet["id"].as_str().unwrap()),
        &operator,
        "",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a model in a chain was deleted"
    );
}

/// One key is enough: each provider has a recipe of its own, and setting one
/// up the way the setup page does -- an account, a model per link, a chain
/// for conversations and a smaller one for titles and summaries -- leaves
/// every class answered at that provider and nowhere else. The keys named
/// here are environment variables nothing sets.
#[tokio::test]
async fn a_recipe_sets_up_with_one_key() {
    let h = harness_or_skip!();
    outturn::api::catalog::at_startup(&h.db.pool).await;
    let (_acme, operator, admin) = template_people(&h).await;
    let json = |body: &str| -> Value { serde_json::from_str(body).unwrap() };

    let (status, _) = h.get("/v1/platform/recipes", Some(&admin)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = h.get("/v1/platform/recipes", Some(&operator)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let recipes = json(&body);
    let recipe = |id: &str| -> Value {
        recipes
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no recipe {id}: {recipes}"))
            .clone()
    };
    let models_of = |links: &Value| -> Vec<String> {
        links
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["model"].as_str().unwrap().to_string())
            .collect()
    };
    // Against the shipped catalog, whose newest in each family these are.
    for (id, main, small) in [
        ("anthropic", "claude-sonnet-5-5", "claude-haiku-5-5"),
        ("openai", "gpt-6.1-sol", "gpt-5.4-mini"),
        ("gemini", "gemini-3.8-flash", "gemini-3.5-flash-lite"),
    ] {
        let r = recipe(id);
        assert_eq!(models_of(&r["links"]), [main], "{r}");
        assert_eq!(models_of(&r["small"]), [small], "{r}");
        assert_eq!(r["vendors"].as_array().unwrap().len(), 1, "{r}");
        assert_eq!(r["vendors"][0]["vendor"]["key"], id);
        assert_eq!(r["dropped"], serde_json::json!([]));
    }

    let openai = recipe("openai");
    let vendor = &openai["vendors"][0]["vendor"];
    let (status, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            &serde_json::json!({
                "name": vendor["name"],
                "protocol": vendor["protocol"],
                "base_url": openai["links"][0]["base_url"],
                "credential_env": "OUTTURN_TEST_NOT_A_KEY",
            })
            .to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let account = json(&body)["id"].as_str().unwrap().to_string();
    let mut chains = Vec::new();
    // Named as Set up names them: chain names are unique.
    let title = openai["title"].as_str().unwrap();
    for (links, name) in [
        (&openai["links"], title.to_string()),
        (&openai["small"], format!("{title}, titles and summaries")),
    ] {
        let mut ids = Vec::new();
        for model in models_of(links) {
            let (status, body) = h
                .post(
                    "/v1/platform/models",
                    Some(&operator),
                    &serde_json::json!({"account_id": account, "model": model, "tracks": true})
                        .to_string(),
                )
                .await;
            assert_eq!(status, StatusCode::CREATED, "{model}: {body}");
            let made = json(&body);
            assert_eq!(made["catalog_provider"], "openai", "{body}");
            ids.push(made["id"].clone());
        }
        let (status, body) = h
            .post(
                "/v1/platform/chains",
                Some(&operator),
                &serde_json::json!({"name": name, "models": ids}).to_string(),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        chains.push(json(&body)["id"].as_str().unwrap().to_string());
    }
    for (class, chain) in [
        ("assistant", &chains[0]),
        ("background", &chains[0]),
        ("session-name", &chains[1]),
        ("compaction", &chains[1]),
    ] {
        let (status, body) = send_as(
            &h,
            "PUT",
            &format!("/v1/platform/chain-assignments/{class}"),
            &operator,
            &serde_json::json!({"chain_id": chain}).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{class}: {body}");
    }

    let (_, body) = h.get("/v1/platform/models", Some(&operator)).await;
    let held = json(&body);
    // The harness seeds an account of its own; the recipe adds one beside it.
    let made: Vec<&Value> = held["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["id"] == account.as_str())
        .collect();
    assert_eq!(made[0]["base_url"], "https://api.openai.com", "{body}");
    assert_eq!(made[0]["credential_env"], "OUTTURN_TEST_NOT_A_KEY");
    assert_eq!(held["assignments"]["assistant"], chains[0].as_str());
    assert_eq!(held["assignments"]["session-name"], chains[1].as_str());
    let model_of = |id: &Value| -> String {
        held["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == *id)
            .unwrap()["model"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let chain_models = |chain: &str| -> Vec<String> {
        held["chains"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == chain)
            .unwrap()["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(model_of)
            .collect()
    };
    assert_eq!(chain_models(&chains[0]), ["gpt-6.1-sol"]);
    assert_eq!(chain_models(&chains[1]), ["gpt-5.4-mini"]);
}

/// A two-vendor recipe asked for with one vendor's key is what it becomes
/// without the other: the missing vendor's link dropped and named, and the
/// rest as it was. Named with neither, nothing is left to set up.
#[tokio::test]
async fn a_recipe_drops_the_vendor_without_a_key() {
    let h = harness_or_skip!();
    outturn::api::catalog::at_startup(&h.db.pool).await;
    let (_acme, operator, _admin) = template_people(&h).await;
    let medium = |with: &str| {
        let h = &h;
        let operator = &operator;
        let with = with.to_string();
        async move {
            let (status, body) = h
                .get(&format!("/v1/platform/recipes?with={with}"), Some(operator))
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let all: Value = serde_json::from_str(&body).unwrap();
            all.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == "medium")
                .unwrap()
                .clone()
        }
    };
    let keys = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|v| {
                v["key"]
                    .as_str()
                    .or(v["vendor"]["key"].as_str())
                    .unwrap()
                    .to_string()
            })
            .collect()
    };

    let both = medium("anthropic,gemini").await;
    assert_eq!(keys(&both["links"]), ["anthropic", "gemini"]);
    assert_eq!(keys(&both["dropped"]), Vec::<String>::new());

    let one = medium("gemini").await;
    assert_eq!(keys(&one["links"]), ["gemini"], "{one}");
    assert_eq!(one["links"][0]["model"], "gemini-3.8-flash");
    assert_eq!(keys(&one["dropped"]), ["anthropic"]);
    // What a key is asked for is still the whole recipe.
    assert_eq!(keys(&one["vendors"]), ["anthropic", "gemini"]);

    let none = medium("").await;
    assert!(none["links"].as_array().unwrap().is_empty(), "{none}");
    assert_eq!(keys(&none["dropped"]), ["anthropic", "gemini"]);
}

/// An account's key is sealed as a provider key, for every workspace, to the
/// account's host alone; sealing it replaces the environment variable.
#[tokio::test]
async fn an_accounts_key_is_sealed_for_its_host_alone() {
    use base64::Engine as _;
    use outturn::egress::seal;

    let h = harness_or_skip!();
    let (_acme, operator, _admin) = template_people(&h).await;
    let (_, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            r#"{"name":"Anthropic","protocol":"anthropic","base_url":"https://api.anthropic.com","credential_env":"ANTHROPIC_API_KEY"}"#,
        )
        .await;
    let account: Value = serde_json::from_str(&body).unwrap();
    let uri = format!(
        "/v1/platform/provider-accounts/{}/key",
        account["id"].as_str().unwrap()
    );

    let public = seal::public_of(SEAL_KEY).unwrap();
    let id = Uuid::now_v7();
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let sealed_for = |binding: Value| {
        let binding = serde_json::to_vec(&binding).unwrap();
        let sealed = seal::seal(&public, &binding, b"sk-ant-test").unwrap();
        serde_json::json!({
            "id": id, "name": "Anthropic key", "binding": b64(&binding),
            "sealed": b64(&sealed), "key_id": seal::key_id(&public),
        })
        .to_string()
    };
    let binding = |kind: &str, host: &str, workspaces: Value| {
        let mut b = serde_json::json!({
            "credential": id, "kind": kind, "workspaces": workspaces, "hosts": [host],
        });
        if kind == "static" {
            b["header"] = "x-api-key".into();
        }
        b
    };
    let every = serde_json::json!(["*"]);
    for (wrong, why) in [
        (
            binding("static", "api.anthropic.com", every.clone()),
            "as a header credential",
        ),
        (
            binding("provider", "elsewhere.example.com", every.clone()),
            "for another host",
        ),
        (
            binding(
                "provider",
                "api.anthropic.com",
                serde_json::json!([Uuid::now_v7()]),
            ),
            "for one workspace",
        ),
    ] {
        let (status, _) = send_as(&h, "PUT", &uri, &operator, &sealed_for(wrong)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a key sealed {why} was taken"
        );
    }
    let (status, body) = send_as(
        &h,
        "PUT",
        &uri,
        &operator,
        &sealed_for(binding("provider", "api.anthropic.com", every)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (_, body) = h.get("/v1/platform/models", Some(&operator)).await;
    let held: Value = serde_json::from_str(&body).unwrap();
    let ours = held["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == account["id"])
        .unwrap();
    assert_eq!(ours["credential_id"], id.to_string());
    assert!(ours["credential_env"].is_null());
}

/// A value for one model is checked against what that model accepts: Sonnet
/// takes no temperature and offers no "minimal" effort, but a budget it can
/// hold is fine, and a setting that is not about a model is not set per
/// model. The workspace sees the platform's models to choose for.
#[tokio::test]
async fn a_setting_for_one_model_is_one_that_model_accepts() {
    let h = harness_or_skip!();
    outturn::api::catalog::at_startup(&h.db.pool).await;
    let (_acme, operator, admin) = template_people(&h).await;
    let (_, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            r#"{"name":"Anthropic","protocol":"anthropic","base_url":"https://api.anthropic.com"}"#,
        )
        .await;
    let account: Value = serde_json::from_str(&body).unwrap();
    let (_, body) = h
        .post(
            "/v1/platform/models",
            Some(&operator),
            &serde_json::json!({"account_id": account["id"], "model": "claude-sonnet-5-5"})
                .to_string(),
        )
        .await;
    let sonnet = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = h.get("/v1/models", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("claude-sonnet-5-5"), "{body}");

    let put = |key: &str, value: Value| {
        let uri = format!("/v1/settings/{key}?model={sonnet}");
        let body = serde_json::json!({ "value": value }).to_string();
        let h = &h;
        let admin = admin.clone();
        async move { send_as(h, "PUT", &uri, &admin, &body).await.0 }
    };
    assert_eq!(
        put("temperature", serde_json::json!(0.5)).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        put("temperature", Value::Null).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        put("reasoning_effort", serde_json::json!("high")).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        put("context_budget", serde_json::json!(800_000)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        put("max_tool_rounds", serde_json::json!(5)).await,
        StatusCode::BAD_REQUEST,
        "not a setting about a model"
    );
    // The same temperature for every model is a preference, not an error: a
    // model that refuses it is passed over when it is asked.
    let (status, _) = send_as(
        &h,
        "PUT",
        "/v1/settings/temperature",
        &admin,
        r#"{"value": 0.5}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = h
        .get(&format!("/v1/settings?model={sonnet}"), Some(&admin))
        .await;
    let view: Vec<Value> = serde_json::from_str(&body).unwrap();
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .unwrap();
    assert_eq!(effort["value"], "high", "{effort}");
    assert_eq!(effort["per_model"], true);

    // The temperature set for every model is Sonnet's to inherit, not its
    // own: once Sonnet's own is cleared, no override shows for it at
    // Sonnet's level, while the effort set for Sonnet itself does.
    let (status, _) = send_as(
        &h,
        "DELETE",
        &format!("/v1/settings/temperature?model={sonnet}"),
        &admin,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h
        .get(&format!("/v1/settings?model={sonnet}"), Some(&admin))
        .await;
    let view: Vec<Value> = serde_json::from_str(&body).unwrap();
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .unwrap();
    let temperature = view.iter().find(|s| s["key"] == "temperature").unwrap();
    assert!(temperature.get("override_value").is_none(), "{temperature}");
    assert_eq!(temperature["value"], 0.5, "{temperature}");
    assert_eq!(effort["override_value"], "high", "{effort}");
}

/// "Latest", against the shipped catalog. A model tracked with nothing set
/// for it moves to the newest in its line at once. One with a value the
/// newest refuses -- a temperature, which Sonnet 5.5 takes none of -- stays,
/// says why and what approving would change, and asks the operator; approving
/// moves it with the value adjusted, and the question is settled. A pinned
/// model stays put and is offered its newer version.
#[tokio::test]
async fn a_tracked_model_moves_or_asks_and_a_pinned_one_is_offered() {
    let h = harness_or_skip!();
    outturn::api::catalog::at_startup(&h.db.pool).await;
    let (_acme, operator, admin) = template_people(&h).await;
    let json = |body: &str| -> Value { serde_json::from_str(body).unwrap() };
    let (_, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            r#"{"name":"Anthropic","protocol":"anthropic","base_url":"https://api.anthropic.com"}"#,
        )
        .await;
    let account = json(&body)["id"].clone();
    let configure = |model: &str| {
        let body = serde_json::json!({"account_id": account, "model": model}).to_string();
        let h = &h;
        let operator = operator.clone();
        async move {
            let (status, body) = h.post("/v1/platform/models", Some(&operator), &body).await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            serde_json::from_str::<Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    let haiku = configure("claude-haiku-4-5").await;
    let sonnet = configure("claude-sonnet-4-5").await;
    let opus = configure("claude-opus-4-8").await;

    let (status, _) = send_as(
        &h,
        "PUT",
        &format!("/v1/settings/temperature?model={sonnet}"),
        &admin,
        r#"{"value": 0.5}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for id in [&haiku, &sonnet] {
        let (status, body) = send_as(
            &h,
            "PATCH",
            &format!("/v1/platform/models/{id}"),
            &operator,
            r#"{"tracks": true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    let model_of = |id: String| {
        let h = &h;
        let operator = operator.clone();
        async move {
            let (_, body) = h.get("/v1/platform/models", Some(&operator)).await;
            serde_json::from_str::<Value>(&body).unwrap()["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["id"] == id.as_str())
                .unwrap()
                .clone()
        }
    };
    let moved = model_of(haiku.clone()).await;
    assert_eq!(moved["model"], "claude-haiku-5-5", "{moved}");
    assert!(moved["pending_model"].is_null());

    let held = model_of(sonnet.clone()).await;
    assert_eq!(held["model"], "claude-sonnet-4-5", "{held}");
    assert_eq!(held["pending_model"], "claude-sonnet-5-5");
    assert_eq!(held["pending_adjustments"][0]["key"], "temperature");
    assert!(held["pending_adjustments"][0]["to"].is_null());

    let pinned = model_of(opus.clone()).await;
    assert_eq!(pinned["model"], "claude-opus-4-8");
    assert_eq!(pinned["pending_model"], "claude-opus-5-5");

    let (_, body) = h.get("/v1/action-items", Some(&operator)).await;
    let asked = json(&body)["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "model.upgrade")
        .count();
    assert_eq!(asked, 2, "the operator was not asked about both: {body}");

    let (status, body) = h
        .post(
            &format!("/v1/platform/models/{sonnet}/upgrade"),
            Some(&operator),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let upgraded = model_of(sonnet.clone()).await;
    assert_eq!(upgraded["model"], "claude-sonnet-5-5");
    let (_, body) = h
        .get(&format!("/v1/settings?model={sonnet}"), Some(&admin))
        .await;
    let view: Vec<Value> = serde_json::from_str(&body).unwrap();
    let temperature = view.iter().find(|s| s["key"] == "temperature").unwrap();
    assert!(
        temperature["value"].is_null(),
        "not adjusted: {temperature}"
    );

    let (_, body) = h.get("/v1/action-items", Some(&operator)).await;
    let still = json(&body)["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "model.upgrade")
        .count();
    assert_eq!(
        still, 1,
        "the approved upgrade's question is still open: {body}"
    );

    // A model that goes takes its question with it.
    let (status, _) = send_as(
        &h,
        "DELETE",
        &format!("/v1/platform/models/{opus}"),
        &operator,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h.get("/v1/action-items", Some(&operator)).await;
    assert!(
        !body.contains("model.upgrade"),
        "a deleted model's question is still open: {body}"
    );

    // The platform's questions are the operator's alone.
    let (_, body) = h.get("/v1/action-items", Some(&admin)).await;
    assert!(
        !body.contains("model.upgrade"),
        "a workspace admin was asked: {body}"
    );
}

/// A model change is noted where it happens: three replies answered by the
/// same fallback carry one note, on the first; a reply back on the first
/// model carries none; and the fallback answering again is a change again.
#[tokio::test]
async fn a_model_change_is_noted_once_where_it_happens() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("changes@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk-changes"}"#,
        )
        .await;
    let agent = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&token),
            &format!(r#"{{"agent_id":"{agent}","title":""}}"#),
        )
        .await;
    let session: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Who answered each reply, in order, and whether it was a fallback.
    let replies = [
        ("backup", true),
        ("backup", true),
        ("backup", true),
        ("first", false),
        ("backup", true),
    ];
    let mut ids = Vec::new();
    for (model, fell_back) in replies {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into agent_messages (id, session_id, role, content, model) \
             values ($1, $2, 'assistant', 'hi', $3)",
        )
        .bind(id)
        .bind(session)
        .bind(model)
        .execute(&h.db.pool)
        .await
        .unwrap();
        if fell_back {
            sqlx::query(
                "insert into reply_fallbacks (reply_id, session_id, configured_model, \
                     served_model, reason) values ($1, $2, 'first', 'backup', 'was unavailable')",
            )
            .bind(id)
            .bind(session)
            .execute(&h.db.pool)
            .await
            .unwrap();
        }
        ids.push(id.to_string());
    }

    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session}/model-changes"),
            Some(&token),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let noted: Vec<String> = serde_json::from_str::<Vec<Value>>(&body)
        .unwrap()
        .iter()
        .map(|c| c["reply_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(noted, [ids[0].clone(), ids[4].clone()]);
    finish!(h);
}

/// An account can be renamed and moved; one whose key is sealed may move
/// only within its host, since the key opens nowhere else.
#[tokio::test]
async fn an_account_moves_only_where_its_key_still_opens() {
    use base64::Engine as _;
    use outturn::egress::seal;

    let h = harness_or_skip!();
    let (_acme, operator, _admin) = template_people(&h).await;
    let (_, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            r#"{"name":"Local","protocol":"openai","base_url":"http://localhost:11434"}"#,
        )
        .await;
    let account: Value = serde_json::from_str(&body).unwrap();
    let uri = format!(
        "/v1/platform/provider-accounts/{}",
        account["id"].as_str().unwrap()
    );
    let (status, body) = send_as(
        &h,
        "PATCH",
        &uri,
        &operator,
        r#"{"name":"Laptop","base_url":"http://models.example.com:11434"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let moved: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(moved["name"], "Laptop");
    assert_eq!(moved["base_url"], "http://models.example.com:11434");

    // Seal a key to that host, and the account may no longer leave it.
    let public = seal::public_of(SEAL_KEY).unwrap();
    let id = Uuid::now_v7();
    let binding = serde_json::to_vec(&serde_json::json!({
        "credential": id, "kind": "provider", "workspaces": ["*"],
        "hosts": ["models.example.com"],
    }))
    .unwrap();
    let sealed = seal::seal(&public, &binding, b"k").unwrap();
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let (status, body) = send_as(
        &h,
        "PUT",
        &format!("{uri}/key"),
        &operator,
        &serde_json::json!({
            "id": id, "name": "key", "binding": b64(&binding),
            "sealed": b64(&sealed), "key_id": seal::key_id(&public),
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = send_as(
        &h,
        "PATCH",
        &uri,
        &operator,
        r#"{"base_url":"http://elsewhere.example.com:11434"}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a sealed key's account left its host"
    );
    let (status, _) = send_as(
        &h,
        "PATCH",
        &uri,
        &operator,
        r#"{"base_url":"https://models.example.com/v1"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the same host refused");
}

/// Accounts, models and chains are picked by name, so a second of one name is
/// refused as a conflict -- except a model's name nobody typed, the
/// catalog's, which takes its account's name after it instead.
#[tokio::test]
async fn names_the_platform_picks_by_are_unique() {
    let h = harness_or_skip!();
    outturn::api::catalog::at_startup(&h.db.pool).await;
    let (_acme, operator, _admin) = template_people(&h).await;
    let json = |body: &str| -> Value { serde_json::from_str(body).unwrap() };
    let account = |name: &str, url: &str| {
        serde_json::json!({"name": name, "protocol": "anthropic", "base_url": url}).to_string()
    };

    let (status, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            &account("Anthropic", "https://api.anthropic.com"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let first = json(&body)["id"].as_str().unwrap().to_string();
    let (status, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            &account("Anthropic", "https://proxy.example.com"),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(json(&body)["code"], "conflict", "{body}");
    assert!(
        json(&body)["message"]
            .as_str()
            .unwrap()
            .contains("\"Anthropic\""),
        "{body}"
    );
    let (status, body) = h
        .post(
            "/v1/platform/provider-accounts",
            Some(&operator),
            &account("Proxy", "https://api.anthropic.com"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let second = json(&body)["id"].as_str().unwrap().to_string();
    let (status, body) = send_as(
        &h,
        "PATCH",
        &format!("/v1/platform/provider-accounts/{second}"),
        &operator,
        r#"{"name":"Anthropic"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let model = |account: &str| {
        serde_json::json!({"account_id": account, "model": "claude-sonnet-5-5"}).to_string()
    };
    let (status, body) = h
        .post("/v1/platform/models", Some(&operator), &model(&first))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let a = json(&body);
    assert_eq!(a["name"], "Claude Sonnet 5.5");
    let (status, body) = h
        .post("/v1/platform/models", Some(&operator), &model(&second))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let b = json(&body);
    assert_eq!(b["name"], "Claude Sonnet 5.5 (Proxy)");
    let (status, body) = send_as(
        &h,
        "PATCH",
        &format!("/v1/platform/models/{}", b["id"].as_str().unwrap()),
        &operator,
        r#"{"name":"Claude Sonnet 5.5"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let chain = serde_json::json!({"name": "assistant", "models": [a["id"]]}).to_string();
    let (status, body) = h.post("/v1/platform/chains", Some(&operator), &chain).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = h.post("/v1/platform/chains", Some(&operator), &chain).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}
