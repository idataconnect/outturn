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
use outturn::api::workspace::{PostgresWorkspaceStore, WorkspaceStore};
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
    workspaces: Arc<dyn WorkspaceStore>,
    #[allow(dead_code)]
    sessions: Arc<dyn SessionStore>,
    agents: Arc<dyn AgentStore>,
    skills: Arc<dyn outturn::api::skill::SkillStore>,
    db: common::TestDb,
    /// Signs with the same key the app validates against, so a test can issue
    /// the platform's own credentials the way the platform does.
    minter: TokenMinter,
    roles: Arc<dyn outturn::api::role::RoleStore>,
}

/// What the test's pretend runtime presents to the work endpoints.
const TEST_RUNTIME_KEY: &str = "test-runtime-key-test-runtime-key-test-runtime-key";

async fn harness() -> Harness {
    // A private schema per test, so tests do not see each other's rows and can
    // run in parallel.
    let db = common::TestDb::new().await;
    let pool: PgPool = db.pool.clone();

    // One seed, two minters: the app's, and one the test uses to issue the
    // platform's own credentials the way the platform does.
    let seed = {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        seed
    };
    let minter = TokenMinter::new(&seed).expect("minter");
    let test_minter = TokenMinter::new(&seed).expect("minter");
    let public_bytes = {
        use ed25519_dalek::SigningKey;
        SigningKey::from_bytes(&seed).verifying_key().to_bytes().to_vec()
    };
    let validator = TokenValidator::new(
        &public_bytes.try_into().expect("32-byte key"),
        outturn::auth::AUDIENCE_API,
    )
    .expect("validator");
    let runtime_key = outturn::auth::RuntimeKey::new(TEST_RUNTIME_KEY).expect("runtime key");

    let workspaces: Arc<dyn WorkspaceStore> = Arc::new(PostgresWorkspaceStore::new(pool.clone()));
    let users: Arc<dyn UserStore> = Arc::new(PostgresUserStore::new(pool.clone()));
    let sessions: Arc<dyn SessionStore> = Arc::new(PostgresSessionStore::new(pool.clone()));
    let agents: Arc<dyn AgentStore> = Arc::new(PostgresAgentStore::new(pool.clone()));
    let chat: Arc<dyn ChatStore> = Arc::new(PostgresChatStore::new(pool.clone()));
    let roles: Arc<dyn outturn::api::role::RoleStore> =
        Arc::new(outturn::api::role::PostgresRoleStore::new(pool.clone()));
    let usage: Arc<dyn outturn::api::usage::UsageStore> =
        Arc::new(outturn::api::usage::PostgresUsageStore::new(pool.clone()));
    let settings: Arc<dyn outturn::api::settings::SettingsStore> =
        Arc::new(outturn::api::settings::PostgresSettingsStore::new(pool.clone()));
    let skills: Arc<dyn outturn::api::skill::SkillStore> =
        Arc::new(outturn::api::skill::PostgresSkillStore::new(pool.clone()));
    let state = Arc::new(ApiState::new(
        workspaces.clone(),
        users.clone(),
        sessions.clone(),
        agents.clone(),
        skills.clone(),
        chat.clone(),
        roles.clone(),
        usage.clone(),
        settings.clone(),
        Some(Arc::new(outturn::runtime::storage::MemoryStorage::new())),
        validator,
        minter,
        runtime_key,
        pool.clone(),
        outturn::events::EventBus::spawn(pool.clone()),
        Arc::new(tokio::sync::Notify::new()),
    ));

    // The endpoints runtimes use need the worker that prepares and records
    // turns, the same as a real API pod.
    state.set_worker(Arc::new(outturn::api::worker::Worker {
        pool: pool.clone(),
        agents: agents.clone(),
        skills: skills.clone(),
        chat: chat.clone(),
        usage: usage.clone(),
        settings: settings.clone(),
        inhibitors: Arc::new(outturn::api::inhibitor::PostgresInhibitorStore::new(pool.clone())),
        // No gateway, so no summarising: the trim underneath carries the
        // whole of what these tests exercise.
        minter: None,
        gateway_url: None,
    }));

    Harness {
        app: routes(Arc::clone(&state)),
        users,
        workspaces,
        sessions,
        agents,
        skills,
        db,
        minter: test_minter,
        roles,
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

    /// The credential the tier that runs turns presents.
    ///
    /// A shared key rather than a token: `Role::Runtime` is granted to no user,
    /// cannot be parsed from a role name, and is never signed into anything.
    /// The runtime holds no signing key at all, which is the point of it.
    fn runtime_token(&self, _workspace: Uuid) -> String {
        TEST_RUNTIME_KEY.to_string()
    }

    /// Creates a user with the given system role and workspace role, then logs in
    /// and returns the scoped token.
    async fn login_as(
        &self,
        email: &str,
        system_role: Option<Role>,
        workspace_role: Option<(Uuid, &str)>,
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
        if let Some((workspace_id, role)) = workspace_role {
            self.users
                .grant_workspace_role(user.id, workspace_id, role)
                .await
                .expect("grant workspace role");
        }

        let workspace_id = match workspace_role {
            Some((id, _)) => id,
            None => self.workspaces.list().await.expect("list")[0].id,
        };

        let req = Request::builder()
            .method("POST")
            .uri("/v1/login")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"email":"{email}","password":"correct-horse","workspace_id":"{workspace_id}"}}"#
            )))
            .expect("request");

        let (status, body, cookie) = self.send_full(req).await;
        assert_eq!(status, StatusCode::OK, "login failed: {body}");
        cookie.expect("login must set a session cookie")
    }

    async fn make_workspace(&self, name: &str, slug: &str) -> Uuid {
        let id = self.make_bare_workspace(name, slug).await;
        // As the API does on creation: a workspace with no roles is one nobody
        // can be given access to.
        self.roles.seed_defaults(id).await.expect("seed roles");
        id
    }

    async fn make_bare_workspace(&self, name: &str, slug: &str) -> Uuid {
        self.workspaces
            .create(outturn::api::workspace::CreateWorkspace {
                name: name.into(),
                slug: slug.into(),
            })
            .await
            .expect("create workspace")
            .id
    }
}

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
async fn system_admin_sees_all_workspaces_and_manages_them() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    h.make_workspace("Globex", "globex").await;

    // No explicit grant in either workspace — system admin still reaches them.
    let token = h
        .login_as("admin@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;

    let (status, body) = h.get("/v1/workspaces", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("Acme") && body.contains("Globex"), "body: {body}");

    let (status, body) = h
        .post(
            "/v1/workspaces",
            Some(&token),
            r#"{"name":"Initech","slug":"initech"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn workspace_admin_cannot_manage_workspaces() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, _) = h.get("/v1/workspaces", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = h
        .post("/v1/workspaces", Some(&token), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn workspace_admin_can_manage_users() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/users",
            Some(&token),
            r#"{"email":"new@acme.example","display_name":"New","password":"correct-horse","role":"viewer"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, body) = h.get("/v1/users", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("new@acme.example"), "body: {body}");

    finish!(h);
}

/// A workspace's administrator sees their own workspace's accounts and nobody else's.
///
/// The user list used to be every account on the platform, which named every
/// other customer's staff to anyone holding users:read in any workspace.
#[tokio::test]
async fn a_workspace_admin_sees_only_their_own_workspaces_users() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;
    let acme_admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    h.login_as("someone@globex.example", None, Some((globex, "viewer")))
        .await;

    let (status, body) = h.get("/v1/users", Some(&acme_admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("admin@acme.example"), "body: {body}");
    assert!(
        !body.contains("globex.example"),
        "another workspace's account was listed: {body}"
    );

    // A system administrator still sees everyone.
    let root = h
        .login_as("root@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;
    let (status, body) = h.get("/v1/users", Some(&root)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("globex.example"), "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn viewer_cannot_manage_users() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
        .await;

    let (status, _) = h.get("/v1/users", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

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
        .login_as("admin@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
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
    assert_eq!(session["workspace_id"].as_str().unwrap(), globex.to_string());

    finish!(h);
}

#[tokio::test]
async fn duplicate_email_conflicts() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
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
    assert!(!body.contains("\"token\""), "token must not be in the body: {body}");

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
    let acme = h.make_workspace("Acme", "acme").await;
    seed_user(&h, "revoked@example.com", acme, "viewer").await;

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

#[tokio::test]
async fn operator_can_manage_agents_in_own_workspace() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
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
async fn agents_are_invisible_across_workspaces() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    let acme_agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Acme Secret".into(),
                slug: "acme-secret".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("create");

    // A user in Globex must not see or reach Acme's agent, even knowing its id.
    let token = h
        .login_as("user@globex.example", None, Some((globex, "admin")))
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
        "another workspace's agent must read as absent"
    );

    finish!(h);
}

#[tokio::test]
async fn system_admin_sees_only_the_workspace_they_are_scoped_to() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    h.agents
        .create(
            globex,
            outturn::api::agent::CreateAgent {
                name: "Globex Bot".into(),
                slug: "globex-bot".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("create");

    // Scoped to Acme: system_admin is not a licence to see every workspace at
    // once, it is the ability to mint a token for any of them.
    let token = h
        .login_as("root@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;

    let (status, body) = h.get("/v1/agents", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("Globex Bot"),
        "scoped token must not cross workspaces: {body}"
    );

    finish!(h);
}

#[tokio::test]
async fn viewer_cannot_create_agents() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
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
    let acme = h.make_workspace("Acme", "acme").await;
    let agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Keeper".into(),
                slug: "keeper".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("create");

    let token = h
        .login_as("op2@acme.example", None, Some((acme, "operator")))
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
async fn slugs_are_unique_per_workspace_not_globally() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;

    let make = || outturn::api::agent::CreateAgent {
        name: "Support".into(),
        slug: "support".into(),
        description: String::new(),
        system_prompt: String::new(),
                policy: None,
    };

    h.agents.create(acme, make()).await.expect("acme");
    // Two workspaces may both have a "support" agent without colliding.
    h.agents.create(globex, make()).await.expect("globex");

    // But not twice within one workspace.
    let dup = h.agents.create(acme, make()).await;
    assert!(dup.is_err(), "duplicate slug within a workspace must be refused");

    finish!(h);
}

#[tokio::test]
async fn partial_update_leaves_other_fields_intact() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let agent = h
        .agents
        .create(
            acme,
            outturn::api::agent::CreateAgent {
                name: "Original".into(),
                slug: "original".into(),
                description: "keep me".into(),
                system_prompt: "keep this too".into(),
                policy: None,
            },
        )
        .await
        .expect("create");

    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
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
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
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
    assert_eq!(body, "[]", "one workspace saw another's rules: {body}");

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

// -- Work distribution --------------------------------------------------------

/// No workspace role, however senior, can take work off the queue.
///
/// A turn handed out carries whichever workspace's transcript it belongs to,
/// that workspace's egress rules, and a token minted for it -- so anything that
/// can ask for work can ask for everyone's. Checking only that a Viewer is
/// refused would pass while an Admin walked through, which is exactly what
/// happened when these endpoints were gated on an authority workspaces hold.
#[tokio::test]
async fn no_workspace_role_can_take_work() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;

    for (email, role) in [
        ("viewer@acme.example", "viewer"),
        ("operator@acme.example", "operator"),
        ("admin@acme.example", "admin"),
    ] {
        let token = h.login_as(email, None, Some((acme, role))).await;
        let (status, body) = h.post("/v1/work", Some(&token), "{}").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{role:?} was handed work: {body}"
        );
    }

    // And a system administrator is a person, not the tier that runs turns.
    let sysadmin = h
        .login_as("root@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;
    let (status, body) = h.post("/v1/work", Some(&sysadmin), "{}").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a system admin took work: {body}");

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
    let acme = h.make_workspace("Acme", "acme").await;
    let runtime = h.runtime_token(acme);

    // Queued but never handed out, so nothing is running it.
    let job = outturn::jobs::enqueue(
        &h.db.pool,
        acme,
        "chat.turn",
        serde_json::json!({}),
        None,
        None,
        outturn::jobs::PRIORITY_BACKGROUND,
    )
    .await
    .expect("enqueue");

    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, Uuid::now_v7().to_string())
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
    let acme = h.make_workspace("Acme", "acme").await;
    let runtime = h.runtime_token(acme);

    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, "null", "an idle cluster should answer null, got {body}");
}

/// A message absorbed by an attempt that was lost is offered to the retry.
///
/// The gateway marks a message as taken when it hands it over. If the pod
/// running that turn then dies, the retry starts over from the prompt -- and
/// unless the mark is cleared, the taken message is never handed over again,
/// while its own turn completes as "already answered". The message vanishes.
#[tokio::test]
async fn a_message_absorbed_by_a_lost_attempt_is_offered_to_the_retry() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{}","title":""}}"#, agent["id"].as_str().expect("id")),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    let session_id: Uuid = session["id"].as_str().expect("id").parse().expect("uuid");

    // The prompt, taken by a runtime.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"one"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job: Uuid = assignment["job_id"].as_str().expect("job").parse().expect("uuid");
    let lease = assignment["lease_token"].as_str().expect("lease").to_string();
    let reply: Uuid = assignment["reply_id"].as_str().expect("reply").parse().expect("uuid");

    // A second message arrives mid-turn and the gateway hands it over.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"two"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let taken = outturn::gateway::routing::take_pending(&h.db.pool, session_id, reply)
        .await
        .expect("take");
    assert_eq!(taken.len(), 1, "the steer should have been handed over once");

    // The runtime dies without reporting, and hands the turn back.
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/abandon"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, &lease)
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The retry is handed the same turn, and the steer is pending again.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let retry: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    assert_eq!(retry["job_id"], assignment["job_id"], "the retry should be the same job");

    let again = outturn::gateway::routing::take_pending(&h.db.pool, session_id, reply)
        .await
        .expect("take");
    assert_eq!(
        again.len(),
        1,
        "the message the lost attempt absorbed was never offered to the retry"
    );
    assert_eq!(again[0].content, "two");
}

// -- Roles as data -----------------------------------------------------------

/// A workspace defines its own roles, and an edit takes effect on the next request.
#[tokio::test]
async fn a_workspace_can_define_a_role_and_it_works_at_once() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"analyst","description":"Reads things","authorities":["agents:read","sessions:read"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let role: serde_json::Value = serde_json::from_str(&body).expect("role");

    // Somebody holding only that role can read agents and nothing more.
    let analyst = h
        .login_as("ann@acme.example", None, Some((acme, "analyst")))
        .await;
    let (status, _) = h.get("/v1/agents", Some(&analyst)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .post("/v1/agents", Some(&analyst), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The role is edited to allow it, and the same token now may.
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/roles/{}", role["id"].as_str().expect("id")))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"authorities":["agents:read","agents:create","sessions:read"]}"#))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = h
        .post("/v1/agents", Some(&analyst), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the role edit did not reach a token minted before it: {body}"
    );

    finish!(h);
}

/// What a role may bundle is bounded twice: by the platform, and by its editor.
#[tokio::test]
async fn a_role_cannot_reach_past_the_platform_or_its_editor() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    // Reserved to the platform, whoever asks.
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"overlord","authorities":["workspaces:create"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a workspace role took workspaces:create: {body}");
    let (status, body) = h
        .post("/v1/roles", Some(&admin), r#"{"name":"taker","authorities":["work:take"]}"#)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a workspace role took work:take: {body}");

    // A platform role's name cannot be reused.
    let (status, _) = h
        .post("/v1/roles", Some(&admin), r#"{"name":"system_admin","authorities":[]}"#)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An editor with roles:manage but without agents:delete cannot hand it out.
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"role-editor","authorities":["roles:manage","agents:read"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let editor = h
        .login_as("ed@acme.example", None, Some((acme, "role-editor")))
        .await;
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&editor),
            r#"{"name":"ladder","authorities":["agents:delete"]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an editor granted an authority they do not hold: {body}"
    );

    finish!(h);
}

/// A role somebody holds stays; a name the workspace has no role for is refused.
#[tokio::test]
async fn roles_in_use_stay_and_unknown_roles_cannot_be_granted() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h.get("/v1/roles", Some(&admin)).await;
    let roles: Vec<serde_json::Value> = serde_json::from_str(&body).expect("roles");
    let admin_role = roles
        .iter()
        .find(|r| r["name"] == "admin")
        .expect("the default admin role");
    assert_eq!(admin_role["holders"], 1);

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/roles/{}", admin_role["id"].as_str().expect("id")))
        .header("authorization", format!("Bearer {admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::CONFLICT, "a held role was deleted: {body}");

    let (status, body) = h
        .post(
            "/v1/users",
            Some(&admin),
            r#"{"email":"x@acme.example","display_name":"X","password":"correct-horse","role":"wizard"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a role that does not exist was granted: {body}");

    finish!(h);
}

// -- Usage ledger ---------------------------------------------------------------

/// Every model call a turn reports becomes a ledger row, tagged with the
/// session's account, and the export pages through them by cursor.
#[tokio::test]
async fn each_model_call_is_written_to_the_ledger_and_exported() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":"","account":"hoa-sunnyvale"}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    assert_eq!(session["account"], "hoa-sunnyvale", "the account was not stored: {body}");
    let session_id = session["id"].as_str().expect("id");

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
    let lease = assignment["lease_token"].as_str().expect("lease");

    // A turn of two model calls: the second fell back to another endpoint.
    let stream = [
        r#"{"kind":"delta","idx":0,"text":"Hi"}"#,
        r#"{"kind":"usage","round":0,"endpoint":"openai:http://vllm:8000","model":"qwen","paid_by":"operator","prompt_tokens":10,"completion_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0}"#,
        r#"{"kind":"usage","round":1,"endpoint":"anthropic:https://api.anthropic.com","model":"claude-sonnet-5","paid_by":"operator","prompt_tokens":20,"completion_tokens":5,"cache_read_tokens":3,"cache_write_tokens":0,"reasoning_tokens":1,"service_tier":"priority","provider_usage":{"input_tokens":20,"cache_creation":{"ephemeral_1h_input_tokens":7}}}"#,
        r#"{"kind":"done","content":"Hi","prompt_tokens":30,"completion_tokens":7,"cache_read_tokens":3,"cache_write_tokens":0,"reasoning_tokens":1,"provider":"anthropic:https://api.anthropic.com"}"#,
    ]
    .join("\n");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    // Paged one at a time, to prove the cursor.
    let (status, body) = h.get("/v1/usage?limit=1", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    let first = &page["entries"][0];
    assert_eq!(first["round"], 0);
    assert_eq!(first["model"], "qwen");
    assert_eq!(first["account"], "hoa-sunnyvale");
    assert_eq!(first["session_id"], session_id);
    assert_eq!(first["credential_owner"], "operator");
    assert_eq!(first["prompt_tokens"], 10);
    // This round carried no usage object, so its numbers came from nowhere a
    // bill can be argued from. The row says so rather than presenting them as
    // measured.
    assert_eq!(
        first["usage_source"], "unknown",
        "a round with no provider usage was recorded as though somebody had \
         counted it: {body}"
    );
    let next = page["next"].as_str().expect("cursor");

    let (status, body) = h
        .get(&format!("/v1/usage?limit=1&after={next}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    assert_eq!(page["entries"][0]["round"], 1);
    assert_eq!(page["entries"][0]["model"], "claude-sonnet-5");
    assert_eq!(page["entries"][0]["reasoning_tokens"], 1);
    // What the provider said, verbatim, beside what was normalised from it.
    assert_eq!(page["entries"][0]["service_tier"], "priority");
    assert_eq!(
        page["entries"][0]["provider_usage"]["cache_creation"]["ephemeral_1h_input_tokens"],
        7,
        "the raw usage object was not kept: {body}"
    );
    let next = page["next"].as_str().expect("cursor");

    let (_, body) = h
        .get(&format!("/v1/usage?limit=1&after={next}"), Some(&admin))
        .await;
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    assert!(page["next"].is_null(), "the ledger should be exhausted: {body}");

    // Another workspace's ledger is not this admin's to read.
    let globex = h.make_workspace("Globex", "globex").await;
    let (status, _) = h
        .get(&format!("/v1/usage?workspace_id={globex}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

// -- Settings cascade -----------------------------------------------------------

/// A value walks down from the operator until a level overrides it, and a
/// cleared override falls back to whatever is above.
#[tokio::test]
async fn settings_cascade_from_operator_to_workspace_to_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let root = h
        .login_as("root@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    // Nothing set anywhere: the catalogue default applies.
    let (status, body) = h.get("/v1/settings", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view.iter().find(|s| s["key"] == "reasoning_effort").expect("effort");
    // Low rather than none: thinking off leaves a tool turn silent on some
    // models, so the catalogue default was raised.
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
        .send(req("PUT", "/v1/platform/settings/reasoning_effort".into(), &root, r#"{"value":"low"}"#))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // A workspace admin may not.
    let (status, _) = h
        .send(req("PUT", "/v1/platform/settings/reasoning_effort".into(), &admin, r#"{"value":"high"}"#))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The workspace now inherits it.
    let (_, body) = h.get("/v1/settings", Some(&admin)).await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view.iter().find(|s| s["key"] == "reasoning_effort").expect("effort");
    assert_eq!(effort["value"], "low");
    assert_eq!(effort["source"], "operator");
    assert!(effort["override_value"].is_null(), "no row at the workspace level yet");

    // The workspace overrides; an agent inherits the workspace's value.
    let (status, _) = h
        .send(req("PUT", "/v1/settings/reasoning_effort".into(), &admin, r#"{"value":"high"}"#))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");
    let (_, body) = h.get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin)).await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view.iter().find(|s| s["key"] == "reasoning_effort").expect("effort");
    assert_eq!(effort["value"], "high");
    assert_eq!(effort["source"], "workspace");
    assert_eq!(effort["inherited"], "high");

    // Bad values are refused by the catalogue, not stored.
    let (status, body) = h
        .send(req("PUT", format!("/v1/agents/{agent_id}/settings/temperature"), &admin, r#"{"value":9}"#))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // Clearing the workspace's override falls back to the operator's value.
    let (status, _) = h
        .send(req("DELETE", "/v1/settings/reasoning_effort".into(), &admin, ""))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h.get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin)).await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view.iter().find(|s| s["key"] == "reasoning_effort").expect("effort");
    assert_eq!(effort["value"], "low");
    assert_eq!(effort["source"], "operator");

    finish!(h);
}

// -- Files -----------------------------------------------------------------------

/// A file a person uploads is one the agent can name, and who may write where
/// follows the storage authorities.
#[tokio::test]
async fn uploads_land_in_the_scope_the_agent_reads_them_from() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let viewer = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
        .await;

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{}","title":""}}"#, agent["id"].as_str().expect("id")),
        )
        .await;
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    let session_id = session["id"].as_str().expect("id");

    let put = |token: &str, scoped: &str, bytes: &str| {
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/agent-sessions/{session_id}/files/{scoped}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes.to_string()))
            .expect("request")
    };

    // The admin puts a file in session scope; it lists under the name the
    // agent would use.
    let (status, body) = h.send(put(&admin, "session/report.txt", "quarterly")).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(body.contains(r#""path":"session/report.txt""#), "body: {body}");

    let (status, body) = h
        .get(&format!("/v1/agent-sessions/{session_id}/files"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("session/report.txt"), "body: {body}");

    // It comes back out as bytes.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session_id}/files/session/report.txt"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "quarterly");

    // A viewer may read the conversation's files but not add to the
    // workspace's; the admin may.
    let (status, body) = h.send(put(&viewer, "workspace/pricing.csv", "x")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a viewer wrote workspace files: {body}");
    let (status, body) = h.send(put(&viewer, "session/mine.txt", "x")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a viewer wrote session files without sessions:create: {body}");
    let (status, body) = h.send(put(&admin, "workspace/pricing.csv", "x")).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // Traversal in an upload path is refused, not repaired.
    let (status, _) = h.send(put(&admin, "session/../workspace/oops.txt", "x")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    finish!(h);
}

/// Only the runtime key takes work; a signed token never does.
///
/// The runtime holds no signing key, so nothing it could present is a token.
/// Conversely a turn token -- which the runtime does hold, one per turn --
/// must not open the work endpoints, or a leaked one would let its holder ask
/// for everyone's turns.
#[tokio::test]
async fn only_the_runtime_key_takes_work() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;

    let wrong = "not-the-key-not-the-key-not-the-key-no";
    let (status, body) = h.post("/v1/work", Some(wrong), "{}").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "a wrong key was accepted: {body}");

    let turn = h
        .minter
        .mint_turn(Uuid::now_v7(), acme, outturn::egress::commit::empty_root())
        .expect("token");
    let (status, body) = h.post("/v1/work", Some(&turn), "{}").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a turn token opened the work endpoint: {body}"
    );

    let (status, body) = h.post("/v1/work", Some(&h.runtime_token(acme)), "{}").await;
    assert_eq!(status, StatusCode::OK, "the runtime key was refused: {body}");
}

/// An agent's policy can be set when it is created, not only afterwards.
///
/// Everything in the policy affects the very first turn: which model runs it,
/// which route serves it, and whether the model deliberates before answering.
/// Creating an agent without one meant every agent made through the product
/// ran with thinking on, which on a local model is half a minute of silence
/// before the first visible character — and the only cure was an UPDATE.
#[tokio::test]
async fn an_agent_can_be_created_with_a_policy() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Quick","slug":"quick","policy":{"reasoning_effort":"none"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(
        body.contains(r#""reasoning_effort":"none""#),
        "the policy was dropped on the way in: {body}"
    );

    // And an agent created without one still gets a usable empty policy
    // rather than null, which the turn path reads with `.get`.
    let (status, body) = h
        .post("/v1/agents", Some(&token), r#"{"name":"Plain","slug":"plain"}"#)
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(body.contains(r#""policy":{}"#), "body: {body}");
}

// Skills -----------------------------------------------------------------------

/// A workspace reads the operator's skills and cannot write them.
///
/// This is the line the whole feature stands on: an operator ships an
/// integration to every customer, and a customer who could edit it in place
/// would be editing everyone's.
#[tokio::test]
async fn a_workspace_reads_the_operators_skills_but_cannot_edit_them() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as("op@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "operator could not ship a skill: {body}");
    let shipped: serde_json::Value = serde_json::from_str(&body).expect("json");
    let skill_id = shipped["id"].as_str().expect("id").to_string();

    // A workspace admin, who is not the operator.
    let admin = h.login_as("admin@acme.example", None, Some((acme, "admin"))).await;

    let (status, body) = h.get("/v1/skills", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"crm\""), "the operator's skill was not visible: {body}");

    // Editing it is not forbidden but absent: the workspace's own id is what
    // the write matches on, so there is nothing there to change.
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/skills/{skill_id}"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"Ours now"}"#))
        .unwrap();
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a workspace edited the operator's skill");

    // And the platform route is closed to them.
    let (status, _) = h
        .post(
            "/v1/platform/skills",
            Some(&admin),
            r#"{"slug":"sneaky","name":"Sneaky","body":"x"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a workspace shipped a platform skill");
}

/// An override composes after the prose it speaks about, and applies wherever
/// its base is used without being bound itself.
#[tokio::test]
async fn an_override_composes_after_its_base_and_needs_no_binding() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as("op2@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    let base: serde_json::Value = serde_json::from_str(&body).expect("json");
    let base_id = base["id"].as_str().expect("id").to_string();

    // The workspace writes its variation once, against the operator's skill.
    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(
                r#"{{"slug":"crm-ours","name":"CRM (ours)","body":"Our region is on v2.","base_skill_id":"{base_id}"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "override refused: {body}");

    // An agent binds the base alone.
    let (_, body) = h
        .post("/v1/agents", Some(&operator), r#"{"name":"Helper","slug":"helper"}"#)
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("json");
    let agent_id = agent["id"].as_str().expect("id").to_string();

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {operator}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{base_id}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "binding refused: {body}");

    let composed = h
        .skills
        .resolve_for_agent(acme, agent_id.parse().unwrap())
        .await
        .expect("resolve");

    assert_eq!(composed.len(), 2, "expected base and override: {composed:?}");
    assert_eq!(composed[0].body, "Call the v1 endpoint.");
    assert_eq!(composed[1].body, "Our region is on v2.", "the override must come last");
}

/// An override is not a thing an agent is given on its own.
#[tokio::test]
async fn an_override_cannot_be_bound_by_itself() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as("op3@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post("/v1/skills", Some(&operator), r#"{"slug":"base","name":"Base","body":"b"}"#)
        .await;
    let base_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(r#"{{"slug":"ov","name":"Ov","body":"o","base_skill_id":"{base_id}"}}"#),
        )
        .await;
    let override_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A second override of the same base has no defined order against the first.
    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(r#"{{"slug":"ov2","name":"Ov2","body":"o2","base_skill_id":"{base_id}"}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a second override was allowed: {body}");

    let (_, body) = h
        .post("/v1/agents", Some(&operator), r#"{"name":"H","slug":"h"}"#)
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {operator}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{override_id}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an override was bound directly: {body}");
}

/// When the operator edits a skill, an override written against the old version
/// is reported as stale rather than left to rot quietly.
#[tokio::test]
async fn editing_a_base_marks_the_overrides_written_against_it() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as("op4@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"v1 endpoint"}"#,
        )
        .await;
    let base_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(r#"{{"slug":"ours","name":"Ours","body":"stay on v1","base_skill_id":"{base_id}"}}"#),
        )
        .await;
    let override_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = h.get(&format!("/v1/skills/{override_id}"), Some(&operator)).await;
    let before: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(before["base_moved"], false, "fresh override reported as stale");

    // The operator ships a new version of the base.
    let (status, body) = h
        .post(
            &format!("/v1/platform/skills/{base_id}/versions"),
            Some(&operator),
            r#"{"body":"v2 endpoint","note":"v2 migration"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "publish refused: {body}");

    let (_, body) = h.get(&format!("/v1/skills/{override_id}"), Some(&operator)).await;
    let after: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        after["base_moved"], true,
        "the override was not reported stale after its base moved: {body}"
    );
}

/// Rolling back writes the old body forward, so what was live on any day stays
/// answerable from the history alone.
#[tokio::test]
async fn a_rollback_appends_rather_than_moving_backwards() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h.login_as("a@acme.example", None, Some((acme, "admin"))).await;

    let (_, body) = h
        .post("/v1/skills", Some(&admin), r#"{"slug":"s","name":"S","body":"one"}"#)
        .await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    h.post(
        &format!("/v1/skills/{id}/versions"),
        Some(&admin),
        r#"{"body":"two","note":"second"}"#,
    )
    .await;
    // The rollback: version one's body, sent forward as version three.
    h.post(
        &format!("/v1/skills/{id}/versions"),
        Some(&admin),
        r#"{"body":"one","note":"rolled back to v1"}"#,
    )
    .await;

    let (_, body) = h.get(&format!("/v1/skills/{id}/versions"), Some(&admin)).await;
    let versions: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(versions.len(), 3, "a rollback lost history: {body}");
    assert_eq!(versions[0]["ordinal"], 3, "newest first");
    assert_eq!(versions[0]["body"], "one", "the rollback did not carry the old body");

    let (_, body) = h.get(&format!("/v1/skills/{id}"), Some(&admin)).await;
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(skill["ordinal"], 3, "the live version is the newest, not the oldest");
}

/// A turn is given its skills, and what it was given is written down.
///
/// This is the end the whole feature exists for: prose the operator shipped,
/// varied by the workspace, reaching the model in an order that makes the
/// variation the one that stands -- and a record afterwards of exactly which
/// versions did it, since an eval cannot measure what it cannot name.
#[tokio::test]
async fn a_turn_is_composed_from_its_skills_and_the_versions_are_recorded() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as("op5@example.com", Some(Role::SystemAdmin), Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    let base_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, _) = h
        .post(
            "/v1/skills",
            Some(&operator),
            &format!(
                r#"{{"slug":"ours","name":"CRM (ours)","body":"Our region is on v2.","base_skill_id":"{base_id}"}}"#
            ),
        )
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&operator),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Only the base is bound. The override rides along because it is the
    // workspace's standing variation on it.
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent_id}/skills"))
        .header("authorization", format!("Bearer {operator}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{base_id}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "binding refused: {body}");

    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&operator),
            &format!(r#"{{"agent_id":"{agent_id}","title":""}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&operator),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "no work handed out: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let prompt = assignment["system_prompt"].as_str().expect("system_prompt");

    // The platform's preamble leads, and the agent's own prompt follows it --
    // see `platform_preamble`. Ordering is what matters here, so the prompt is
    // located rather than required to come first.
    let own_at = prompt.find("Be brief.").expect("the agent's own prompt went missing");
    let base_at = prompt.find("Call the v1 endpoint.").expect("base prose missing");
    assert!(own_at < base_at, "the agent's prompt should lead its skills:\n{prompt}");
    let over_at = prompt.find("Our region is on v2.").expect("override prose missing");
    assert!(base_at < over_at, "the override did not come last:\n{prompt}");

    // And the turn knows what it was built from, before it has even run.
    let reply: Uuid = assignment["reply_id"].as_str().expect("reply").parse().unwrap();
    let recorded: Vec<(Uuid, i32)> =
        sqlx::query_as("select skill_id, position from turn_skills where reply_id = $1 order by position")
            .bind(reply)
            .fetch_all(&h.db.pool)
            .await
            .expect("turn_skills");
    assert_eq!(recorded.len(), 2, "the turn did not record both skills: {recorded:?}");
    assert_eq!(
        recorded[0].0.to_string(),
        base_id,
        "the base should be recorded first"
    );
}

/// The templates a workspace's roles are copied from must be ones the code can
/// honour.
///
/// They are rows now, not constants, so nothing about them is checked when this
/// builds. Read raw rather than through the store, which filters: the point is
/// to catch a seed that would come out quietly narrower than it reads.
#[tokio::test]
async fn seeded_role_templates_are_all_honourable() {
    let h = harness_or_skip!();

    let rows: Vec<(String, String)> =
        sqlx::query_as("select template_name, authority from role_template_authorities")
            .fetch_all(&h.db.pool)
            .await
            .expect("template authorities");
    assert!(!rows.is_empty(), "no role templates were seeded");

    for (template, raw) in &rows {
        let parsed = outturn::auth::Authority::parse(raw);
        assert!(parsed.is_some(), "{template} names {raw}, which is not an authority");
        assert!(
            parsed.unwrap().workspace_assignable(),
            "{template} bundles {raw}, which is reserved to the platform"
        );
    }

    // And what the store hands back is what a new workspace actually gets.
    let names: Vec<String> = h.roles.templates().await.expect("templates").into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"admin".to_string()), "no admin template: {names:?}");
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
    let admin = h.login_as("wx@acme.example", None, Some((acme, "admin"))).await;

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
    assert_eq!(skill["hosts"][0], "api.open-meteo.com", "not normalised: {body}");
    assert_eq!(skill["unmet_hosts"][0], "api.open-meteo.com", "should be unmet: {body}");

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
        .post(&format!("/v1/skills/{skill_id}/hosts/approve"), Some(&admin), "")
        .await;
    assert_eq!(status, StatusCode::OK, "approve failed: {body}");
    assert!(body.contains("api.open-meteo.com"), "body: {body}");

    let (status, body) = h
        .send(bind(&admin, format!(r#"[{{"skill_id":"{skill_id}"}}]"#)))
        .await;
    assert_eq!(status, StatusCode::OK, "still refused after approval: {body}");

    // And the rule remembers which skill asked for it.
    let from: Option<Uuid> =
        sqlx::query_scalar("select from_skill_id from egress_rules where host = 'api.open-meteo.com'")
            .fetch_one(&h.db.pool)
            .await
            .expect("rule");
    assert_eq!(from.map(|u| u.to_string()), Some(skill_id), "provenance not recorded");
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
    let operator = h.login_as("op9@acme.example", None, Some((acme, "operator"))).await;

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            r#"{"slug":"weather","name":"Weather","body":"x","hosts":["api.open-meteo.com"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "an operator may write a skill: {body}");
    let skill_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // But not open what it names.
    let (status, body) = h
        .post(&format!("/v1/skills/{skill_id}/hosts/approve"), Some(&operator), "")
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator granted itself network access through a skill: {body}"
    );

    // An admin, who may write the rule by hand, may approve it.
    let admin = h.login_as("ad9@acme.example", None, Some((acme, "admin"))).await;
    let (status, _) = h
        .post(&format!("/v1/skills/{skill_id}/hosts/approve"), Some(&admin), "")
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// A version that adds a host is unvetted again; one that only rewords is not.
#[tokio::test]
async fn only_a_new_host_asks_for_approval_again() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h.login_as("rv@acme.example", None, Some((acme, "admin"))).await;

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
    h.post(&format!("/v1/skills/{id}/hosts/approve"), Some(&admin), "").await;

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
    assert_eq!(unmet.len(), 1, "should ask about the new host alone: {body}");
    assert_eq!(unmet[0], "api.example.com");
}

/// A workspace kill switch stops a turn before it runs.
///
/// The enforcement point is turn preparation, not the API edge: a message is
/// always accepted, and what a hold stops is the agent acting on it. So the
/// message is taken, no work is handed out, and the session is latched with
/// what stopped it -- see docs/inhibitors.md.
#[tokio::test]
async fn a_kill_switch_stops_a_turn_and_latches_the_session() {
    use outturn::api::inhibitor::{InhibitorStore, Scope, Strength, TakeInhibitor};

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let runtime = h.runtime_token(acme);

    // The customer's spend cap trips.
    let inhibitors = outturn::api::inhibitor::PostgresInhibitorStore::new(h.db.pool.clone());
    inhibitors
        .take(TakeInhibitor {
            scope: Scope::Workspace { workspace_id: acme },
            strength: Strength::Stopped,
            reason: "monthly spend cap reached".into(),
            held_by: "billing-bot".into(),
        })
        .await
        .expect("take");

    // The message is still accepted: what a hold stops is the agent, not the
    // person talking to it.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "the hold refused a message");

    // No work: an idle cluster answers 200 with a null body, and a held one
    // looks the same to a runtime -- which is the point. The runtime is not
    // told why, because it is not the tier that decides.
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, "null", "work was handed out while a kill switch was on: {body}");

    // And the session says why, so the next turn does not read an unexplained
    // silence.
    let latched: Option<String> = sqlx::query_scalar(
        "select stopped_reason from agent_sessions where id = $1 and stopped_at is not null",
    )
    .bind(session_id.parse::<Uuid>().unwrap())
    .fetch_optional(&h.db.pool)
    .await
    .expect("query")
    .flatten();
    assert_eq!(latched.as_deref(), Some("monthly spend cap reached"));
}

/// Releasing the hold does not resume anything; a person has to.
///
/// This is what makes it a kill switch rather than a pause with a harsher
/// name. Fifty conversations stopped by one switch need fifty deliberate
/// restarts.
#[tokio::test]
async fn a_released_kill_switch_does_not_resume_by_itself() {
    use outturn::api::inhibitor::{InhibitorStore, Scope, Strength, TakeInhibitor};

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let inhibitors = outturn::api::inhibitor::PostgresInhibitorStore::new(h.db.pool.clone());
    let hold = inhibitors
        .take(TakeInhibitor {
            scope: Scope::Workspace { workspace_id: acme },
            strength: Strength::Stopped,
            reason: "cap reached".into(),
            held_by: "billing-bot".into(),
        })
        .await
        .expect("take");

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let runtime = h.runtime_token(acme);
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(body, "null", "the hold let work through: {body}");

    // The customer tops up.
    inhibitors.release(hold.id).await.expect("release");

    // Nothing resumes on its own: the latch outlives the hold.
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(
        body, "null",
        "a released hold resumed a stopped session by itself: {body}"
    );

    // A person saying something clears it, and that turn runs.
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"are you there?"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "a person could not restart it: {body}");
}

/// Stopping an org and stopping one agent are different powers.
///
/// An operator builds and runs agents, so stopping one is theirs; the
/// workspace switch stops work they may know nothing about, so it is not.
#[tokio::test]
async fn stopping_an_org_needs_more_than_stopping_an_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // An operator may stop an agent.
    let (status, body) = h
        .post(
            &format!("/v1/agents/{agent_id}/stop"),
            Some(&operator),
            r#"{"reason":"looping on the same tool"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "an operator could not stop an agent: {body}");

    // But not the whole workspace.
    let (status, body) = h
        .post("/v1/workspace/stop", Some(&operator), r#"{"reason":"nope"}"#)
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator stopped the whole workspace: {body}"
    );

    // An admin may.
    let (status, body) = h
        .post("/v1/workspace/stop", Some(&admin), r#"{"reason":"spend cap"}"#)
        .await;
    assert_eq!(status, StatusCode::CREATED, "an admin could not stop the workspace: {body}");

    // Both show up, with their reasons, to anyone who can see agents.
    let (status, body) = h.get("/v1/inhibitors", Some(&operator)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let held: Vec<serde_json::Value> = serde_json::from_str(&body).expect("inhibitors");
    assert_eq!(held.len(), 2, "{held:?}");
    let reasons: Vec<&str> = held.iter().map(|i| i["reason"].as_str().unwrap()).collect();
    assert!(reasons.contains(&"looping on the same tool"), "{reasons:?}");
    assert!(reasons.contains(&"spend cap"), "{reasons:?}");

    // Releasing the workspace hold needs the wider authority too.
    let workspace_hold = held
        .iter()
        .find(|i| i["scope"]["level"] == "workspace")
        .expect("workspace hold");
    let id = workspace_hold["id"].as_str().unwrap();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/inhibitors/{id}"))
        .header("authorization", format!("Bearer {operator}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator released the workspace's hold: {body}"
    );
}

/// A hold needs a reason, because the conversation it stops cannot explain
/// itself.
#[tokio::test]
async fn a_hold_without_a_reason_is_refused() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post("/v1/workspace/stop", Some(&admin), r#"{"reason":"   "}"#)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a blank reason was accepted: {body}");
}

/// The gateway's check sees every level, and reports the strongest.
///
/// It holds a turn token naming a workspace and a session but no agent, so the
/// agent level is resolved from the session row. A hold it could not see is a
/// runaway turn that keeps generating past a cap that has already tripped.
#[tokio::test]
async fn the_gateway_sees_a_hold_at_any_level() {
    use outturn::api::inhibitor::{
        InhibitorStore, Scope, Strength, TakeInhibitor, decide, postgres::covering_session,
    };

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let inhibitors = outturn::api::inhibitor::PostgresInhibitorStore::new(h.db.pool.clone());

    // Nothing held: nothing to report.
    assert!(
        covering_session(&h.db.pool, acme, session_id)
            .await
            .expect("query")
            .is_empty()
    );

    // An agent hold, which the gateway can only find through the session.
    inhibitors
        .take(TakeInhibitor {
            scope: Scope::Agent { workspace_id: acme, agent_id },
            strength: Strength::Suspended,
            reason: "waiting on somebody".into(),
            held_by: "tester".into(),
        })
        .await
        .expect("take");
    let decision = decide(covering_session(&h.db.pool, acme, session_id).await.expect("query"));
    assert_eq!(decision.verdict, outturn::api::inhibitor::Verdict::Suspended);

    // A stop anywhere outranks it, and its reason is the one reported.
    inhibitors
        .take(TakeInhibitor {
            scope: Scope::Workspace { workspace_id: acme },
            strength: Strength::Stopped,
            reason: "spend cap reached".into(),
            held_by: "billing-bot".into(),
        })
        .await
        .expect("take");
    let decision = decide(covering_session(&h.db.pool, acme, session_id).await.expect("query"));
    assert_eq!(decision.verdict, outturn::api::inhibitor::Verdict::Stopped);
    // The deciding hold's reason, and only it: the suspended one is not what
    // stopped this, so naming it would send somebody to release the wrong hold.
    assert_eq!(decision.why(), "spend cap reached");
}

/// A hold that cuts a turn latches the session, even if it is released first.
///
/// The gateway cuts the stream and the turn ends, but the hold may be gone by
/// the time anybody sends another message -- and a session that was never
/// latched would simply carry on. A stop is supposed to need a person to lift
/// it, so the latch is written when generation stops rather than a turn later.
#[tokio::test]
async fn a_hold_that_cut_a_turn_latches_the_session_even_once_released() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"tell me a long story"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "no work: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
    let lease = assignment["lease_token"].as_str().expect("lease");

    // The turn reports what a gateway-cut turn reports: a partial reply, and
    // the reason the stream ended. No hold is taken here at all -- this is the
    // case where it was taken and released while the turn was still streaming,
    // so there is nothing left in the table to find.
    let stream = [
        r#"{"kind":"delta","idx":0,"text":"Once upon a"}"#,
        r#"{"kind":"done","content":"Once upon a","prompt_tokens":10,"completion_tokens":3,"held":"runaway turn"}"#,
    ]
    .join("\n");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    // Latched, with the reason, though nothing is holding the workspace now.
    let latched: Option<String> = sqlx::query_scalar(
        "select stopped_reason from agent_sessions where id = $1 and stopped_at is not null",
    )
    .bind(session_id.parse::<Uuid>().unwrap())
    .fetch_optional(&h.db.pool)
    .await
    .expect("query")
    .flatten();
    assert_eq!(
        latched.as_deref(),
        Some("runaway turn"),
        "a turn cut by a hold left the session able to carry on"
    );

    // And the latch is what the next turn actually trips over. The message is
    // accepted -- input is never blocked -- so what proves the refusal is that
    // no work is handed out for it.
    //
    // Posted by the runtime rather than a person, because a person's message
    // clears the latch by design: asserting on one would test the restart, not
    // the refusal.
    let another = Uuid::now_v7();
    sqlx::query(
        "insert into agent_messages (id, session_id, role, content, metadata, delivery) \
         values ($1, $2, 'user', 'go on', '{}'::jsonb, 'steer')",
    )
    .bind(another)
    .bind(session_id.parse::<Uuid>().unwrap())
    .execute(&h.db.pool)
    .await
    .expect("insert");
    outturn::jobs::enqueue(
        &h.db.pool,
        acme,
        "chat.turn",
        serde_json::json!({
            "workspace_id": acme,
            "session_id": session_id.parse::<Uuid>().unwrap(),
            "agent_id": agent_id.parse::<Uuid>().unwrap(),
            "message_id": another,
        }),
        None,
        Some(&format!("session:{session_id}")),
        outturn::jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");

    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body, "null",
        "a latched session handed out work to a message no person sent: {body}"
    );
}

/// A turn a hold cut and that then failed latches too.
///
/// A failure and a stop both end a turn without a reply, and the difference
/// matters: a failure is retried, and a retry that finds the hold released
/// would run the work the hold existed to prevent.
#[tokio::test]
async fn a_held_turn_that_then_failed_still_latches() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"tell me a long story"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
    let lease = assignment["lease_token"].as_str().expect("lease");

    // The stream was cut by a hold, and then the guest fell over while tidying
    // up -- so what the runtime reports is a failure carrying the reason.
    let stream =
        r#"{"kind":"failed","message":"guest trapped","held":"runaway turn"}"#;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, _) = h.send(req).await;
    // The turn failed, so the report is not a success -- but the latch is
    // written regardless, which is the point.
    assert_ne!(status, StatusCode::OK, "a failed turn reported as succeeding");

    let latched: Option<String> = sqlx::query_scalar(
        "select stopped_reason from agent_sessions where id = $1 and stopped_at is not null",
    )
    .bind(session_id.parse::<Uuid>().unwrap())
    .fetch_optional(&h.db.pool)
    .await
    .expect("query")
    .flatten();
    assert_eq!(
        latched.as_deref(),
        Some("runaway turn"),
        "a held turn that failed left the session able to carry on"
    );
}

/// An ordinary turn leaves the session alone.
#[tokio::test]
async fn a_turn_that_finished_normally_does_not_latch() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","system_prompt":"Be brief."}"#,
        )
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#),
        )
        .await;
    let session_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let runtime = h.runtime_token(acme);
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
    let lease = assignment["lease_token"].as_str().expect("lease");

    let stream = r#"{"kind":"done","content":"Hi","prompt_tokens":10,"completion_tokens":1}"#;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let stopped: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select stopped_at from agent_sessions where id = $1")
            .bind(session_id.parse::<Uuid>().unwrap())
            .fetch_one(&h.db.pool)
            .await
            .expect("query");
    assert!(stopped.is_none(), "an ordinary turn latched the session");
}
