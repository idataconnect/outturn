//! Integration tests against a real Postgres.
//!
//! Built only under the integration-tests feature. Each test runs in its own
//! schema, so they are isolated from each other and safe to run in parallel.
//!
//! `skaffold dev` already forwards postgres to 15432; otherwise run
//! `kubectl port-forward svc/postgres 15432:5432` yourself.
//!
//!   TEST_DATABASE_URL=postgres://outturn:outturn-dev@localhost:15432/outturn_test \
//!     cargo test -- --test-threads=1

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use outturn::api::tenant::{PostgresTenantStore, TenantStore};
use outturn::api::user::{CreateUser, PostgresUserStore, UserStore};
use outturn::api::agent::{AgentStore, PostgresAgentStore};
use outturn::api::chat::{ChatStore, PostgresChatStore};
use outturn::api::session::{PostgresSessionStore, SessionStore};
use outturn::api::{ApiState, routes};
use outturn::auth::{Role, TokenMinter, TokenValidator};
use serde_json::Value;
use sqlx::postgres::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

mod common;

struct Harness {
    app: axum::Router,
    users: Arc<dyn UserStore>,
    tenants: Arc<dyn TenantStore>,
    #[allow(dead_code)]
    sessions: Arc<dyn SessionStore>,
    agents: Arc<dyn AgentStore>,
    db: common::TestDb,
}

async fn harness() -> Harness {
    // A private schema per test, so tests do not see each other's rows and can
    // run in parallel.
    let db = common::TestDb::new().await;
    let pool: PgPool = db.pool.clone();

    let (minter, public_bytes) = TokenMinter::generate().expect("keypair");
    let validator =
        TokenValidator::new(&public_bytes.try_into().expect("32-byte key")).expect("validator");

    let tenants: Arc<dyn TenantStore> = Arc::new(PostgresTenantStore::new(pool.clone()));
    let users: Arc<dyn UserStore> = Arc::new(PostgresUserStore::new(pool.clone()));
    let sessions: Arc<dyn SessionStore> = Arc::new(PostgresSessionStore::new(pool.clone()));
    let agents: Arc<dyn AgentStore> = Arc::new(PostgresAgentStore::new(pool.clone()));
    let chat: Arc<dyn ChatStore> = Arc::new(PostgresChatStore::new(pool.clone()));
    let state = Arc::new(ApiState::new(
        tenants.clone(),
        users.clone(),
        sessions.clone(),
        agents.clone(),
        chat.clone(),
        validator,
        minter,
        pool.clone(),
        outturn::events::EventBus::spawn(pool.clone()),
        Arc::new(tokio::sync::Notify::new()),
    ));

    // The endpoints runtimes use need the worker that prepares and records
    // turns, the same as a real API pod.
    state.set_worker(Arc::new(outturn::api::worker::Worker {
        pool: pool.clone(),
        agents: agents.clone(),
        chat: chat.clone(),
        minter: Arc::new(TokenMinter::generate().expect("keypair").0),
    }));

    Harness {
        app: routes(Arc::clone(&state)),
        users,
        tenants,
        sessions,
        agents,
        db,
    }
}

macro_rules! harness_or_skip {
    () => {
        harness().await
    };
}

/// Drops the test's schema. Skipped on failure, so a failing test leaves its
/// rows behind to inspect.
macro_rules! finish {
    ($h:expr) => {
        $h.db.cleanup().await
    };
}

impl Harness {
    async fn send(&self, req: Request<Body>) -> (StatusCode, String) {
        let (status, body, _) = self.send_full(req).await;
        (status, body)
    }

    /// Also returns the session cookie value, if one was set.
    async fn send_full(&self, req: Request<Body>) -> (StatusCode, String, Option<String>) {
        let response = self.app.clone().oneshot(req).await.expect("response");
        let status = response.status();
        let cookie = response
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .and_then(|v| v.split_once('='))
            .map(|(_, value)| value.to_string());
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned(), cookie)
    }

    async fn post(&self, uri: &str, token: Option<&str>, body: &str) -> (StatusCode, String) {
        let mut req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        self.send(req.body(Body::from(body.to_string())).expect("request"))
            .await
    }

    async fn get(&self, uri: &str, token: Option<&str>) -> (StatusCode, String) {
        let mut req = Request::builder().method("GET").uri(uri);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        self.send(req.body(Body::empty()).expect("request")).await
    }

    /// Creates a user with the given system role and tenant role, then logs in
    /// and returns the scoped token.
    async fn login_as(
        &self,
        email: &str,
        system_role: Option<Role>,
        tenant_role: Option<(Uuid, Role)>,
    ) -> String {
        let user = self
            .users
            .create(CreateUser {
                email: email.into(),
                display_name: "Test User".into(),
                password: "correct-horse".into(),
            })
            .await
            .expect("create user");

        if let Some(role) = system_role {
            self.users
                .grant_system_role(user.id, role)
                .await
                .expect("grant system role");
        }
        if let Some((tenant_id, role)) = tenant_role {
            self.users
                .grant_tenant_role(user.id, tenant_id, role)
                .await
                .expect("grant tenant role");
        }

        let tenant_id = match tenant_role {
            Some((id, _)) => id,
            None => self.tenants.list().await.expect("list")[0].id,
        };

        let req = Request::builder()
            .method("POST")
            .uri("/v1/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"email":"{email}","password":"correct-horse","tenant_id":"{tenant_id}"}}"#
            )))
            .expect("request");

        let (status, body, cookie) = self.send_full(req).await;
        assert_eq!(status, StatusCode::OK, "login failed: {body}");
        cookie.expect("login must set a session cookie")
    }

    async fn make_tenant(&self, name: &str, slug: &str) -> Uuid {
        self.tenants
            .create(outturn::api::tenant::CreateTenant {
                name: name.into(),
                slug: slug.into(),
            })
            .await
            .expect("create tenant")
            .id
    }
}

#[tokio::test]
async fn login_without_tenant_returns_picker() {
    let h = harness_or_skip!();
    let tenant = h.make_tenant("Acme", "acme").await;

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
        .grant_tenant_role(user.id, tenant, Role::Viewer)
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
    assert!(body.contains("select_tenant"), "body: {body}");
    assert!(body.contains("acme"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn bad_password_is_unauthorized() {
    let h = harness_or_skip!();
    h.make_tenant("Acme", "acme").await;
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
async fn system_admin_sees_all_tenants_and_manages_them() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    h.make_tenant("Globex", "globex").await;

    // No explicit grant in either tenant — system admin still reaches them.
    let token = h
        .login_as("admin@example.com", Some(Role::SystemAdmin), Some((acme, Role::Admin)))
        .await;

    let (status, body) = h.get("/v1/tenants", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("Acme") && body.contains("Globex"), "body: {body}");

    let (status, body) = h
        .post(
            "/v1/tenants",
            Some(&token),
            r#"{"name":"Initech","slug":"initech"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn tenant_admin_cannot_manage_tenants() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
        .await;

    let (status, _) = h.get("/v1/tenants", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = h
        .post("/v1/tenants", Some(&token), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn tenant_admin_can_manage_users() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
        .await;

    let (status, body) = h
        .post(
            "/v1/users",
            Some(&token),
            r#"{"email":"new@acme.example","display_name":"New","password":"correct-horse"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, body) = h.get("/v1/users", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("new@acme.example"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn viewer_cannot_manage_users() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("viewer@acme.example", None, Some((acme, Role::Viewer)))
        .await;

    let (status, _) = h.get("/v1/users", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn login_to_unaffiliated_tenant_is_forbidden() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let globex = h.make_tenant("Globex", "globex").await;

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
        .grant_tenant_role(user.id, acme, Role::Viewer)
        .await
        .expect("grant");

    let (status, body) = h
        .post(
            "/v1/login",
            None,
            &format!(
                r#"{{"email":"acme-only@example.com","password":"correct-horse","tenant_id":"{globex}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn tenant_switch_remints_for_new_tenant() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let globex = h.make_tenant("Globex", "globex").await;

    let token = h
        .login_as("admin@example.com", Some(Role::SystemAdmin), Some((acme, Role::Admin)))
        .await;

    let req = Request::builder()
        .method("POST")
        .uri("/v1/session/tenant")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"tenant_id":"{globex}"}}"#)))
        .expect("request");
    let (status, body, new_cookie) = h.send_full(req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let parsed: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["tenant_id"].as_str().unwrap(), globex.to_string());

    // The re-minted token must actually be scoped to the new tenant.
    let new_token = new_cookie.expect("switch must re-set the cookie");
    let (status, body) = h.get("/v1/session", Some(&new_token)).await;
    assert_eq!(status, StatusCode::OK);
    let session: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(session["tenant_id"].as_str().unwrap(), globex.to_string());

    finish!(h);
}

#[tokio::test]
async fn duplicate_email_conflicts() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
        .await;

    let body = r#"{"email":"dup@acme.example","display_name":"Dup","password":"correct-horse"}"#;
    let (status, _) = h.post("/v1/users", Some(&token), body).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = h.post("/v1/users", Some(&token), body).await;
    assert_eq!(status, StatusCode::CONFLICT);

    finish!(h);
}

#[tokio::test]
async fn missing_token_is_unauthorized() {
    let h = harness_or_skip!();
    let (status, _) = h.get("/v1/tenants", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    finish!(h);
}

#[tokio::test]
async fn account_can_have_several_emails() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;

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
        .grant_tenant_role(user.id, acme, Role::Admin)
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
                    r#"{{"email":"{email}","password":"correct-horse","tenant_id":"{acme}"}}"#
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
    h.make_tenant("Acme", "acme").await;

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
    h.make_tenant("Acme", "acme").await;

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
    let acme = h.make_tenant("Acme", "acme").await;

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
        .grant_tenant_role(user.id, acme, Role::Viewer)
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
                r#"{{"email":"drop@example.com","password":"correct-horse","tenant_id":"{acme}"}}"#
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
                r#"{{"email":"keep@example.com","password":"correct-horse","tenant_id":"{acme}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    finish!(h);
}

#[tokio::test]
async fn session_cookie_is_httponly_and_samesite() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;

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
        .grant_tenant_role(user.id, acme, Role::Viewer)
        .await
        .expect("grant");

    let req = Request::builder()
        .method("POST")
        .uri("/v1/login")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"cookie@example.com","password":"correct-horse","tenant_id":"{acme}"}}"#
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
    assert!(!body.contains("\"token\""), "token must not be in the body: {body}");

    finish!(h);
}

#[tokio::test]
async fn cookie_authenticates_subsequent_requests() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let cookie = h
        .login_as("cookieuser@example.com", None, Some((acme, Role::Admin)))
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
    let acme = h.make_tenant("Acme", "acme").await;
    seed_user(&h, "lifetime@example.com", acme, Role::Viewer).await;

    let response = login_raw(&h, "lifetime@example.com", acme).await;
    assert_eq!(response.status(), StatusCode::OK);

    // The access token is deliberately short now that refresh renews it: that
    // is what bounds how long a stolen token or a revoked role stays usable.
    // The refresh token must outlast it by a wide margin, or the user is sent
    // back to the login form during ordinary use.
    assert!(
        outturn::api::session::REFRESH_LIFETIME_SECS
            > outturn::auth::SESSION_TOKEN_LIFETIME_SECS * 100,
        "refresh lifetime {} must dwarf the access lifetime {}",
        outturn::api::session::REFRESH_LIFETIME_SECS,
        outturn::auth::SESSION_TOKEN_LIFETIME_SECS,
    );

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

async fn login_raw(h: &Harness, email: &str, tenant: Uuid) -> axum::response::Response {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/login")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"email":"{email}","password":"correct-horse","tenant_id":"{tenant}"}}"#
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

async fn seed_user(h: &Harness, email: &str, tenant: Uuid, role: Role) {
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
        .grant_tenant_role(user.id, tenant, role)
        .await
        .expect("grant");
}

#[tokio::test]
async fn login_issues_a_path_scoped_refresh_cookie() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    seed_user(&h, "refresh@example.com", acme, Role::Viewer).await;

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
    let acme = h.make_tenant("Acme", "acme").await;
    seed_user(&h, "rotate@example.com", acme, Role::Admin).await;

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
    let acme = h.make_tenant("Acme", "acme").await;
    seed_user(&h, "replay@example.com", acme, Role::Viewer).await;

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
    let acme = h.make_tenant("Acme", "acme").await;
    seed_user(&h, "logout@example.com", acme, Role::Viewer).await;

    let first = login_raw(&h, "logout@example.com", acme).await;
    let refresh = cookie_named(&first, "outturn_refresh").expect("refresh cookie");

    let req = Request::builder()
        .method("POST")
        .uri("/v1/logout")
        .header("cookie", format!("outturn_refresh={refresh}"))
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
async fn refresh_picks_up_revoked_roles() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    seed_user(&h, "revoked@example.com", acme, Role::Viewer).await;

    let first = login_raw(&h, "revoked@example.com", acme).await;
    let refresh = cookie_named(&first, "outturn_refresh").expect("refresh cookie");

    // Access is withdrawn while the session is live.
    let users = h.users.list().await.expect("list");
    let target = users
        .iter()
        .find(|u| {
            u.identities
                .iter()
                .any(|i| i.subject == "revoked@example.com")
        })
        .expect("user");
    h.users
        .revoke_tenant_role(target.id, acme, Role::Viewer)
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

#[tokio::test]
async fn operator_can_manage_agents_in_own_tenant() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("op@acme.example", None, Some((acme, Role::Operator)))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Support","slug":"support","system_prompt":"Be helpful."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, body) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Support"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn agents_are_invisible_across_tenants() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let globex = h.make_tenant("Globex", "globex").await;

    let acme_agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Acme Secret".into(),
                slug: "acme-secret".into(),
                description: String::new(),
                system_prompt: String::new(),
            },
        )
        .await
        .expect("create");

    // A user in Globex must not see or reach Acme's agent, even knowing its id.
    let token = h
        .login_as("user@globex.example", None, Some((globex, Role::Admin)))
        .await;

    let (status, body) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("Acme Secret"), "leaked: {body}");

    let (status, _) = h
        .get(&format!("/v1/agents/{}", acme_agent.id), Some(&token))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another tenant's agent must read as absent"
    );

    finish!(h);
}

#[tokio::test]
async fn system_admin_sees_only_the_tenant_they_are_scoped_to() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let globex = h.make_tenant("Globex", "globex").await;

    h.agents
        .create(
            globex,
            outturn::api::agent::CreateAgent {
                name: "Globex Bot".into(),
                slug: "globex-bot".into(),
                description: String::new(),
                system_prompt: String::new(),
            },
        )
        .await
        .expect("create");

    // Scoped to Acme: system_admin is not a licence to see every tenant at
    // once, it is the ability to mint a token for any of them.
    let token = h
        .login_as("root@example.com", Some(Role::SystemAdmin), Some((acme, Role::Admin)))
        .await;

    let (status, body) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("Globex Bot"),
        "scoped token must not cross tenants: {body}"
    );

    finish!(h);
}

#[tokio::test]
async fn viewer_cannot_create_agents() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("viewer@acme.example", None, Some((acme, Role::Viewer)))
        .await;

    let (status, _) = h
        .post("/v1/agents", Some(&token), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Reading is still allowed.
    let (status, _) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);

    finish!(h);
}

#[tokio::test]
async fn operator_cannot_delete_agents() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Keeper".into(),
                slug: "keeper".into(),
                description: String::new(),
                system_prompt: String::new(),
            },
        )
        .await
        .expect("create");

    let token = h
        .login_as("op2@acme.example", None, Some((acme, Role::Operator)))
        .await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/agents/{}", agent.id))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn slugs_are_unique_per_tenant_not_globally() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let globex = h.make_tenant("Globex", "globex").await;

    let make = || outturn::api::agent::CreateAgent {
        name: "Support".into(),
        slug: "support".into(),
        description: String::new(),
        system_prompt: String::new(),
    };

    h.agents.create(acme, make()).await.expect("acme");
    // Two tenants may both have a "support" agent without colliding.
    h.agents.create(globex, make()).await.expect("globex");

    // But not twice within one tenant.
    let dup = h.agents.create(acme, make()).await;
    assert!(dup.is_err(), "duplicate slug within a tenant must be refused");

    finish!(h);
}

#[tokio::test]
async fn partial_update_leaves_other_fields_intact() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Original".into(),
                slug: "original".into(),
                description: "keep me".into(),
                system_prompt: "keep this too".into(),
            },
        )
        .await
        .expect("create");

    let token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
        .await;

    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/agents/{}", agent.id))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"Renamed"}"#))
        .expect("request");

    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("Renamed"), "body: {body}");
    assert!(body.contains("keep me"), "description must survive: {body}");
    assert!(body.contains("keep this too"), "prompt must survive: {body}");

    finish!(h);
}

// -- Egress rules -------------------------------------------------------------

/// Adding an API is meant to be one paste of whatever the docs showed.
#[tokio::test]
async fn a_pasted_url_becomes_an_egress_rule() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
        .await;

    // Nothing to begin with, which is what an agent can reach to begin with.
    let (status, body) = h.get("/v1/egress-rules", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, "[]");

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
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
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
    ] {
        let (status, body) = h.post("/v1/egress-rules", Some(&token), input).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{input} gave: {body}");
        assert!(body.contains(expected), "{input} gave: {body}");
    }
}

/// One rule per host, so which credential travels does not depend on
/// insertion order.
#[tokio::test]
async fn a_host_cannot_be_allowed_twice() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
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

/// One tenant's list is not another's, and neither is reachable from the
/// other's token.
#[tokio::test]
async fn egress_rules_do_not_cross_tenants() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let globex = h.make_tenant("Globex", "globex").await;
    let acme_token = h
        .login_as("admin@acme.example", None, Some((acme, Role::Admin)))
        .await;
    let globex_token = h
        .login_as("admin@globex.example", None, Some((globex, Role::Admin)))
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
    assert_eq!(body, "[]", "one tenant saw another's rules: {body}");

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
        "a tenant deleted another tenant's rule"
    );

    let (status, body) = h.get("/v1/egress-rules", Some(&acme_token)).await;
    assert!(body.contains("api.acme.example"), "status {status}: {body}");
}

// -- Work distribution --------------------------------------------------------

/// Nothing is handed out to a caller that is not the platform's own tier.
#[tokio::test]
async fn taking_work_needs_more_than_a_tenant_token() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let member = h
        .login_as("member@acme.example", None, Some((acme, Role::Viewer)))
        .await;

    let (status, body) = h.post("/v1/work", Some(&member), "{}").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    let (status, _) = h.post("/v1/work", None, "{}").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// A turn may only be reported while it is out with a runtime.
///
/// The claim is the ticket. Without this check a job id is enough to write
/// into a transcript -- including a second time, over a reply already
/// finished by the runtime that really ran it.
#[tokio::test]
async fn a_turn_that_is_not_running_cannot_be_reported() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let operator = h
        .login_as("runtime@acme.example", None, Some((acme, Role::Operator)))
        .await;

    // Queued but never handed out, so nothing is running it.
    let job = outturn::jobs::enqueue(
        &h.db.pool,
        acme,
        "chat.turn",
        serde_json::json!({}),
        None,
        None,
    )
    .await
    .expect("enqueue");

    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {operator}"))
        .body(Body::from("{}\n"))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a job nobody is running was reported: {body}"
    );
}

/// An empty queue is answered with nothing, not an error.
#[tokio::test]
async fn asking_for_work_when_there_is_none_says_so() {
    let h = harness_or_skip!();
    let acme = h.make_tenant("Acme", "acme").await;
    let operator = h
        .login_as("runtime@acme.example", None, Some((acme, Role::Operator)))
        .await;

    let (status, body) = h.post("/v1/work", Some(&operator), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, "null", "an idle cluster should answer null, got {body}");
}
