//! Egress rules, and the hosts a skill names before it can be bound.

use super::*;

// -- Egress rules -------------------------------------------------------------

/// Adding an API is meant to be one paste of whatever the docs showed.
#[tokio::test]
async fn a_pasted_url_becomes_an_egress_rule() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    // Nothing to begin with, which is what an agent can reach to begin with.
    let (status, body) = h.get("/v1/egress-rules", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, r#"{"items":[],"next":null}"#);

    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&token),
            r#"{"host":"https://api.stripe.com/v1/charges"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(
        body.contains(r#""host":"api.stripe.com""#),
        "a pasted URL did not become a host rule: {body}"
    );

    let (status, body) = h.get("/v1/egress-rules", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("api.stripe.com"), "body: {body}");
}

/// A rule that would not do what its author expected is refused with the
/// reason, not a validation code.
#[tokio::test]
async fn a_rule_that_would_mislead_its_author_is_refused() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    for (input, expected) in [
        (r#"{"host":"*"}"#, "has to name a host"),
        (r#"{"host":"localhost"}"#, "no domain"),
        (r#"{"host":"10.0.0.5"}"#, "inside the network"),
        (
            r#"{"host":"api.example.com","header":"authorization"}"#,
            "environment variable",
        ),
        (
            r#"{"host":"api.example.com","credential_env":"KEY"}"#,
            "needs a header",
        ),
        // The gateway's own secrets live in the same environment; a rule
        // naming one would have an agent carry it anywhere it is allowed.
        (
            r#"{"host":"api.example.com","header":"authorization","credential_env":"GEMINI_API_KEY"}"#,
            "OUTTURN_EGRESS_",
        ),
    ] {
        let (status, body) = h.post("/v1/egress-rules", Some(&token), input).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{input} gave: {body}");
        assert!(body.contains(expected), "{input} gave: {body}");
    }
}

/// A rule exchanging client credentials names everything and holds nothing,
/// and it reaches the turn as written -- every field is what the commitment
/// covers, so one dropped on the way would fail every request it made.
#[tokio::test]
async fn a_client_credentials_rule_is_kept_and_reaches_the_turn() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&token),
            r#"{"host":"api.example.com","client":{
                "token_url":"https://login.example.com/tenant-a/oauth2/token",
                "scope":"bookings.read bookings.write",
                "client_id_env":"OUTTURN_EGRESS_BOOKING_CLIENT_ID",
                "client_secret_env":"OUTTURN_EGRESS_BOOKING_CLIENT_SECRET",
                "client_auth":"post"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(
        body.contains(r#""client_secret_env":"OUTTURN_EGRESS_BOOKING_CLIENT_SECRET""#),
        "{body}"
    );

    let rules = outturn::api::egress::rules_for(&h.db.pool, acme)
        .await
        .expect("rules");
    let client = rules[0].client.as_ref().expect("the exchange came back");
    assert_eq!(
        client.token_url,
        "https://login.example.com/tenant-a/oauth2/token"
    );
    assert_eq!(
        client.scope.as_deref(),
        Some("bookings.read bookings.write")
    );
    assert_eq!(client.client_id_env, "OUTTURN_EGRESS_BOOKING_CLIENT_ID");
    assert_eq!(client.client_auth, outturn::egress::rule::ClientAuth::Post);
    assert!(rules[0].header.is_none());
}

/// A rule naming a credential the operator did not bind to this workspace and
/// host is refused while somebody is looking -- and one that got in some other
/// way says so when listed. The gateway refuses either regardless; this is
/// only the earlier, clearer answer.
#[tokio::test]
async fn a_rule_naming_an_unbound_credential_is_refused_and_marked() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    for (input, expected) in [
        // Somebody else's variable: bound to nobody this harness knows of.
        (
            r#"{"host":"collect.example.net","header":"authorization","credential_env":"OUTTURN_EGRESS_GLOBEX_STRIPE_KEY"}"#,
            "not bound to any workspace",
        ),
        // A bound pair, aimed at a host it is not bound to.
        (
            r#"{"host":"collect.example.net","client":{"token_url":"https://login.example.com/tenant-a/oauth2/token","client_id_env":"OUTTURN_EGRESS_BOOKING_CLIENT_ID","client_secret_env":"OUTTURN_EGRESS_BOOKING_CLIENT_SECRET"}}"#,
            "not bound to collect.example.net",
        ),
        // At its host, but exchanged somewhere else.
        (
            r#"{"host":"api.example.com","client":{"token_url":"https://collect.example.net/token","client_id_env":"OUTTURN_EGRESS_BOOKING_CLIENT_ID","client_secret_env":"OUTTURN_EGRESS_BOOKING_CLIENT_SECRET"}}"#,
            "token endpoint",
        ),
    ] {
        let (status, body) = h.post("/v1/egress-rules", Some(&token), input).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{input} gave: {body}");
        assert!(body.contains(expected), "{input} gave: {body}");
    }

    // A row from before bindings existed.
    sqlx::query(
        "insert into egress_rules (id, workspace_id, host, header, credential_env) \
         values ($1, $2, 'collect.example.net', 'authorization', 'OUTTURN_EGRESS_GLOBEX_STRIPE_KEY')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(acme)
    .execute(&h.db.pool)
    .await
    .expect("insert");
    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&token),
            r#"{"host":"plain.example.com"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (_, body) = h.get("/v1/egress-rules", Some(&token)).await;
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();
    let unbound: Vec<(&str, bool)> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["host"].as_str().unwrap(),
                r["unbound"].as_bool().unwrap_or(false),
            )
        })
        .collect();
    assert!(unbound.contains(&("collect.example.net", true)), "{body}");
    assert!(unbound.contains(&("plain.example.com", false)), "{body}");
}

/// An exchange that would send its secret somewhere it should not go, or could
/// never work, is refused while somebody is looking.
#[tokio::test]
async fn a_client_credentials_rule_that_would_leak_or_fail_is_refused() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let client = |fields: &str| {
        format!(
            r#"{{"host":"api.example.com","client":{{"client_id_env":"OUTTURN_EGRESS_ID","client_secret_env":"OUTTURN_EGRESS_SECRET",{fields}}}}}"#
        )
    };
    for (input, expected) in [
        (
            client(r#""token_url":"http://login.example.com/token""#),
            "has to be https",
        ),
        (
            client(r#""token_url":"https://u:p@login.example.com/token""#),
            "credentials of its own",
        ),
        (client(r#""token_url":"login.example.com""#), "is not a URL"),
        (
            r#"{"host":"api.example.com","client":{"token_url":"https://login.example.com/t","client_id_env":"OUTTURN_EGRESS_ID","client_secret_env":"sk_live_abc-123"}}"#.to_string(),
            "not the name of an environment variable",
        ),
        (
            client(r#""token_url":"https://login.example.com/t","scope":"a  b""#),
            "single spaces",
        ),
        (
            r#"{"host":"api.example.com","client":{"token_url":"https://login.example.com/t","client_id_env":"OUTTURN_EGRESS_ID","client_secret_env":"DATABASE_URL"}}"#.to_string(),
            "OUTTURN_EGRESS_",
        ),
        (
            r#"{"host":"api.example.com","header":"authorization","credential_env":"KEY","client":{"token_url":"https://login.example.com/t","client_id_env":"ID","client_secret_env":"SECRET"}}"#.to_string(),
            "not both",
        ),
    ] {
        let (status, body) = h.post("/v1/egress-rules", Some(&token), &input).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{input} gave: {body}");
        assert!(body.contains(expected), "{input} gave: {body}");
    }
}

/// One rule per host, so which credential travels does not depend on
/// insertion order.
#[tokio::test]
async fn a_host_cannot_be_allowed_twice() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let rule = r#"{"host":"api.example.com"}"#;
    let (status, _) = h.post("/v1/egress-rules", Some(&token), rule).await;
    assert_eq!(status, StatusCode::CREATED);

    // The same host, spelled the way someone else would paste it.
    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&token),
            r#"{"host":"https://API.example.com/"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
}

/// One workspace's list is not another's, and neither is reachable from the
/// other's token.
#[tokio::test]
async fn egress_rules_do_not_cross_workspaces() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;
    let acme_token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let globex_token = h
        .login_as("admin@globex.example", None, Some((globex, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/egress-rules",
            Some(&acme_token),
            r#"{"host":"api.acme.example"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let id = body
        .split(r#""id":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("id")
        .to_string();

    let (status, body) = h.get("/v1/egress-rules", Some(&globex_token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, r#"{"items":[],"next":null}"#,
        "one workspace saw another's rules: {body}"
    );

    // Knowing an id is not the same as being able to use it.
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/egress-rules/{id}"))
        .header("authorization", format!("Bearer {globex_token}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a workspace deleted another workspace's rule"
    );

    let (status, body) = h.get("/v1/egress-rules", Some(&acme_token)).await;
    assert!(body.contains("api.acme.example"), "status {status}: {body}");
}

/// A skill that reaches somewhere the workspace has not allowed cannot be bound.
///
/// Declaring a host is a statement of what a skill needs, never a grant. The
/// egress rules stay the only thing that opens one, so the refusal names the
/// hosts and leaves the decision with somebody who can make it.
#[tokio::test]
async fn a_skill_cannot_be_bound_until_the_hosts_it_names_are_allowed() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("wx@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"weather","name":"Weather","body":"Call open-meteo.",
                "hosts":["https://api.open-meteo.com/v1/forecast"]}"#,
        )
        .await;
    let skill: serde_json::Value = serde_json::from_str(&body).expect("skill");
    let skill_id = skill["id"].as_str().expect("id").to_string();

    // The URL is reduced to a host, the same way a hand-written rule is.
    assert_eq!(
        skill["hosts"][0], "api.open-meteo.com",
        "not normalized: {body}"
    );
    assert_eq!(
        skill["unmet_hosts"][0], "api.open-meteo.com",
        "should be unmet: {body}"
    );

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"W","slug":"w"}"#)
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let bind = |token: &str, body: String| {
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/agents/{agent_id}/skills"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };

    let (status, body) = h
        .send(bind(&admin, format!(r#"[{{"skill_id":"{skill_id}"}}]"#)))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "bound without access: {body}");
    assert!(
        body.contains("api.open-meteo.com"),
        "the refusal must name the host: {body}"
    );

    // Approving opens it, and reports what it opened.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{skill_id}/hosts/approve"),
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "approve failed: {body}");
    assert!(body.contains("api.open-meteo.com"), "body: {body}");

    let (status, body) = h
        .send(bind(&admin, format!(r#"[{{"skill_id":"{skill_id}"}}]"#)))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "still refused after approval: {body}"
    );

    // And the rule remembers which skill asked for it.
    let from: Option<Uuid> = sqlx::query_scalar(
        "select from_skill_id from egress_rules where host = 'api.open-meteo.com'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("rule");
    assert_eq!(
        from.map(|u| u.to_string()),
        Some(skill_id),
        "provenance not recorded"
    );
}

/// Authoring a skill is not consent to what it reaches.
///
/// The operator role may write skills and may not open the network. Without
/// this, declaring a host and installing one's own skill would be a way around
/// the authority that governs egress.
#[tokio::test]
async fn writing_a_skill_does_not_grant_the_network_access_it_names() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as("op9@acme.example", None, Some((acme, "operator")))
        .await;

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            r#"{"slug":"weather","name":"Weather","body":"x","hosts":["api.open-meteo.com"]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an operator may write a skill: {body}"
    );
    let skill_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // But not open what it names.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{skill_id}/hosts/approve"),
            Some(&operator),
            "",
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator granted itself network access through a skill: {body}"
    );

    // An admin, who may write the rule by hand, may approve it.
    let admin = h
        .login_as("ad9@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, _) = h
        .post(
            &format!("/v1/skills/{skill_id}/hosts/approve"),
            Some(&admin),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// A version that adds a host is unvetted again; one that only rewords is not.
#[tokio::test]
async fn only_a_new_host_asks_for_approval_again() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("rv@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"w","name":"W","body":"one","hosts":["api.open-meteo.com"]}"#,
        )
        .await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.post(&format!("/v1/skills/{id}/hosts/approve"), Some(&admin), "")
        .await;

    // Reworded, same hosts: nothing to approve.
    h.post(
        &format!("/v1/skills/{id}/versions"),
        Some(&admin),
        r#"{"body":"two","note":"reworded","hosts":["api.open-meteo.com"]}"#,
    )
    .await;
    let (_, body) = h.get(&format!("/v1/skills/{id}"), Some(&admin)).await;
    let after: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        after["unmet_hosts"].as_array().unwrap().len(),
        0,
        "a reworded version asked for approval again: {body}"
    );

    // A version that reaches somewhere new does need approving, and only for
    // the host that is new.
    h.post(
        &format!("/v1/skills/{id}/versions"),
        Some(&admin),
        r#"{"body":"three","note":"adds a host","hosts":["api.open-meteo.com","api.example.com"]}"#,
    )
    .await;
    let (_, body) = h.get(&format!("/v1/skills/{id}"), Some(&admin)).await;
    let after: serde_json::Value = serde_json::from_str(&body).unwrap();
    let unmet = after["unmet_hosts"].as_array().unwrap();
    assert_eq!(
        unmet.len(),
        1,
        "should ask about the new host alone: {body}"
    );
    assert_eq!(unmet[0], "api.example.com");
}
