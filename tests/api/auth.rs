//! Signing in, sessions and their cookies, refresh, identities and passwords.

use super::*;

#[tokio::test]
async fn login_without_workspace_returns_picker() {
    let h = harness_or_skip!();
    let workspace = h.make_workspace("Acme", "acme").await;

    let user = h
        .users
        .create(CreateUser {
            email: "picker@example.com".into(),
            display_name: "Picker".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");
    h.users
        .grant_workspace_role(user.id, workspace, "viewer")
        .await
        .expect("grant");

    let (status, body) = h
        .post(
            "/v1/login",
            None,
            r#"{"email":"picker@example.com","password":"correct-horse"}"#,
        )
        .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("select_workspace"), "body: {body}");
    assert!(body.contains("acme"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn first_operator_signs_in_to_their_workspace() {
    let h = harness_or_skip!();
    let seed = |op| outturn::api::seed::first_operator(&h.users, &h.workspaces, &h.roles, op);

    // A bad slug is refused before anything is written, so the next attempt
    // still finds an empty deployment.
    assert!(
        seed(operator("op@example.com", "Not A Slug"))
            .await
            .is_err()
    );
    let workspace = seed(operator("op@example.com", "i-data-connect"))
        .await
        .expect("first operator");
    assert_eq!(workspace.name, "i Data Connect!");

    let (status, body) = h
        .post(
            "/v1/login",
            None,
            r#"{"email":"op@example.com","password":"correct-horse"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("i-data-connect"), "body: {body}");

    // Once anybody has an account, accounts are made by somebody signed in.
    let again = seed(operator("second@example.com", "second")).await;
    assert!(again.is_err(), "a second operator was created");

    finish!(h);
}

#[tokio::test]
async fn bad_password_is_unauthorized() {
    let h = harness_or_skip!();
    h.make_workspace("Acme", "acme").await;
    h.users
        .create(CreateUser {
            email: "user@example.com".into(),
            display_name: "User".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");

    let (status, _) = h
        .post(
            "/v1/login",
            None,
            r#"{"email":"user@example.com","password":"wrong"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    finish!(h);
}

#[tokio::test]
async fn login_to_unaffiliated_workspace_is_forbidden() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    let user = h
        .users
        .create(CreateUser {
            email: "acme-only@example.com".into(),
            display_name: "Acme Only".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");
    h.users
        .grant_workspace_role(user.id, acme, "viewer")
        .await
        .expect("grant");

    let (status, body) = h
        .post(
            "/v1/login",
            None,
            &format!(
                r#"{{"email":"acme-only@example.com","password":"correct-horse","workspace_id":"{globex}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn workspace_switch_remints_for_new_workspace() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    let token = h
        .login_as(
            "admin@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/session/workspace")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"workspace_id":"{globex}"}}"#)))
        .expect("request");
    let (status, body, new_cookie) = h.send_full(req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["workspace_id"].as_str().unwrap(), globex.to_string());

    // The re-minted token must actually be scoped to the new workspace.
    let new_token = new_cookie.expect("switch must re-set the cookie");
    let (status, body) = h.get("/v1/session", Some(&new_token)).await;
    assert_eq!(status, StatusCode::OK);
    let session: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        session["workspace_id"].as_str().unwrap(),
        globex.to_string()
    );

    finish!(h);
}

#[tokio::test]
async fn missing_token_is_unauthorized() {
    let h = harness_or_skip!();
    let (status, _) = h.get("/v1/workspaces", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    finish!(h);
}

#[tokio::test]
async fn account_can_have_several_emails() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;

    let user = h
        .users
        .create(CreateUser {
            email: "primary@example.com".into(),
            display_name: "Multi".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");
    h.users
        .grant_workspace_role(user.id, acme, "admin")
        .await
        .expect("grant");
    h.users
        .add_password_identity(user.id, "second@example.com", "correct-horse")
        .await
        .expect("add identity");

    // Either address reaches the same account, with the same roles.
    for email in ["primary@example.com", "second@example.com"] {
        let (status, body) = h
            .post(
                "/v1/login",
                None,
                &format!(
                    r#"{{"email":"{email}","password":"correct-horse","workspace_id":"{acme}"}}"#
                ),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{email}: {body}");
        let parsed: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            parsed["user_id"].as_str().unwrap(),
            user.id.to_string(),
            "{email} must resolve to the same account"
        );
        assert!(body.contains("admin"), "{email} keeps its roles: {body}");
    }

    finish!(h);
}

#[tokio::test]
async fn identities_are_globally_unique() {
    let h = harness_or_skip!();
    h.make_workspace("Acme", "acme").await;

    h.users
        .create(CreateUser {
            email: "taken@example.com".into(),
            display_name: "First".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");

    // A second account must not be able to claim the same address.
    let second = h
        .users
        .create(CreateUser {
            email: "other@example.com".into(),
            display_name: "Second".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");

    let result = h
        .users
        .add_password_identity(second.id, "taken@example.com", "correct-horse")
        .await;
    assert!(result.is_err(), "duplicate address must be refused");

    finish!(h);
}

#[tokio::test]
async fn last_identity_cannot_be_removed() {
    let h = harness_or_skip!();
    h.make_workspace("Acme", "acme").await;

    let user = h
        .users
        .create(CreateUser {
            email: "only@example.com".into(),
            display_name: "Only".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");

    let fetched = h.users.get(user.id).await.expect("get");
    assert_eq!(fetched.identities.len(), 1);

    let result = h
        .users
        .remove_identity(user.id, fetched.identities[0].id)
        .await;
    assert!(
        result.is_err(),
        "removing the only sign-in method must be refused"
    );

    // With a second identity present, removal is allowed.
    h.users
        .add_password_identity(user.id, "spare@example.com", "correct-horse")
        .await
        .expect("add");
    h.users
        .remove_identity(user.id, fetched.identities[0].id)
        .await
        .expect("remove once another exists");

    finish!(h);
}

#[tokio::test]
async fn removed_identity_can_no_longer_sign_in() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;

    let user = h
        .users
        .create(CreateUser {
            email: "keep@example.com".into(),
            display_name: "Keeper".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");
    h.users
        .grant_workspace_role(user.id, acme, "viewer")
        .await
        .expect("grant");
    let dropped = h
        .users
        .add_password_identity(user.id, "drop@example.com", "correct-horse")
        .await
        .expect("add");

    h.users
        .remove_identity(user.id, dropped.id)
        .await
        .expect("remove");

    let (status, _) = h
        .post(
            "/v1/login",
            None,
            &format!(
                r#"{{"email":"drop@example.com","password":"correct-horse","workspace_id":"{acme}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The account itself still works through its remaining identity.
    let (status, _) = h
        .post(
            "/v1/login",
            None,
            &format!(
                r#"{{"email":"keep@example.com","password":"correct-horse","workspace_id":"{acme}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    finish!(h);
}

#[tokio::test]
async fn session_cookie_is_httponly_and_samesite() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;

    let user = h
        .users
        .create(CreateUser {
            email: "cookie@example.com".into(),
            display_name: "Cookie".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");
    h.users
        .grant_workspace_role(user.id, acme, "viewer")
        .await
        .expect("grant");

    let req = Request::builder()
        .method("POST")
        .uri("/v1/login")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"cookie@example.com","password":"correct-horse","workspace_id":"{acme}"}}"#
        )))
        .expect("request");

    let response = h.app.clone().oneshot(req).await.expect("response");
    assert_eq!(response.status(), StatusCode::OK);

    let set_cookie = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("cookie set")
        .to_str()
        .expect("utf8");

    assert!(set_cookie.contains("HttpOnly"), "got: {set_cookie}");
    assert!(set_cookie.contains("Secure"), "got: {set_cookie}");
    assert!(set_cookie.contains("SameSite=Lax"), "got: {set_cookie}");

    // The token must not also appear in the body, or HttpOnly buys nothing.
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        !body.contains("\"token\""),
        "token must not be in the body: {body}"
    );

    finish!(h);
}

#[tokio::test]
async fn cookie_authenticates_subsequent_requests() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let cookie = h
        .login_as("cookieuser@example.com", None, Some((acme, "admin")))
        .await;

    // Sent as a Cookie header rather than Authorization.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/session")
        .header("cookie", format!("outturn_session={cookie}"))
        .body(Body::empty())
        .expect("request");

    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("admin"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn a_valid_bearer_wins_over_an_invalid_session_cookie() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let good = h
        .login_as("bothcreds@example.com", None, Some((acme, "admin")))
        .await;

    let with = |cookie: &str, bearer: Option<&str>| {
        let mut b = Request::builder()
            .method("GET")
            .uri("/v1/session")
            .header("cookie", format!("outturn_session={cookie}"));
        if let Some(t) = bearer {
            b = b.header("authorization", format!("bearer {t}"));
        }
        b.body(Body::empty()).expect("request")
    };

    let (status, body) = h.send(with("not-a-token", Some(&good))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a stale cookie shadowed the bearer: {body}"
    );

    let (status, _) = h.send(with("not-a-token", Some("also-bad"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = h.send(with("not-a-token", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A good cookie still settles the request over a stale bearer.
    let (status, body) = h.send(with(&good, Some("stale"))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a stale bearer shadowed the cookie: {body}"
    );

    finish!(h);
}

#[tokio::test]
async fn logout_clears_the_cookie() {
    let h = harness_or_skip!();

    let req = Request::builder()
        .method("POST")
        .uri("/v1/logout")
        .body(Body::empty())
        .expect("request");

    let response = h.app.clone().oneshot(req).await.expect("response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let set_cookie = response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .expect("cookie cleared")
        .to_str()
        .expect("utf8");
    assert!(set_cookie.contains("Max-Age=0"), "got: {set_cookie}");

    finish!(h);
}

#[tokio::test]
async fn refresh_outlives_the_access_token() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "lifetime@example.com", acme, "viewer").await;

    let response = login_raw(&h, "lifetime@example.com", acme).await;
    assert_eq!(response.status(), StatusCode::OK);

    // The access token is deliberately short now that refresh renews it: that
    // is what bounds how long a stolen token or a revoked role stays usable.
    // The refresh token must outlast it by a wide margin, or the user is sent
    // back to the login form during ordinary use.
    const {
        assert!(
            outturn::api::session::REFRESH_LIFETIME_SECS
                > outturn::auth::SESSION_TOKEN_LIFETIME_SECS * 100,
            "refresh lifetime must dwarf the access lifetime",
        )
    };

    finish!(h);
}

/// Extracts one named cookie from all Set-Cookie headers on a response.
fn cookie_named(response: &axum::response::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            let first = v.split(';').next()?;
            let (k, value) = first.split_once('=')?;
            (k == name).then(|| value.to_string())
        })
}

async fn login_raw(h: &Harness, email: &str, workspace: Uuid) -> axum::response::Response {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/login")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"{email}","password":"correct-horse","workspace_id":"{workspace}"}}"#
        )))
        .expect("request");
    h.app.clone().oneshot(req).await.expect("response")
}

async fn refresh_with(h: &Harness, token: &str) -> axum::response::Response {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/session/refresh")
        .header("cookie", format!("outturn_refresh={token}"))
        .body(Body::empty())
        .expect("request");
    h.app.clone().oneshot(req).await.expect("response")
}

async fn seed_user(h: &Harness, email: &str, workspace: Uuid, role: &str) {
    let user = h
        .users
        .create(CreateUser {
            email: email.into(),
            display_name: "Refresh User".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");
    h.users
        .grant_workspace_role(user.id, workspace, role)
        .await
        .expect("grant");
}

#[tokio::test]
async fn login_issues_a_path_scoped_refresh_cookie() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "refresh@example.com", acme, "viewer").await;

    let response = login_raw(&h, "refresh@example.com", acme).await;
    assert_eq!(response.status(), StatusCode::OK);

    let raw = response
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("outturn_refresh="))
        .expect("refresh cookie set")
        .to_string();

    assert!(raw.contains("HttpOnly"), "got: {raw}");
    assert!(raw.contains("Secure"), "got: {raw}");
    // Confined to the refresh endpoint, so it is not sent on ordinary calls.
    assert!(raw.contains("Path=/v1/session/refresh"), "got: {raw}");

    finish!(h);
}

#[tokio::test]
async fn refresh_rotates_and_returns_a_working_access_token() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "rotate@example.com", acme, "admin").await;

    let first = login_raw(&h, "rotate@example.com", acme).await;
    let refresh_one = cookie_named(&first, "outturn_refresh").expect("refresh cookie");

    let rotated = refresh_with(&h, &refresh_one).await;
    assert_eq!(rotated.status(), StatusCode::OK);

    let refresh_two = cookie_named(&rotated, "outturn_refresh").expect("rotated refresh");
    assert_ne!(refresh_one, refresh_two, "refresh token must rotate");

    // The new access token must actually work.
    let access = cookie_named(&rotated, "outturn_session").expect("access cookie");
    let (status, body) = h
        .send(
            Request::builder()
                .method("GET")
                .uri("/v1/session")
                .header("cookie", format!("outturn_session={access}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("admin"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn replaying_a_rotated_refresh_token_revokes_the_family() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "replay@example.com", acme, "viewer").await;

    let first = login_raw(&h, "replay@example.com", acme).await;
    let stolen = cookie_named(&first, "outturn_refresh").expect("refresh cookie");

    let rotated = refresh_with(&h, &stolen).await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let legitimate = cookie_named(&rotated, "outturn_refresh").expect("rotated refresh");

    // An attacker replays the token the legitimate client already exchanged.
    let replay = refresh_with(&h, &stolen).await;
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

    // Detection must revoke the whole family, not just the replayed token:
    // otherwise the thief simply keeps using the current one.
    let after = refresh_with(&h, &legitimate).await;
    assert_eq!(
        after.status(),
        StatusCode::UNAUTHORIZED,
        "replay must revoke the entire family"
    );

    finish!(h);
}

#[tokio::test]
async fn logout_revokes_the_refresh_token_server_side() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "logout@example.com", acme, "viewer").await;

    let first = login_raw(&h, "logout@example.com", acme).await;
    let refresh = cookie_named(&first, "outturn_refresh").expect("refresh cookie");
    let access = cookie_named(&first, "outturn_session").expect("access cookie");

    // A browser sends the refresh cookie only to the refresh route, so logout
    // arrives with the access cookie alone.
    let req = Request::builder()
        .method("POST")
        .uri("/v1/logout")
        .header("cookie", format!("outturn_session={access}"))
        .body(Body::empty())
        .expect("request");
    let response = h.app.clone().oneshot(req).await.expect("response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Clearing the cookie is not enough: the token itself must be dead.
    let after = refresh_with(&h, &refresh).await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);

    finish!(h);
}

#[tokio::test]
async fn workspace_switch_revokes_the_old_refresh_token() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;
    seed_user(&h, "switch@example.com", acme, "viewer").await;
    let users = h.users.list(None, 100).await.expect("list");
    let user = users
        .iter()
        .find(|u| {
            u.identities
                .iter()
                .any(|i| i.subject == "switch@example.com")
        })
        .expect("user");
    h.users
        .grant_workspace_role(user.id, globex, "viewer")
        .await
        .expect("grant");

    let first = login_raw(&h, "switch@example.com", acme).await;
    let old_refresh = cookie_named(&first, "outturn_refresh").expect("refresh cookie");
    let access = cookie_named(&first, "outturn_session").expect("access cookie");

    let req = Request::builder()
        .method("POST")
        .uri("/v1/session/workspace")
        .header("cookie", format!("outturn_session={access}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"workspace_id":"{globex}"}}"#)))
        .expect("request");
    let switched = h.app.clone().oneshot(req).await.expect("response");
    assert_eq!(switched.status(), StatusCode::OK);
    let new_refresh = cookie_named(&switched, "outturn_refresh").expect("new refresh");

    let stale = refresh_with(&h, &old_refresh).await;
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);
    let fresh = refresh_with(&h, &new_refresh).await;
    assert_eq!(fresh.status(), StatusCode::OK);

    finish!(h);
}

#[tokio::test]
async fn removing_an_identity_ends_every_session() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "ident@example.com", acme, "viewer").await;
    let users = h.users.list(None, 100).await.expect("list");
    let user = users
        .iter()
        .find(|u| {
            u.identities
                .iter()
                .any(|i| i.subject == "ident@example.com")
        })
        .expect("user");
    let spare = h
        .users
        .add_password_identity(user.id, "ident-spare@example.com", "correct-horse")
        .await
        .expect("add");

    let first = login_raw(&h, "ident@example.com", acme).await;
    let refresh = cookie_named(&first, "outturn_refresh").expect("refresh cookie");
    let access = cookie_named(&first, "outturn_session").expect("access cookie");

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/users/{}/identities/{}", user.id, spare.id))
        .header("cookie", format!("outturn_session={access}"))
        .header("content-type", "application/json")
        // Their own account, so they prove it is them.
        .body(Body::from(r#"{"current_password":"correct-horse"}"#))
        .expect("request");
    let response = h.app.clone().oneshot(req).await.expect("response");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let after = refresh_with(&h, &refresh).await;
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);

    finish!(h);
}

#[tokio::test]
async fn refresh_picks_up_revoked_roles() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "revoked@example.com", acme, "viewer").await;

    let first = login_raw(&h, "revoked@example.com", acme).await;
    let refresh = cookie_named(&first, "outturn_refresh").expect("refresh cookie");

    // Access is withdrawn while the session is live.
    let users = h.users.list(None, 100).await.expect("list");
    let target = users
        .iter()
        .find(|u| {
            u.identities
                .iter()
                .any(|i| i.subject == "revoked@example.com")
        })
        .expect("user");
    h.users
        .revoke_workspace_role(target.id, acme, "viewer")
        .await
        .expect("revoke");

    // Roles are re-read on refresh, so the session cannot outlive the grant.
    let after = refresh_with(&h, &refresh).await;
    assert_eq!(after.status(), StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn refresh_without_a_cookie_is_unauthorized() {
    let h = harness_or_skip!();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/session/refresh")
        .body(Body::empty())
        .expect("request");
    let response = h.app.clone().oneshot(req).await.expect("response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    finish!(h);
}

/// A person changes their own password with the current one; a wrong current
/// one is refused without signing them out; afterwards the new password signs
/// in and the old does not, and every other session ends while theirs goes on.
#[tokio::test]
async fn a_person_changes_their_own_password() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "pw@example.com", acme, "viewer").await;
    let users = h.users.list(None, 100).await.expect("list");
    let user = users
        .iter()
        .find(|u| u.identities.iter().any(|i| i.subject == "pw@example.com"))
        .expect("user")
        .clone();
    let identity = user.identities[0].id;

    let elsewhere = login_raw(&h, "pw@example.com", acme).await;
    let elsewhere_refresh = cookie_named(&elsewhere, "outturn_refresh").expect("refresh");
    let here = login_raw(&h, "pw@example.com", acme).await;
    let here_refresh = cookie_named(&here, "outturn_refresh").expect("refresh");
    let access = cookie_named(&here, "outturn_session").expect("access");

    let put = |body: serde_json::Value| {
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/v1/users/{}/identities/{identity}/password",
                user.id
            ))
            .header("cookie", format!("outturn_session={access}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    };
    let wrong = h
        .app
        .clone()
        .oneshot(put(serde_json::json!({
            "current_password": "not-it-at-all",
            "new_password": "battery-staple",
        })))
        .await
        .expect("response");
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    let missing = h
        .app
        .clone()
        .oneshot(put(serde_json::json!({ "new_password": "battery-staple" })))
        .await
        .expect("response");
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);

    let changed = h
        .app
        .clone()
        .oneshot(put(serde_json::json!({
            "current_password": "correct-horse",
            "new_password": "battery-staple",
        })))
        .await
        .expect("response");
    assert_eq!(changed.status(), StatusCode::NO_CONTENT);

    assert!(
        h.users
            .authenticate("pw@example.com", "battery-staple")
            .await
            .is_ok()
    );
    assert!(
        h.users
            .authenticate("pw@example.com", "correct-horse")
            .await
            .is_err()
    );
    assert_eq!(
        refresh_with(&h, &elsewhere_refresh).await.status(),
        StatusCode::UNAUTHORIZED,
        "another session outlived the change"
    );
    assert_eq!(
        refresh_with(&h, &here_refresh).await.status(),
        StatusCode::OK,
        "the session that changed it was signed out"
    );

    // Adding a sign-in to their own account takes the same proof, so an open
    // session cannot be used to add a passer-by's own way in.
    let add = |body: serde_json::Value| {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/users/{}/identities", user.id))
            .header("cookie", format!("outturn_session={access}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    };
    let unproven = h
        .app
        .clone()
        .oneshot(add(serde_json::json!({
            "email": "passer-by@example.com",
            "password": "mine-now-1",
        })))
        .await
        .expect("response");
    assert_eq!(unproven.status(), StatusCode::BAD_REQUEST);
    let proven = h
        .app
        .clone()
        .oneshot(add(serde_json::json!({
            "email": "pw-spare@example.com",
            "password": "spare-horse",
            "current_password": "battery-staple",
        })))
        .await
        .expect("response");
    assert_eq!(proven.status(), StatusCode::CREATED);

    finish!(h);
}
