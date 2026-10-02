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
use outturn::api::agent::{AgentStore, PostgresAgentStore};
use outturn::api::chat::{ChatStore, PostgresChatStore};
use outturn::api::session::{PostgresSessionStore, SessionStore};
use outturn::api::user::{CreateUser, PostgresUserStore, UserStore};
use outturn::api::workspace::{PostgresWorkspaceStore, WorkspaceStore};
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
    gateway_validator: TokenValidator,
    roles: Arc<dyn outturn::api::role::RoleStore>,
    /// The same instance the app holds. A second store would have a second
    /// cache, and without the listener a test runs without, writing through
    /// one would leave the other answering from before the write.
    scopes: Arc<dyn outturn::api::scope::ScopeStore>,
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
        SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes()
            .to_vec()
    };
    let public_key: [u8; 32] = public_bytes.try_into().expect("32-byte key");
    let validator =
        TokenValidator::new(&public_key, outturn::auth::AUDIENCE_API).expect("validator");
    // What the gateway would check a turn token with, for tests that need to
    // read one the API minted.
    let gateway_validator = TokenValidator::new(&public_key, outturn::auth::AUDIENCE_GATEWAY)
        .expect("gateway validator");
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
    let settings: Arc<dyn outturn::api::settings::SettingsStore> = Arc::new(
        outturn::api::settings::PostgresSettingsStore::new(pool.clone()),
    );
    let skills: Arc<dyn outturn::api::skill::SkillStore> =
        Arc::new(outturn::api::skill::PostgresSkillStore::new(pool.clone()));
    // No invalidation listener: one per test would hold a connection each
    // against a server the whole suite shares, and a test writing through this
    // same instance clears the cache the app reads without needing one.
    let scopes: Arc<dyn outturn::api::scope::ScopeStore> =
        Arc::new(outturn::api::scope::PostgresScopeStore::new(pool.clone()));

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
        scopes.clone(),
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
        inhibitors: Arc::new(outturn::api::inhibitor::PostgresInhibitorStore::new(
            pool.clone(),
        )),
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
        gateway_validator,
        roles,
        scopes,
    }
}

macro_rules! harness_or_skip {
    () => {
        harness().await
    };
}

/// The rows of one page of a list endpoint, which answers `{items, next}`.
///
/// Fails on a bare array rather than accepting either, so a list endpoint that
/// quietly stopped paging is noticed here.
fn items(body: &str) -> Vec<Value> {
    let page: Value = serde_json::from_str(body).expect("a page of json");
    page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("not a page: {body}"))
        .clone()
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
            // Where a system administrator with no workspace role lands. Named
            // rather than taken as the first row listed, which it was until the
            // list stopped offering it.
            None => outturn::api::usage::PLATFORM_WORKSPACE,
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

    /// A session with one message in it, so the feed has something to carry.
    async fn session_with_a_message(&self, token: &str, agent: Uuid, text: &str) -> Uuid {
        let (_, body) = self
            .post(
                "/v1/agent-sessions",
                Some(token),
                &format!(r#"{{"agent_id":"{agent}","title":"t"}}"#),
            )
            .await;
        let id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        self.post(
            &format!("/v1/agent-sessions/{id}/messages"),
            Some(token),
            &serde_json::json!({ "content": text }).to_string(),
        )
        .await;
        id
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
        .login_as(
            "admin@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (status, body) = h.get("/v1/workspaces", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.contains("Acme") && body.contains("Globex"),
        "body: {body}"
    );

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
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
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
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
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
    assert!(
        dup.is_err(),
        "duplicate slug within a workspace must be refused"
    );

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
    assert!(
        body.contains("keep this too"),
        "prompt must survive: {body}"
    );

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
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h.post("/v1/work", Some(&sysadmin), "{}").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a system admin took work: {body}"
    );

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
    assert_eq!(
        body, "null",
        "an idle cluster should answer null, got {body}"
    );
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
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
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
    let job: Uuid = assignment["job_id"]
        .as_str()
        .expect("job")
        .parse()
        .expect("uuid");
    let lease = assignment["lease_token"]
        .as_str()
        .expect("lease")
        .to_string();
    let reply: Uuid = assignment["reply_id"]
        .as_str()
        .expect("reply")
        .parse()
        .expect("uuid");

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
    assert_eq!(
        taken.len(),
        1,
        "the steer should have been handed over once"
    );

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
    assert_eq!(
        retry["job_id"], assignment["job_id"],
        "the retry should be the same job"
    );

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
        .body(Body::from(
            r#"{"authorities":["agents:read","agents:create","sessions:read"]}"#,
        ))
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
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a workspace role took workspaces:create: {body}"
    );
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"taker","authorities":["work:take"]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a workspace role took work:take: {body}"
    );

    // A platform role's name cannot be reused.
    let (status, _) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"system_admin","authorities":[]}"#,
        )
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
    let roles = items(&body);
    let admin_role = roles
        .iter()
        .find(|r| r["name"] == "admin")
        .expect("the default admin role");
    assert_eq!(admin_role["holders"], 1);

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/v1/roles/{}",
            admin_role["id"].as_str().expect("id")
        ))
        .header("authorization", format!("Bearer {admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a held role was deleted: {body}"
    );

    let (status, body) = h
        .post(
            "/v1/users",
            Some(&admin),
            r#"{"email":"x@acme.example","display_name":"X","password":"correct-horse","role":"wizard"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a role that does not exist was granted: {body}"
    );

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
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":"","account":"shipper-northvale"}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    assert_eq!(
        session["account"], "shipper-northvale",
        "the account was not stored: {body}"
    );
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
    assert_eq!(first["account"], "shipper-northvale");
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
        page["entries"][0]["provider_usage"]["cache_creation"]["ephemeral_1h_input_tokens"], 7,
        "the raw usage object was not kept: {body}"
    );
    let next = page["next"].as_str().expect("cursor");

    let (_, body) = h
        .get(&format!("/v1/usage?limit=1&after={next}"), Some(&admin))
        .await;
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    assert!(
        page["next"].is_null(),
        "the ledger should be exhausted: {body}"
    );

    // Another workspace's ledger is not this admin's to read.
    let globex = h.make_workspace("Globex", "globex").await;
    let (status, _) = h
        .get(&format!("/v1/usage?workspace_id={globex}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The summary is a reading of the same rows: what the export pages, these
    // figures add up.
    let (status, body) = h.get("/v1/usage/summary", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let summary: serde_json::Value = serde_json::from_str(&body).expect("summary");
    assert_eq!(
        summary["totals"]["calls"], 2,
        "both rounds should be counted: {body}"
    );
    assert_eq!(summary["totals"]["prompt_tokens"], 30);
    assert_eq!(summary["totals"]["completion_tokens"], 7);
    assert_eq!(summary["totals"]["sessions"], 1);

    // Every day of the window is present, including the ones nothing happened
    // on -- a chart drawn from a series that skips them draws an idle day as no
    // day at all.
    let daily = summary["daily"].as_array().expect("daily");
    assert_eq!(
        daily.len(),
        30,
        "the default window is 30 days of buckets: {body}"
    );
    // The last bucket is today's, whole rather than cut off at the moment of
    // the call, which is why the window ends at the next midnight.
    assert_eq!(
        daily.last().expect("a bucket")["calls"],
        2,
        "the turn just recorded belongs in the last bucket: {body}"
    );
    let charged: i64 = daily
        .iter()
        .map(|d| d["prompt_tokens"].as_i64().expect("tokens"))
        .sum();
    assert_eq!(
        charged, 30,
        "the daily series must add up to the total above it: {body}"
    );

    // Every bucket opens at midnight UTC, whatever timezone the database
    // session is in. `date_trunc` in its two-argument form reads the session's
    // TimeZone, which nothing in this path sets, so a deployment whose server
    // or pooler defaults elsewhere would shift every boundary -- and the
    // browser, which labels buckets in UTC, would print the wrong day against
    // the right figures. The explicit zone in the query is what pins this.
    for day in daily {
        let at = day["at"].as_str().expect("a bucket opens somewhere");
        assert!(
            at.ends_with("T00:00:00Z"),
            "a bucket opened at {at} rather than at midnight UTC: {body}"
        );
    }

    // The slices carry names, not just ids, and the account label the session
    // was opened with.
    let by_model = summary["by_model"].as_array().expect("by_model");
    assert_eq!(by_model.len(), 2, "two models answered: {body}");
    let models: Vec<&str> = by_model
        .iter()
        .map(|m| m["key"].as_str().expect("key"))
        .collect();
    assert!(
        models.contains(&"qwen") && models.contains(&"claude-sonnet-5"),
        "{body}"
    );
    assert_eq!(
        summary["by_account"][0]["key"], "shipper-northvale",
        "{body}"
    );
    assert_eq!(
        summary["by_workspace"][0]["label"], "Acme",
        "the workspace was not named: {body}"
    );

    // Whose spend it was. A turn's own rounds bill to the agent that ran them,
    // and both carry the session's account label -- which is what a workspace
    // joins its bill to its own records by, so a row missing it is spend they
    // cannot attribute to anybody.
    let (_, body) = h.get("/v1/usage?limit=100", Some(&admin)).await;
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    for entry in page["entries"].as_array().expect("entries") {
        assert_eq!(
            entry["account"], "shipper-northvale",
            "a row the session produced lost its account label: {entry}"
        );
        // Every row a session produced bills to that session's agent, whoever
        // started the call. A session cannot exist without one, so a null here
        // is spend nobody can explain rather than spend nobody owns -- and the
        // platform's own work (naming, compaction) is still done for one
        // agent's conversation.
        assert!(
            !entry["agent_id"].is_null(),
            "a row a session produced was left unattributed: {entry}"
        );
    }

    // Scope is what widens a summary, and it is not this admin's to ask for.
    let (status, _) = h.get("/v1/usage/summary?scope=all", Some(&admin)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a workspace administrator must not see every workspace"
    );

    // The operator may. Globex has no rows, so the platform total is still Acme's.
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h.get("/v1/usage/summary?scope=all", Some(&root)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let platform: serde_json::Value = serde_json::from_str(&body).expect("summary");
    assert_eq!(platform["totals"]["calls"], 2, "body: {body}");

    // A window that does not close is refused rather than guessed at.
    let (status, _) = h
        .get(
            "/v1/usage/summary?from=2026-02-01T00:00:00Z&to=2026-01-01T00:00:00Z",
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

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
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    // Nothing set anywhere: the catalogue default applies.
    let (status, body) = h.get("/v1/settings", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
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
        .send(req(
            "PUT",
            "/v1/platform/settings/reasoning_effort".into(),
            &root,
            r#"{"value":"low"}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // A workspace admin may not.
    let (status, _) = h
        .send(req(
            "PUT",
            "/v1/platform/settings/reasoning_effort".into(),
            &admin,
            r#"{"value":"high"}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The workspace now inherits it.
    let (_, body) = h.get("/v1/settings", Some(&admin)).await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
    assert_eq!(effort["value"], "low");
    assert_eq!(effort["source"], "operator");
    assert!(
        effort["override_value"].is_null(),
        "no row at the workspace level yet"
    );

    // The workspace overrides; an agent inherits the workspace's value.
    let (status, _) = h
        .send(req(
            "PUT",
            "/v1/settings/reasoning_effort".into(),
            &admin,
            r#"{"value":"high"}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");
    let (_, body) = h
        .get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin))
        .await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
    assert_eq!(effort["value"], "high");
    assert_eq!(effort["source"], "workspace");
    assert_eq!(effort["inherited"], "high");

    // Bad values are refused by the catalogue, not stored.
    let (status, body) = h
        .send(req(
            "PUT",
            format!("/v1/agents/{agent_id}/settings/temperature"),
            &admin,
            r#"{"value":9}"#,
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // Clearing the workspace's override falls back to the operator's value.
    let (status, _) = h
        .send(req(
            "DELETE",
            "/v1/settings/reasoning_effort".into(),
            &admin,
            "",
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h
        .get(&format!("/v1/agents/{agent_id}/settings"), Some(&admin))
        .await;
    let view: Vec<serde_json::Value> = serde_json::from_str(&body).expect("view");
    let effort = view
        .iter()
        .find(|s| s["key"] == "reasoning_effort")
        .expect("effort");
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
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
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
    assert!(
        body.contains(r#""path":"session/report.txt""#),
        "body: {body}"
    );

    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session_id}/files"),
            Some(&admin),
        )
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
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a viewer wrote workspace files: {body}"
    );
    let (status, body) = h.send(put(&viewer, "session/mine.txt", "x")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a viewer wrote session files without sessions:create: {body}"
    );
    let (status, body) = h.send(put(&admin, "workspace/pricing.csv", "x")).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // Traversal in an upload path is refused, not repaired.
    let (status, _) = h
        .send(put(&admin, "session/../workspace/oops.txt", "x"))
        .await;
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
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a wrong key was accepted: {body}"
    );

    let turn = h
        .minter
        .mint_turn(
            Uuid::now_v7(),
            acme,
            outturn::egress::commit::empty_root(),
            outturn::egress::gate::Gates::none().root(acme),
        )
        .expect("token");
    let (status, body) = h.post("/v1/work", Some(&turn), "{}").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a turn token opened the work endpoint: {body}"
    );

    let (status, body) = h.post("/v1/work", Some(&h.runtime_token(acme)), "{}").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the runtime key was refused: {body}"
    );
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
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Plain","slug":"plain"}"#,
        )
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
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Call the v1 endpoint."}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "operator could not ship a skill: {body}"
    );
    let shipped: serde_json::Value = serde_json::from_str(&body).expect("json");
    let skill_id = shipped["id"].as_str().expect("id").to_string();

    // A workspace admin, who is not the operator.
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h.get("/v1/skills", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("\"crm\""),
        "the operator's skill was not visible: {body}"
    );

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
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a workspace edited the operator's skill"
    );

    // And the platform route is closed to them.
    let (status, _) = h
        .post(
            "/v1/platform/skills",
            Some(&admin),
            r#"{"slug":"sneaky","name":"Sneaky","body":"x"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a workspace shipped a platform skill"
    );
}

/// A version read back lists the hosts it declared, not an empty list.
#[tokio::test]
async fn a_version_read_back_names_its_hosts() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"inn","name":"Inn","body":"x","hosts":["api.inn.example"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = skill["id"].as_str().unwrap();
    let v = skill["version_id"].as_str().unwrap();

    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions"), Some(&admin))
        .await;
    let list = items(&body);
    assert_eq!(list[0]["hosts"][0], "api.inn.example", "{body}");
    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions/{v}"), Some(&admin))
        .await;
    let one: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(one["hosts"][0], "api.inn.example", "{body}");
}

/// A version's files are part of it: listed with it, readable as published, and
/// unchanged by later versions.
#[tokio::test]
async fn a_skill_versions_its_files_with_its_body() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"inn","name":"Inn","body":"Read inn/book.md first.",
                "files":[{"path":"book.md","content":"POST /bookings, v1"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "create: {body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = skill["id"].as_str().unwrap().to_string();
    let v1 = skill["version_id"].as_str().unwrap().to_string();

    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions/{v1}"), Some(&admin))
        .await;
    let version: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(version["files"][0]["path"], "book.md", "{body}");

    // A body edit that says nothing about files keeps them.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"Read book.md before booking."}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body edit: {body}");
    let v2: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v2["files"][0]["path"], "book.md",
        "files were dropped: {body}"
    );

    // Changing a file appends a version, and the old one still reads as it was.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"Read book.md before booking.",
                "files":[{"path":"book.md","content":"POST /bookings, v2"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "file edit: {body}");
    let v3: serde_json::Value = serde_json::from_str(&body).unwrap();
    let v3_id = v3["id"].as_str().unwrap().to_string();

    let (status, body) = h
        .get(
            &format!("/v1/skills/{id}/versions/{v1}/files/book.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "POST /bookings, v1");
    let (_, body) = h
        .get(
            &format!("/v1/skills/{id}/versions/{v3_id}/files/book.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(body, "POST /bookings, v2");

    // Sending the live version again appends nothing.
    let (status, body) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"Read book.md before booking.",
                "files":[{"path":"book.md","content":"POST /bookings, v2"}]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an identical version was appended: {body}"
    );
    let same: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(same["id"], v3["id"]);

    // A path that climbs is refused rather than cleaned up.
    let (status, _) = h
        .post(
            &format!("/v1/skills/{id}/versions"),
            Some(&admin),
            r#"{"body":"x","files":[{"path":"../other/x.md","content":"x"}]}"#,
        )
        .await;
    assert!(
        status.is_client_error(),
        "a climbing path was accepted: {status}"
    );
}

/// An operator's skill reaches a workspace with its files, including through a
/// fork, and a workspace's override cannot carry files of its own.
#[tokio::test]
async fn an_operators_skill_files_reach_the_workspace() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op3@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"See crm/call.md.",
                "files":[{"path":"call.md","content":"GET /v1/accounts"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let base: serde_json::Value = serde_json::from_str(&body).unwrap();
    let base_id = base["id"].as_str().unwrap().to_string();
    let base_v = base["version_id"].as_str().unwrap().to_string();

    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, body) = h
        .get(
            &format!("/v1/skills/{base_id}/versions/{base_v}/files/call.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "GET /v1/accounts");

    let (status, body) = h
        .post(
            &format!("/v1/skills/{base_id}/fork"),
            Some(&admin),
            r#"{"slug":"crm-mine","name":"CRM (mine)"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "fork: {body}");
    let fork: serde_json::Value = serde_json::from_str(&body).unwrap();
    let fork_id = fork["id"].as_str().unwrap().to_string();
    let fork_v = fork["version_id"].as_str().unwrap().to_string();
    let (status, body) = h
        .get(
            &format!("/v1/skills/{fork_id}/versions/{fork_v}/files/call.md"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the fork lost its file: {body}");
    assert_eq!(body, "GET /v1/accounts");

    let (status, _) = h
        .post(
            "/v1/skills",
            Some(&admin),
            &format!(
                r#"{{"slug":"crm-ours","name":"Ours","body":"v2 here.","base_skill_id":"{base_id}",
                    "files":[{{"path":"call.md","content":"GET /v2/accounts"}}]}}"#
            ),
        )
        .await;
    assert!(
        status.is_client_error(),
        "an override carried files: {status}"
    );
}

/// An override composes after the prose it speaks about, and applies wherever
/// its base is used without being bound itself.
#[tokio::test]
async fn an_override_composes_after_its_base_and_needs_no_binding() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op2@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
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
        .post(
            "/v1/agents",
            Some(&operator),
            r#"{"name":"Helper","slug":"helper"}"#,
        )
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

    assert_eq!(
        composed.len(),
        2,
        "expected base and override: {composed:?}"
    );
    assert_eq!(composed[0].body, "Call the v1 endpoint.");
    assert_eq!(
        composed[1].body, "Our region is on v2.",
        "the override must come last"
    );
}

/// An override is not a thing an agent is given on its own.
#[tokio::test]
async fn an_override_cannot_be_bound_by_itself() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op3@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&operator),
            r#"{"slug":"base","name":"Base","body":"b"}"#,
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
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a second override was allowed: {body}"
    );

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
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an override was bound directly: {body}"
    );
}

/// When the operator edits a skill, an override written against the old version
/// is reported as stale rather than left to rot quietly.
#[tokio::test]
async fn editing_a_base_marks_the_overrides_written_against_it() {
    let h = harness().await;
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op4@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
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
            &format!(
                r#"{{"slug":"ours","name":"Ours","body":"stay on v1","base_skill_id":"{base_id}"}}"#
            ),
        )
        .await;
    let override_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (_, body) = h
        .get(&format!("/v1/skills/{override_id}"), Some(&operator))
        .await;
    let before: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        before["base_moved"], false,
        "fresh override reported as stale"
    );

    // The operator ships a new version of the base.
    let (status, body) = h
        .post(
            &format!("/v1/platform/skills/{base_id}/versions"),
            Some(&operator),
            r#"{"body":"v2 endpoint","note":"v2 migration"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "publish refused: {body}");

    let (_, body) = h
        .get(&format!("/v1/skills/{override_id}"), Some(&operator))
        .await;
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
    let admin = h
        .login_as("a@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/skills",
            Some(&admin),
            r#"{"slug":"s","name":"S","body":"one"}"#,
        )
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

    let (_, body) = h
        .get(&format!("/v1/skills/{id}/versions"), Some(&admin))
        .await;
    let versions = items(&body);
    assert_eq!(versions.len(), 3, "a rollback lost history: {body}");
    assert_eq!(versions[0]["ordinal"], 3, "newest first");
    assert_eq!(
        versions[0]["body"], "one",
        "the rollback did not carry the old body"
    );

    let (_, body) = h.get(&format!("/v1/skills/{id}"), Some(&admin)).await;
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        skill["ordinal"], 3,
        "the live version is the newest, not the oldest"
    );
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
        .login_as(
            "op5@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
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
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
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
    let own_at = prompt
        .find("Be brief.")
        .expect("the agent's own prompt went missing");
    let base_at = prompt
        .find("Call the v1 endpoint.")
        .expect("base prose missing");
    assert!(
        own_at < base_at,
        "the agent's prompt should lead its skills:\n{prompt}"
    );
    let over_at = prompt
        .find("Our region is on v2.")
        .expect("override prose missing");
    assert!(
        base_at < over_at,
        "the override did not come last:\n{prompt}"
    );

    // And the turn knows what it was built from, before it has even run.
    let reply: Uuid = assignment["reply_id"]
        .as_str()
        .expect("reply")
        .parse()
        .unwrap();
    let recorded: Vec<(Uuid, i32)> = sqlx::query_as(
        "select skill_id, position from turn_skills where reply_id = $1 order by position",
    )
    .bind(reply)
    .fetch_all(&h.db.pool)
    .await
    .expect("turn_skills");
    assert_eq!(
        recorded.len(),
        2,
        "the turn did not record both skills: {recorded:?}"
    );
    assert_eq!(
        recorded[0].0.to_string(),
        base_id,
        "the base should be recorded first"
    );
}

/// A turn is handed the files of the version its agent is bound to -- the
/// pinned one, not the live one -- under the workspace that owns them.
#[tokio::test]
async fn a_turn_is_handed_its_pinned_skill_files() {
    use sha2::Digest;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op5@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (status, body) = h
        .post(
            "/v1/platform/skills",
            Some(&operator),
            r#"{"slug":"crm","name":"CRM","body":"Read skill/crm/call.md.",
                "files":[{"path":"call.md","content":"GET /v1/accounts"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: serde_json::Value = serde_json::from_str(&body).unwrap();
    let skill_id = skill["id"].as_str().unwrap().to_string();
    let v1 = skill["version_id"].as_str().unwrap().to_string();
    let (status, body) = h
        .post(
            &format!("/v1/platform/skills/{skill_id}/versions"),
            Some(&operator),
            r#"{"body":"Read skill/crm/call.md.",
                "files":[{"path":"call.md","content":"GET /v2/accounts"}]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&operator),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
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
        .body(Body::from(format!(
            r#"[{{"skill_id":"{skill_id}","version_id":"{v1}"}}]"#
        )))
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
    let files = assignment["skill_files"].as_array().expect("skill_files");
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0]["path"], "skill/crm/call.md");
    assert_eq!(
        files[0]["sha256"],
        hex::encode(sha2::Sha256::digest(b"GET /v1/accounts")),
        "the turn was handed the live version's file, not the pinned one"
    );
    assert_eq!(
        files[0]["workspace_id"],
        outturn::api::usage::PLATFORM_WORKSPACE.to_string(),
        "an operator's file lives under the operator's prefix"
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
        assert!(
            parsed.is_some(),
            "{template} names {raw}, which is not an authority"
        );
        assert!(
            parsed.unwrap().workspace_assignable(),
            "{template} bundles {raw}, which is reserved to the platform"
        );
    }

    // And what the store hands back is what a new workspace actually gets.
    let names: Vec<String> = h
        .roles
        .templates()
        .await
        .expect("templates")
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"admin".to_string()),
        "no admin template: {names:?}"
    );
}

/// The seeded roles are a ladder: each rung holds everything the one below it
/// does, and more.
///
/// Not a matter of taste. A role that is not a superset of the one beneath it
/// makes "promote this person" a question nobody can answer from the names --
/// and the way it went wrong is instructive: `viewer` was seeded with
/// `settings:read` and `operator` was not, so the person who builds agents
/// could not read the settings those agents inherit while the person who only
/// looks could. It survived until somebody signed in as an operator and found
/// a page they had been invited to open and could not read.
#[tokio::test]
async fn the_seeded_roles_are_a_ladder() {
    let h = harness_or_skip!();

    let rows: Vec<(String, String)> =
        sqlx::query_as("select template_name, authority from role_template_authorities")
            .fetch_all(&h.db.pool)
            .await
            .expect("template authorities");

    let held = |name: &str| -> std::collections::BTreeSet<String> {
        rows.iter()
            .filter(|(template, _)| template == name)
            .map(|(_, authority)| authority.clone())
            .collect()
    };

    let viewer = held("viewer");
    let operator = held("operator");
    let admin = held("admin");
    assert!(!viewer.is_empty(), "no viewer template was seeded");

    let below_not_above = |lower: &std::collections::BTreeSet<String>,
                           upper: &std::collections::BTreeSet<String>| {
        lower.difference(upper).cloned().collect::<Vec<_>>()
    };

    assert!(
        below_not_above(&viewer, &operator).is_empty(),
        "viewer holds what operator does not: {:?}",
        below_not_above(&viewer, &operator)
    );
    assert!(
        below_not_above(&operator, &admin).is_empty(),
        "operator holds what admin does not: {:?}",
        below_not_above(&operator, &admin)
    );

    // And each rung is strictly higher, or two of them are the same role
    // wearing different names.
    assert!(
        operator.len() > viewer.len(),
        "operator adds nothing to viewer"
    );
    assert!(
        admin.len() > operator.len(),
        "admin adds nothing to operator"
    );
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
        "not normalised: {body}"
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
    assert_eq!(
        body, "null",
        "work was handed out while a kill switch was on: {body}"
    );

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

/// A held turn says so, as a hold rather than as a failure.
///
/// The refusal happens before a placeholder exists, so with no event the
/// message sits in the transcript with no reply and no indication, and the
/// thread view cannot read `/v1/inhibitors` to work out why. `chat.held`
/// rather than `chat.error`: a failure is retried and a stop is not, and the
/// browser's error path files the turn under failures and discards the reply
/// bubble -- a reader shown that for a deliberate pause is told the system
/// broke. See docs/inhibitors.md.
#[tokio::test]
async fn a_held_turn_is_announced_as_held_rather_than_as_a_failure() {
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
    let agent_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

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

    let session_id = h.session_with_a_message(&admin, agent_id, "hello").await;

    // The runtime is offered the work and finds none, which is where the
    // refusal happens.
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from events where session_id = $1 order by id")
            .bind(session_id)
            .fetch_all(&h.db.pool)
            .await
            .expect("events");

    assert!(
        kinds.iter().any(|k| k == "chat.held"),
        "a held turn was never announced: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == "chat.error"),
        "a deliberate hold was announced as a failure: {kinds:?}"
    );

    // And it carries the latch's own reason, not merely "stopped".
    let held: serde_json::Value = sqlx::query_scalar(
        "select payload from events where session_id = $1 and kind = 'chat.held' order by id limit 1",
    )
    .bind(session_id)
    .fetch_one(&h.db.pool)
    .await
    .expect("held event");
    assert!(
        held["message"].as_str().unwrap_or("").contains("spend cap"),
        "the hold did not say why: {held}"
    );
    assert_eq!(
        held["resumable"],
        serde_json::json!(false),
        "a stop was announced as resuming by itself: {held}"
    );
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
    assert_eq!(
        status,
        StatusCode::OK,
        "a person could not restart it: {body}"
    );
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
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an operator could not stop an agent: {body}"
    );

    // But not the whole workspace.
    let (status, body) = h
        .post(
            "/v1/workspace/stop",
            Some(&operator),
            r#"{"reason":"nope"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator stopped the whole workspace: {body}"
    );

    // An admin may.
    let (status, body) = h
        .post(
            "/v1/workspace/stop",
            Some(&admin),
            r#"{"reason":"spend cap"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an admin could not stop the workspace: {body}"
    );

    // Both show up, with their reasons, to anyone who can see agents.
    let (status, body) = h.get("/v1/inhibitors", Some(&operator)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let held = items(&body);
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
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a blank reason was accepted: {body}"
    );
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
            scope: Scope::Agent {
                workspace_id: acme,
                agent_id,
            },
            strength: Strength::Suspended,
            reason: "waiting on somebody".into(),
            held_by: "tester".into(),
        })
        .await
        .expect("take");
    let decision = decide(
        covering_session(&h.db.pool, acme, session_id)
            .await
            .expect("query"),
    );
    assert_eq!(
        decision.verdict,
        outturn::api::inhibitor::Verdict::Suspended
    );

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
    let decision = decide(
        covering_session(&h.db.pool, acme, session_id)
            .await
            .expect("query"),
    );
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
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
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
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
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
    let stream = r#"{"kind":"failed","message":"guest trapped","held":"runaway turn"}"#;
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
    assert_ne!(
        status,
        StatusCode::OK,
        "a failed turn reported as succeeding"
    );

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
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"},"system_prompt":"Be brief."}"#,
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

/// A person narrowed to one agent cannot start a conversation with another.
///
/// The roster stays workspace-public -- an administrator has to see what is
/// running -- so what a scope narrows is what an agent has done, and who may
/// talk to it. See docs/authorities.md.
#[tokio::test]
async fn a_narrowed_person_reaches_only_the_agents_they_were_given() {
    use outturn::api::scope::ScopeStore;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["accounting", "support"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&admin),
                &format!(r#"{{"name":"{slug}","slug":"{slug}","system_prompt":"x"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (accounting, support) = (ids[0], ids[1]);

    // Before anybody narrows them, both are reachable: absence of a scope is
    // the authority behaving as it always did.
    for agent in [accounting, support] {
        let (status, body) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &format!(r#"{{"agent_id":"{agent}","title":"t"}}"#),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "unnarrowed access refused: {body}"
        );
    }

    // Narrowed to accounting.
    let scopes = &h.scopes;
    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("admin@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    scopes
        .set(acme, me, &[accounting])
        .await
        .expect("set scope");

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{accounting}","title":"t"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the named agent was refused: {body}"
    );

    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(r#"{{"agent_id":"{support}","title":"t"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an agent nobody granted was still reachable: {body}"
    );

    // The roster is not narrowed: an administrator still sees what runs here.
    let (status, body) = h.get("/v1/agents", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let agents = items(&body);
    assert_eq!(agents.len(), 2, "the roster was narrowed too: {body}");

    // And removing the narrowing puts everything back. The notification that
    // drops the cache travels through Postgres, so a moment passes between the
    // write and every pod knowing -- including this one. Polled rather than
    // slept through: what is asserted is that it arrives, not how long it takes.
    scopes.set(acme, me, &[]).await.expect("clear scope");
    let mut restored = false;
    for _ in 0..50 {
        let (status, _) = h
            .post(
                "/v1/agent-sessions",
                Some(&admin),
                &format!(r#"{{"agent_id":"{support}","title":"t"}}"#),
            )
            .await;
        if status == StatusCode::CREATED {
            restored = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        restored,
        "clearing the scope never restored access within a second"
    );
}

/// An agent's files are narrowed with its conversations, and your own are
/// still yours.
///
/// A transcript says what was said; an agent's files are what somebody
/// uploaded. Both are what an agent has done, so a scope that hides one has to
/// hide the other -- and the session a person started themselves is theirs to
/// read whoever else may not. See docs/authorities.md.
#[tokio::test]
async fn an_agents_files_are_narrowed_with_it_but_your_own_stay_yours() {
    use outturn::api::scope::ScopeStore;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Two sessions: one the operator started, one the admin did.
    let body = format!(r#"{{"agent_id":"{agent_id}","title":"t"}}"#);
    let (_, made) = h.post("/v1/agent-sessions", Some(&admin), &body).await;
    let theirs = serde_json::from_str::<serde_json::Value>(&made).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, made) = h.post("/v1/agent-sessions", Some(&operator), &body).await;
    let mine = serde_json::from_str::<serde_json::Value>(&made).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let put = |token: &str, session: &str, bytes: &str| {
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/v1/agent-sessions/{session}/files/session/note.txt"
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes.to_string()))
            .expect("request")
    };
    let (status, _) = h.send(put(&admin, &theirs, "theirs")).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = h.send(put(&operator, &mine, "mine")).await;
    assert_eq!(status, StatusCode::CREATED);

    // Narrow the operator to an agent that is not this one.
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"B","slug":"b"}"#)
        .await;
    let other: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let op_id: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("op@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the operator's account");
    h.scopes
        .set(acme, op_id, &[other])
        .await
        .expect("set scope");

    // Somebody else's session with the narrowed-away agent is closed to them.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{theirs}/files/session/note.txt"),
            Some(&operator),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a narrowed person read another session's files: {body}"
    );

    // Their own is not. They put it there.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{mine}/files/session/note.txt"),
            Some(&operator),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "their own files were withheld: {body}"
    );
    assert_eq!(body, "mine");

    // And the admin, narrowed by nobody, still reads both.
    for session in [&theirs, &mine] {
        let (status, body) = h
            .get(
                &format!("/v1/agent-sessions/{session}/files/session/note.txt"),
                Some(&admin),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an unnarrowed reader was refused: {body}"
        );
    }
}

/// Narrowing somebody is an act of administering people, so it needs the
/// authority that grants roles -- and only inside your own workspace.
#[tokio::test]
async fn setting_a_scope_needs_the_authority_that_assigns_roles() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let op_id: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("op@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the operator's account");

    let put = |token: &str, user: Uuid, body: String| {
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/scopes/{user}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("request")
    };

    // An operator builds agents; deciding who may reach them is not theirs.
    let (status, body) = h
        .send(put(
            &operator,
            op_id,
            format!(r#"{{"agents":["{agent_id}"]}}"#),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an operator narrowed somebody: {body}"
    );

    let (status, body) = h
        .send(put(
            &admin,
            op_id,
            format!(r#"{{"agents":["{agent_id}"]}}"#),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an admin could not narrow: {body}"
    );

    // It reads back, and only the narrowed are listed.
    let (status, body) = h.get("/v1/scopes", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).expect("scopes");
    assert_eq!(rows.len(), 1, "the unnarrowed were listed too: {body}");
    assert_eq!(rows[0]["user_id"], op_id.to_string());
    assert_eq!(rows[0]["agents"][0], agent_id);

    // Somebody outside the workspace cannot be narrowed into it.
    let stranger = Uuid::now_v7();
    let (status, body) = h
        .send(put(
            &admin,
            stranger,
            format!(r#"{{"agents":["{agent_id}"]}}"#),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger was given a scope here: {body}"
    );

    // An agent in no workspace of theirs is refused rather than stored.
    let (status, body) = h
        .send(put(
            &admin,
            op_id,
            format!(r#"{{"agents":["{}"]}}"#, Uuid::now_v7()),
        ))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a scope named an agent that does not exist: {body}"
    );

    // And clearing it empties the listing.
    let (status, _) = h
        .send(put(&admin, op_id, r#"{"agents":[]}"#.to_string()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = h.get("/v1/scopes", Some(&admin)).await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).expect("scopes");
    assert!(rows.is_empty(), "a cleared scope was still listed: {body}");
}

/// Narrowing holds on writes, not only on reads.
///
/// `scope::is_narrowed` names all four session authorities, and the module doc
/// says a write is narrowed wherever its read is -- but only the reads went
/// through `require_for_agent`, so somebody narrowed to one agent could delete,
/// rename, post into and stop another agent's conversations. Session ids travel
/// in URLs, so "they would have to guess the id" was never the guarantee.
#[tokio::test]
async fn a_narrowed_person_cannot_write_to_an_agent_they_were_not_given() {
    use outturn::api::scope::ScopeStore;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let owner = h
        .login_as("owner@acme.example", None, Some((acme, "admin")))
        .await;
    let narrowed = h
        .login_as("narrowed@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["accounting", "support"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&owner),
                &format!(r#"{{"name":"{slug}","slug":"{slug}","system_prompt":"x"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (accounting, support) = (ids[0], ids[1]);

    // Somebody else's conversation, with the agent the narrowing excludes.
    // Theirs rather than the caller's, because a person's own session is their
    // own whichever agent it is with -- that exemption is deliberate, and
    // testing against it would prove nothing.
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&owner),
            &format!(r#"{{"agent_id":"{support}","title":"theirs"}}"#),
        )
        .await;
    let theirs: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&owner),
            r#"{"content":"something private"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "owner could not post: {body}");

    // And one of the caller's own, so the feed below has something to return
    // rather than parking for the full long-poll timeout.
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&narrowed),
            &format!(r#"{{"agent_id":"{accounting}","title":"mine"}}"#),
        )
        .await;
    let mine: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.post(
        &format!("/v1/agent-sessions/{mine}/messages"),
        Some(&narrowed),
        r#"{"content":"mine"}"#,
    )
    .await;

    // And one of the caller's own with the agent they are about to lose,
    // started while they still could.
    let mine_with_support = h
        .session_with_a_message(&narrowed, support, "started early")
        .await;

    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("narrowed@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    h.scopes
        .set(acme, me, &[accounting])
        .await
        .expect("set scope");

    // The read was already refused. Stated here so a regression that loosens
    // it fails beside the writes rather than silently.
    let (status, _) = h
        .get(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&narrowed),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the read stopped being narrowed"
    );

    let (status, body) = h
        .send(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/agent-sessions/{theirs}"))
                .header("authorization", format!("Bearer {narrowed}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"renamed by somebody else"}"#))
                .unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "rename was not narrowed: {body}"
    );

    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&narrowed),
            r#"{"content":"posting into a conversation I cannot read"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "send was not narrowed: {body}"
    );

    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{theirs}/cancel"),
            Some(&narrowed),
            "",
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "cancel was not narrowed: {body}"
    );

    let (status, body) = h
        .send(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/agent-sessions/{theirs}"))
                .header("authorization", format!("Bearer {narrowed}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "delete was not narrowed: {body}"
    );

    // The conversation is still there, and still named what its owner named it.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{theirs}/messages"),
            Some(&owner),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the owner lost their session: {body}"
    );

    // Sending into their own session with the excluded agent is refused too.
    // Having started a thread is not permission to keep using an agent
    // somebody has since narrowed away: the tools, the skills and the
    // workspace's allowance are all still reachable through it, and an
    // exemption here would narrow the roster and nothing else.
    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{mine_with_support}/messages"),
            Some(&narrowed),
            r#"{"content":"still talking to the agent I lost"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an old thread kept an excluded agent reachable: {body}"
    );

    // Reading and stopping that same thread are still theirs. Stopping is the
    // safe direction, and refusing it would leave somebody watching an agent
    // they cannot reach spend the allowance.
    let (status, _) = h
        .get(
            &format!("/v1/agent-sessions/{mine_with_support}/messages"),
            Some(&narrowed),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "their own history stopped being theirs"
    );
    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{mine_with_support}/cancel"),
            Some(&narrowed),
            "",
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stopping their own turn was refused"
    );
}

/// The event feed is narrowed the same way the transcript is.
///
/// The feed carries the conversation as it is written -- the same words, a
/// little earlier -- so a narrowing that stopped at the transcript would
/// refuse the history and stream the present. See docs/authorities.md.
#[tokio::test]
async fn the_event_feed_is_narrowed_the_same_way_the_transcript_is() {
    use outturn::api::scope::ScopeStore;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let owner = h
        .login_as("owner@acme.example", None, Some((acme, "admin")))
        .await;
    let narrowed = h
        .login_as("narrowed@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["accounting", "support"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&owner),
                &format!(r#"{{"name":"{slug}","slug":"{slug}","system_prompt":"x"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (accounting, support) = (ids[0], ids[1]);

    let theirs = h
        .session_with_a_message(&owner, support, "something private")
        .await;
    let mine = h
        .session_with_a_message(&narrowed, accounting, "mine")
        .await;

    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("narrowed@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    h.scopes
        .set(acme, me, &[accounting])
        .await
        .expect("set scope");

    let (status, body) = h
        .get(
            "/v1/events?after=00000000-0000-0000-0000-000000000000&limit=500",
            Some(&narrowed),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let feed: serde_json::Value = serde_json::from_str(&body).expect("events");
    let events = feed["events"].as_array().expect("events array");
    assert!(
        events
            .iter()
            .any(|e| e["session_id"] == serde_json::json!(mine.to_string())),
        "the caller's own events were filtered out too: {body}"
    );
    assert!(
        !events
            .iter()
            .any(|e| e["session_id"] == serde_json::json!(theirs.to_string())),
        "an agent the caller was never given streamed through the feed: {body}"
    );

    // The cursor moves over what was filtered out. Left where it was, the next
    // poll rescans the same span and the one after a longer one -- a busy
    // agent the caller cannot see turning an idle reader into a scan of the
    // day's events on every notification.
    let cursor = feed["cursor"].as_str().expect("cursor");
    let newest: Uuid = sqlx::query_scalar(
        "select id from events where workspace_id = $1 order by id desc limit 1",
    )
    .bind(acme)
    .fetch_one(&h.db.pool)
    .await
    .expect("newest event");
    assert_eq!(
        cursor,
        newest.to_string(),
        "the cursor stopped at the last visible event instead of the end of the window: {body}"
    );
}

// ---------------------------------------------------------------------------
// Schedules
//
// The unit tests underneath these exercise cron arithmetic in isolation, which
// is exactly why they missed what these catch: every bug found in review lived
// in the path between the row and the firing, not in the arithmetic.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_cannot_be_pointed_at_another_workspaces_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let other = h.make_workspace("Other", "other").await;

    let acme_admin = h
        .login_as("sched-a@acme.example", None, Some((acme, "admin")))
        .await;
    let other_admin = h
        .login_as("sched-b@other.example", None, Some((other, "admin")))
        .await;

    // An agent belonging to the other workspace.
    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&other_admin),
            r#"{"name":"Theirs","slug":"theirs"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let theirs: Value = serde_json::from_str(&body).expect("agent json");
    let theirs = theirs["id"].as_str().expect("agent id");

    // Acme's admin naming it. The row's workspace_id comes from the token and
    // the agent_id from the body, so without a check this stores one
    // workspace beside another's agent -- and the firing loop would run it.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&acme_admin),
            &format!(
                r#"{{"agent_id":"{theirs}","name":"Borrowed","prompt":"go","expression":"0 9 * * *","timezone":"UTC"}}"#
            ),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another workspace's agent was accepted: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_round_trips_through_the_database() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-c@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Reporter","slug":"reporter"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent json");
    let agent_id = agent["id"].as_str().expect("agent id");

    // Every column the row carries is written and read back here. The columns
    // are named in hand-written SQL rather than checked by the compiler, so a
    // typo only ever surfaces at runtime -- which is what this is for.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Morning","prompt":"Summarise yesterday","expression":"0 9 * * 1-5","timezone":"Europe/London","account":"acme-ops"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let created: Value = serde_json::from_str(&body).expect("schedule json");
    let id = created["id"].as_str().expect("schedule id").to_string();

    assert_eq!(created["account"], "acme-ops");
    assert_eq!(created["timezone"], "Europe/London");
    assert!(
        created["next_run_at"].is_string(),
        "a new schedule should know when it fires next: {body}"
    );
    assert_eq!(
        created["upcoming"].as_array().expect("upcoming").len(),
        3,
        "the editor's next-firings list came back empty: {body}"
    );

    // Listing, filtered by agent.
    let (status, body) = h
        .get(&format!("/v1/schedules?agent_id={agent_id}"), Some(&token))
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("Morning"), "body: {body}");

    // Moving a schedule to another agent is refused rather than answered with
    // a 200 that did not do it.
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Other","slug":"other-agent"}"#,
        )
        .await;
    let second: Value = serde_json::from_str(&body).expect("agent json");
    let second_id = second["id"].as_str().expect("agent id");

    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/schedules/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"agent_id":"{second_id}","name":"Morning","prompt":"Summarise yesterday","expression":"0 9 * * 1-5","timezone":"Europe/London"}}"#
        )))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a schedule was silently moved between agents: {body}"
    );

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/schedules/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn firing_a_schedule_produces_a_turn_nobody_sent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-d@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Nightly","slug":"nightly"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent json");
    let agent_id = agent["id"].as_str().expect("agent id");

    let (_, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Nightly sweep","prompt":"check things","expression":"0 * * * *","timezone":"UTC","account":"acme-ops"}}"#
            ),
        )
        .await;
    let created: Value = serde_json::from_str(&body).expect("schedule json");
    let id: Uuid = created["id"].as_str().expect("id").parse().expect("uuid");

    // Owed an hour ago, so the firing is late and something was missed. Set
    // directly because waiting for the clock is not a test.
    sqlx::query("update schedules set next_run_at = now() - interval '3 hours' where id = $1")
        .bind(id)
        .execute(&h.db.pool)
        .await
        .expect("set due");

    let taken = outturn::api::schedule::postgres::take_due(&h.db.pool, chrono::Utc::now())
        .await
        .expect("take_due")
        .expect("a schedule was due");
    let (schedule, owed) = taken;
    assert_eq!(schedule.id, id);
    // The clear destroys the row's own copy, so the owed time has to survive
    // separately -- passing `now` here is what made every firing look punctual
    // and the skip count permanently zero.
    assert!(
        owed < chrono::Utc::now(),
        "the firing this was owed came back as the present"
    );

    // Nothing is due a second time: the take cleared it, which is what stops
    // two API pods firing one schedule.
    assert!(
        outturn::api::schedule::postgres::take_due(&h.db.pool, chrono::Utc::now())
            .await
            .expect("take_due")
            .is_none(),
        "the same schedule was handed out twice"
    );

    finish!(h);
}

// ---------------------------------------------------------------------------
// Webhook triggers
//
// The public half is the only endpoint here reachable by somebody this
// platform never gave a credential to, so most of what is worth testing is
// what it refuses.
// ---------------------------------------------------------------------------

/// Signs a body the way a sender does, for tests that should be accepted.
/// The window a signature is accepted in, read from the code that enforces it
/// rather than written again here: these tests position signatures relative to
/// its edges, and a copy would keep passing while testing a window production
/// no longer uses.
const TOLERANCE: i64 = outturn::api::webhook::TIMESTAMP_TOLERANCE_SECS;

fn sign(secret: &str, body: &str, at: i64) -> (String, String) {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let ts = at.to_string();
    let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).expect("key");
    mac.update(format!("{ts}.{body}").as_bytes());
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (format!("sha256={hex}"), ts)
}

async fn make_trigger(h: &Harness, token: &str, agent_id: &str, body: &str) -> (String, String) {
    let (status, created) = h.post("/v1/webhook-triggers", Some(token), body).await;
    assert_eq!(status, StatusCode::CREATED, "body: {created}");
    let v: Value = serde_json::from_str(&created).expect("json");
    let path = v["path"].as_str().expect("path").to_string();
    let secret = v["secret"].as_str().expect("secret").to_string();
    (path, secret)
}

async fn deliver(h: &Harness, path: &str, body: &str, headers: &[(&str, &str)]) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/v1/hooks/{path}"))
        .header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let (status, _) = h
        .send(req.body(Body::from(body.to_string())).expect("request"))
        .await;
    status
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_signed_delivery_starts_a_turn_and_an_unsigned_one_does_not() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-a@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Bookings","prompt":"A booking arrived: {{{{body}}}}","scheme":"hmac"}}"#
        ),
    )
    .await;

    let payload = r#"{"event":"booking.created","id":"bk_1"}"#;
    let (sig, ts) = sign(&secret, payload, chrono::Utc::now().timestamp());

    assert_eq!(
        deliver(
            &h,
            &path,
            payload,
            &[("x-outturn-signature", &sig), ("x-outturn-timestamp", &ts)],
        )
        .await,
        StatusCode::ACCEPTED,
        "a correctly signed delivery was not accepted"
    );

    // Unsigned, and otherwise identical.
    assert_eq!(
        deliver(&h, &path, payload, &[]).await,
        StatusCode::NOT_FOUND,
        "an unsigned delivery was accepted"
    );

    // Signed, then the body changed -- which is the whole reason for signing
    // the body rather than sending a token.
    assert_eq!(
        deliver(
            &h,
            &path,
            r#"{"event":"booking.created","id":"bk_TAMPERED"}"#,
            &[("x-outturn-signature", &sig), ("x-outturn-timestamp", &ts)],
        )
        .await,
        StatusCode::NOT_FOUND,
        "a tampered body was accepted"
    );

    // An old signature, correctly computed. Staleness, not replay: replay
    // within the window is permitted and is recorded as such in
    // docs/triggers.md, because nothing here stores which signatures have
    // been seen.
    let old = chrono::Utc::now().timestamp() - 3600;
    let (old_sig, old_ts) = sign(&secret, payload, old);
    assert_eq!(
        deliver(
            &h,
            &path,
            payload,
            &[
                ("x-outturn-signature", &old_sig),
                ("x-outturn-timestamp", &old_ts)
            ],
        )
        .await,
        StatusCode::NOT_FOUND,
        "a delivery signed outside the timestamp window was accepted"
    );

    // The accepted one produced a session with a message nobody sent.
    let (status, body) = h.get("/v1/agent-sessions", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Bookings"),
        "no session was started by the delivery: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_that_exists_is_indistinguishable_from_one_that_does_not() {
    // Otherwise the endpoint is an oracle for which paths are real, and an
    // unauthenticated caller can enumerate a deployment's triggers.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-b@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk2"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, _) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Real","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    let real = deliver(&h, &path, "{}", &[]).await;
    let imaginary = deliver(&h, "0123456789abcdef0123456789abcdef", "{}", &[]).await;
    assert_eq!(real, imaginary, "a real path answered differently");
    assert_eq!(real, StatusCode::NOT_FOUND);

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hourly_ceiling_refuses_rather_than_queueing() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-c@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk3"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    // A ceiling of two, so the third is refused.
    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Busy","prompt":"got {{{{body}}}}","scheme":"shared_secret","max_per_hour":2}}"#
        ),
    )
    .await;

    for n in 1..=2 {
        assert_eq!(
            deliver(&h, &path, "{}", &[("x-outturn-token", &secret)]).await,
            StatusCode::ACCEPTED,
            "delivery {n} of 2 was refused"
        );
    }
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &secret)]).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the ceiling did not refuse the third delivery"
    );

    // And the refusal is recorded, because a hook dropping traffic silently
    // looks exactly like a sender that stopped sending.
    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 1,
        "the refusal was not counted: {listed}"
    );
    // The secret is never handed back.
    assert!(
        !listed.contains(&secret),
        "the secret was returned to a reader: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trigger_cannot_be_pointed_at_another_workspaces_agent() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let other = h.make_workspace("Other", "other").await;
    let acme_admin = h
        .login_as("hook-d@acme.example", None, Some((acme, "admin")))
        .await;
    let other_admin = h
        .login_as("hook-e@other.example", None, Some((other, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&other_admin),
            r#"{"name":"Theirs","slug":"theirs-hook"}"#,
        )
        .await;
    let theirs: Value = serde_json::from_str(&body).expect("agent");
    let theirs = theirs["id"].as_str().expect("id");

    let (status, body) = h
        .post(
            "/v1/webhook-triggers",
            Some(&acme_admin),
            &format!(r#"{{"agent_id":"{theirs}","name":"Borrowed","prompt":"go"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another workspace's agent was accepted: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_before_the_credential_is_proved_writes_nothing() {
    // Three findings had one cause: recording a refusal before knowing the
    // caller holds the secret. It let anybody with the URL drive row-locked
    // updates on the row every real delivery needs, filled the operator's one
    // diagnostic with a prober's noise, and made the two 404s distinguishable
    // by the 1.3ms the write costs.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-f@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk4"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, _secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Quiet","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    // Every refusal that happens before verification.
    let stale = (chrono::Utc::now().timestamp() - 9999).to_string();
    for headers in [
        vec![],
        vec![
            ("x-outturn-signature", "sha256=wrong"),
            ("x-outturn-timestamp", "1"),
        ],
        vec![
            ("x-outturn-signature", "sha256=wrong"),
            ("x-outturn-timestamp", stale.as_str()),
        ],
    ] {
        assert_eq!(
            deliver(&h, &path, "{}", &headers).await,
            StatusCode::NOT_FOUND
        );
    }

    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 0,
        "an unauthenticated caller moved the refusal counter: {listed}"
    );
    assert!(
        rows[0]["last_status"].is_null(),
        "an unauthenticated caller wrote the trigger's status: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disabled_trigger_refuses_without_saying_it_exists() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-g@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk5"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Off","prompt":"got {{{{body}}}}","scheme":"shared_secret","enabled":false}}"#
        ),
    )
    .await;

    // Correct credential, disabled trigger: still a 404, and still no write.
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &secret)]).await,
        StatusCode::NOT_FOUND
    );
    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 0,
        "a disabled trigger was written to: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rotated_secret_replaces_the_one_before_it() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("hook-h@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"desk6"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (_, created) = h
        .post(
            "/v1/webhook-triggers",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Rotating","prompt":"got {{{{body}}}}","scheme":"shared_secret"}}"#
            ),
        )
        .await;
    let v: Value = serde_json::from_str(&created).expect("json");
    let id = v["id"].as_str().expect("id").to_string();
    let path = v["path"].as_str().expect("path").to_string();
    let first = v["secret"].as_str().expect("secret").to_string();

    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &first)]).await,
        StatusCode::ACCEPTED
    );

    let (status, rotated) = h
        .post(
            &format!("/v1/webhook-triggers/{id}/rotate"),
            Some(&token),
            "",
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {rotated}");
    let second = serde_json::from_str::<Value>(&rotated).expect("json")["secret"]
        .as_str()
        .expect("secret")
        .to_string();
    assert_ne!(first, second, "rotating returned the same secret");

    // The old one stops working and the new one works. Rotating that left the
    // old credential valid would be a rotation that rotated nothing.
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &first)]).await,
        StatusCode::NOT_FOUND,
        "the old secret still worked after rotation"
    );
    assert_eq!(
        deliver(&h, &path, "{}", &[("x-outturn-token", &second)]).await,
        StatusCode::ACCEPTED,
        "the new secret did not work"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_still_fires_through_the_shared_trigger_path() {
    // start_turn moved out of schedule/worker.rs into trigger.rs so webhooks
    // could use it. The copy it replaced had three steps missing, found by an
    // earlier review, and the shared version is now the single place where
    // getting this wrong breaks both trigger kinds at once.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sched-e@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Nightly","slug":"nightly2"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (_, body) = h
        .post(
            "/v1/schedules",
            Some(&token),
            &format!(
                r#"{{"agent_id":"{agent_id}","name":"Sweep","prompt":"check things","expression":"0 * * * *","timezone":"UTC","account":"acme-ops"}}"#
            ),
        )
        .await;
    let created: Value = serde_json::from_str(&body).expect("json");
    let id: Uuid = created["id"].as_str().expect("id").parse().expect("uuid");

    sqlx::query("update schedules set next_run_at = now() - interval '1 minute' where id = $1")
        .bind(id)
        .execute(&h.db.pool)
        .await
        .expect("set due");

    let (schedule, owed) =
        outturn::api::schedule::postgres::take_due(&h.db.pool, chrono::Utc::now())
            .await
            .expect("take_due")
            .expect("due");

    let session = outturn::api::trigger::start(
        &h.db.pool,
        outturn::api::trigger::Started {
            workspace_id: schedule.workspace_id,
            agent_id: schedule.agent_id,
            title: schedule.name.clone(),
            prompt: schedule.prompt.clone(),
            account: schedule.account.clone(),
            timezone: Some(schedule.timezone.clone()),
            source: outturn::api::trigger::Source::Schedule(schedule.id),
            metadata: serde_json::json!({ "schedule_id": schedule.id }),
        },
    )
    .await
    .expect("start");
    assert!(owed < chrono::Utc::now());

    // All three steps the earlier review found missing from the old copy.
    let (live, account, sched, events): (i64, Option<String>, Option<Uuid>, i64) = sqlx::query_as(
        "select \
           (select count(*) from live_sessions where session_id = $1), \
           (select account from agent_sessions where id = $1), \
           (select schedule_id from agent_sessions where id = $1), \
           (select count(*) from events where session_id = $1 and kind = 'chat.message')",
    )
    .bind(session)
    .fetch_one(&h.db.pool)
    .await
    .expect("query");

    assert_eq!(live, 1, "the autoscaler cannot see this session");
    assert_eq!(
        account.as_deref(),
        Some("acme-ops"),
        "the ledger loses its account label"
    );
    assert_eq!(
        sched,
        Some(schedule.id),
        "the session forgot which schedule started it"
    );
    assert_eq!(events, 1, "a browser would never see the message arrive");

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowed_person_cannot_trigger_an_agent_they_were_scoped_away_from() {
    // A trigger is a standing instruction to start sessions with an agent, so
    // it has to clear the bar `sessions::create_session` clears. It did not:
    // `agents:update` is deliberately not narrowed per agent, so creating a
    // schedule or a hook was a way around the narrowing rather than a use of
    // it. Two reviews raised this before it was fixed.
    use outturn::api::scope::ScopeStore;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("narrow@acme.example", None, Some((acme, "admin")))
        .await;

    let mut ids = Vec::new();
    for slug in ["books", "helpdesk"] {
        let (_, body) = h
            .post(
                "/v1/agents",
                Some(&admin),
                &format!(r#"{{"name":"{slug}","slug":"{slug}"}}"#),
            )
            .await;
        ids.push(
            serde_json::from_str::<Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    let (books, helpdesk) = (ids[0], ids[1]);

    let me: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("narrow@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the account that logged in");
    h.scopes
        .set(acme, me, &[helpdesk])
        .await
        .expect("set scope");

    // The agent they were given is still reachable both ways.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{helpdesk}","name":"ok","prompt":"go","expression":"0 9 * * *"}}"#
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, body) = h
        .post(
            "/v1/webhook-triggers",
            Some(&admin),
            &format!(r#"{{"agent_id":"{helpdesk}","name":"ok","prompt":"got {{{{body}}}}"}}"#),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // The one they were scoped away from is refused on both.
    let (status, body) = h
        .post(
            "/v1/schedules",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{books}","name":"sneaky","prompt":"go","expression":"0 9 * * *"}}"#
            ),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a scoped-away agent was given a schedule: {body}"
    );

    let (status, body) = h
        .post(
            "/v1/webhook-triggers",
            Some(&admin),
            &format!(r#"{{"agent_id":"{books}","name":"sneaky","prompt":"got {{{{body}}}}"}}"#),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a scoped-away agent was given a public endpoint: {body}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_delivery_is_accepted_once_and_refused_after() {
    // A signature stays valid until its timestamp ages out, so without this
    // the same captured request works repeatedly for the whole window -- each
    // acceptance a new session and a new turn against an agent that may act on
    // the world.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("replay@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"replay-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Once","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    let payload = r#"{"event":"booking.created","id":"bk_once"}"#;
    let (sig, ts) = sign(&secret, payload, chrono::Utc::now().timestamp());
    let headers = [
        ("x-outturn-signature", sig.as_str()),
        ("x-outturn-timestamp", ts.as_str()),
    ];

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED,
        "the first delivery was not accepted"
    );

    // Byte for byte the same request. It holds the credential, so it is told
    // plainly what happened rather than hidden behind a 404.
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT,
        "the same delivery was accepted twice"
    );
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT
    );

    // Exactly one session, so the replays produced no work.
    let sessions: i64 = sqlx::query_scalar(
        "select count(*) from agent_sessions where webhook_trigger_id is not null",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("count");
    assert_eq!(sessions, 1, "a replay started a turn");

    // A genuinely different event, signed at the same instant, is not a
    // replay -- the signature binds the body, so this must still get through.
    let other = r#"{"event":"booking.created","id":"bk_two"}"#;
    let (sig2, ts2) = sign(&secret, other, chrono::Utc::now().timestamp());
    assert_eq!(
        deliver(
            &h,
            &path,
            other,
            &[
                ("x-outturn-signature", sig2.as_str()),
                ("x-outturn-timestamp", ts2.as_str())
            ],
        )
        .await,
        StatusCode::ACCEPTED,
        "a different delivery was mistaken for a replay"
    );

    // And a replay does not move the ceiling's counter, which means one thing.
    let (_, listed) = h.get("/v1/webhook-triggers", Some(&token)).await;
    let rows = Value::Array(items(&listed));
    assert_eq!(
        rows[0]["refused"], 0,
        "a replay was counted against the hourly ceiling: {listed}"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forgotten_delivery_can_be_sent_again() {
    // Deliveries are remembered only while their signature would still be
    // accepted. Past that the row protects nothing, and the sweep removes it
    // -- so the table stays the size of a few minutes of traffic rather than
    // growing for ever.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("sweep@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"sweep-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Sweepable","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    // Signed near the old edge of the tolerance: still inside it, so it is
    // accepted, but its record expires within the window rather than at some
    // point the test would have to wait for.
    let payload = r#"{"event":"sweep"}"#;
    let behind = chrono::Utc::now().timestamp() - (TOLERANCE - 20);
    let (sig, ts) = sign(&secret, payload, behind);
    let headers = [
        ("x-outturn-signature", sig.as_str()),
        ("x-outturn-timestamp", ts.as_str()),
    ];

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT
    );

    // The record is anchored to the signature, so waiting for one to lapse is
    // waiting for both. Moving the clock forward stands in for that wait --
    // and it moves the *record*, which is what the sweep reads, exactly as far
    // as the signature has already aged past its own edge.
    //
    // Deliberately not `expires_at = now() - interval` out of nowhere: that
    // would sweep a record whose signature was still good, which is a state
    // this code cannot produce and a test asserting on it would be asserting
    // on fiction.
    sqlx::query("update webhook_deliveries set expires_at = to_timestamp($1)")
        .bind((behind - 600 + TOLERANCE) as f64)
        .execute(&h.db.pool)
        .await
        .expect("age");
    let gone = outturn::api::webhook::postgres::forget_expired(&h.db.pool)
        .await
        .expect("sweep");
    assert_eq!(gone, 1, "the sweep forgot nothing");

    // Nothing is let through by the sweep. The record is gone, so the replay
    // check no longer refuses it -- but the signature it carries is the thing
    // the record was anchored to, and in production the two lapse together.
    // Here the signature is still inside its window, so this asserts the
    // narrower fact the sweep is responsible for: the row went, and the table
    // does not grow without bound.
    let remembered: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(remembered, 0, "the sweep left the record behind");

    // The name's own claim, asserted rather than implied: once the record is
    // gone the replay check no longer refuses these bytes. Without this the
    // test passes against a `remember` that refuses everything after the
    // first insert, since every other assertion here is about the sweep's SQL.
    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED,
        "a forgotten delivery was still refused as a replay"
    );

    // And a signature genuinely past its window is refused whether or not a
    // record survives -- the timestamp check is what bounds replay once the
    // table has forgotten, and it is checked before the digest.
    let stale = r#"{"event":"stale"}"#;
    let (sig2, ts2) = sign(
        &secret,
        stale,
        chrono::Utc::now().timestamp() - TOLERANCE - 100,
    );
    assert_eq!(
        deliver(
            &h,
            &path,
            stale,
            &[
                ("x-outturn-signature", sig2.as_str()),
                ("x-outturn-timestamp", ts2.as_str())
            ],
        )
        .await,
        StatusCode::NOT_FOUND,
        "a signature past its window was accepted"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_the_ceiling_refused_can_be_sent_again() {
    // The replay record is claimed before the ceiling is consulted, so that a
    // replay spends nobody's allowance. The hazard in that ordering is the
    // mirror image: a delivery the ceiling turns away has already claimed its
    // slot, and a 429 is precisely the response that asks a sender to send the
    // same bytes again. Left claimed, that retry comes back 409 and the event
    // is lost -- silently, which is worse than the duplicate turn the record
    // exists to prevent.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("ceiling@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"ceiling-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    // One delivery an hour, so the second is refused by the ceiling.
    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Tight","prompt":"got {{{{body}}}}","max_per_hour":1}}"#
        ),
    )
    .await;

    let first = r#"{"event":"first"}"#;
    let (sig1, ts1) = sign(&secret, first, chrono::Utc::now().timestamp());
    assert_eq!(
        deliver(
            &h,
            &path,
            first,
            &[
                ("x-outturn-signature", sig1.as_str()),
                ("x-outturn-timestamp", ts1.as_str())
            ],
        )
        .await,
        StatusCode::ACCEPTED
    );

    // A different event, refused because the ceiling is spent.
    let second = r#"{"event":"second"}"#;
    let (sig2, ts2) = sign(&secret, second, chrono::Utc::now().timestamp());
    let headers2 = [
        ("x-outturn-signature", sig2.as_str()),
        ("x-outturn-timestamp", ts2.as_str()),
    ];
    assert_eq!(
        deliver(&h, &path, second, &headers2).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Raise the ceiling, as an operator would, and let the sender retry the
    // same signed request. It must be admitted: it never became a turn, so it
    // is not a replay of one.
    sqlx::query("update webhook_triggers set max_per_hour = 100 where path = $1")
        .bind(&path)
        .execute(&h.db.pool)
        .await
        .expect("raise");

    assert_eq!(
        deliver(&h, &path, second, &headers2).await,
        StatusCode::ACCEPTED,
        "a ceiling-refused delivery was refused as a replay on retry, so the event was lost"
    );

    // Two turns, not one: the retry produced the turn the 429 had denied.
    let sessions: i64 = sqlx::query_scalar(
        "select count(*) from agent_sessions where webhook_trigger_id is not null",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("count");
    assert_eq!(sessions, 2, "the retried delivery did not start a turn");

    // And now that it *has* become a turn, sending it a third time is a real
    // replay and must be refused.
    assert_eq!(
        deliver(&h, &path, second, &headers2).await,
        StatusCode::CONFLICT,
        "a delivery that became a turn was replayable"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_losing_racer_is_not_undone_by_the_winner_failing() {
    // Two byte-identical deliveries arrive together; one claims the record and
    // the other is refused as a replay. If the claim were keyed on the digest
    // alone, the winner failing afterwards would release the row the loser's
    // refusal rests on -- no turn, a 409 already sent saying one had started,
    // and the event lost. The claimant is what keeps a release to its own row.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("race@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"race-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    // One an hour, so the second delivery is the one the ceiling refuses.
    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Racy","prompt":"got {{{{body}}}}","max_per_hour":1}}"#
        ),
    )
    .await;

    // Spend the allowance, so the next delivery claims its record and is then
    // refused by the ceiling -- the winner-fails half of the race, made
    // deterministic.
    let first = r#"{"event":"first"}"#;
    let (sig1, ts1) = sign(&secret, first, chrono::Utc::now().timestamp());
    assert_eq!(
        deliver(
            &h,
            &path,
            first,
            &[
                ("x-outturn-signature", sig1.as_str()),
                ("x-outturn-timestamp", ts1.as_str())
            ],
        )
        .await,
        StatusCode::ACCEPTED
    );

    let racer = r#"{"event":"racer"}"#;
    let (sig2, ts2) = sign(&secret, racer, chrono::Utc::now().timestamp());
    let headers2 = [
        ("x-outturn-signature", sig2.as_str()),
        ("x-outturn-timestamp", ts2.as_str()),
    ];

    // Claims, then loses to the ceiling, then releases its own row.
    assert_eq!(
        deliver(&h, &path, racer, &headers2).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // The racer released its own row and nothing else: the first delivery
    // became a turn, so its record must survive -- releasing by digest alone
    // would be indistinguishable here, but a release that took the wrong row
    // would leave that first delivery replayable.
    let remembered: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        remembered, 1,
        "the racer did not release its own row, or took one that was not its"
    );

    // Which row survived: the one that became a turn, still unreplayable.
    assert_eq!(
        deliver(
            &h,
            &path,
            first,
            &[
                ("x-outturn-signature", sig1.as_str()),
                ("x-outturn-timestamp", ts1.as_str())
            ],
        )
        .await,
        StatusCode::CONFLICT,
        "the racer's release took the accepted delivery's record with it"
    );

    // And the racer's own bytes are retryable, which is what the release is
    // for: raise the ceiling and the event it was refused for still lands.
    sqlx::query("update webhook_triggers set max_per_hour = 100 where path = $1")
        .bind(&path)
        .execute(&h.db.pool)
        .await
        .expect("raise");
    assert_eq!(
        deliver(&h, &path, racer, &headers2).await,
        StatusCode::ACCEPTED,
        "the racer could not be retried after its claim was released"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shared_secret_sender_may_repeat_a_payload() {
    // `shared_secret` binds no time, so the only thing to key a replay record
    // on is the token and the body -- which makes a heartbeat, a retry, or the
    // same event reported twice indistinguishable from a replay. It would also
    // buy nothing: the credential travels on every delivery and never expires,
    // so anyone holding it mints fresh requests rather than resending an old
    // one. Refusing real traffic to defend against nothing is the trade this
    // pins shut.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("repeat@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"repeat-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(
            r#"{{"agent_id":"{agent_id}","name":"Repeats","prompt":"got {{{{body}}}}","scheme":"shared_secret"}}"#
        ),
    )
    .await;

    let payload = r#"{"event":"heartbeat"}"#;
    for attempt in 1..=3 {
        assert_eq!(
            deliver(&h, &path, payload, &[("x-outturn-token", &secret)]).await,
            StatusCode::ACCEPTED,
            "an identical shared_secret payload was refused on attempt {attempt}"
        );
    }

    // And nothing was written to the replay table, which is what keeps the
    // sweep bounded by hmac traffic alone.
    let remembered: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        remembered, 0,
        "a shared_secret delivery was recorded, so identical payloads will collide"
    );

    finish!(h);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_future_dated_signature_is_remembered_until_it_stops_verifying() {
    // The window is checked with `abs`, so a signature stamped in the future
    // verifies now *and* keeps verifying until its own timestamp plus the
    // tolerance. Anchoring the record to arrival instead would sweep it while
    // the signature it refuses is still good -- and the replay walks in behind
    // the sweep. The record must outlive the credential, not the request.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("ahead@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&token),
            r#"{"name":"Desk","slug":"ahead-desk"}"#,
        )
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let agent_id = agent["id"].as_str().expect("id");

    let (path, secret) = make_trigger(
        &h,
        &token,
        agent_id,
        &format!(r#"{{"agent_id":"{agent_id}","name":"Ahead","prompt":"got {{{{body}}}}"}}"#),
    )
    .await;

    // Stamped near the far edge of the tolerance, which still verifies.
    let ahead = chrono::Utc::now().timestamp() + (TOLERANCE - 60);
    let payload = r#"{"event":"ahead"}"#;
    let (sig, ts) = sign(&secret, payload, ahead);
    let headers = [
        ("x-outturn-signature", sig.as_str()),
        ("x-outturn-timestamp", ts.as_str()),
    ];

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::ACCEPTED
    );

    // The record must still be there after an arrival-anchored one would have
    // gone, because the signature is still being accepted.
    let gone = outturn::api::webhook::postgres::forget_expired(&h.db.pool)
        .await
        .expect("sweep");
    assert_eq!(gone, 0, "a still-valid signature was forgotten");

    let expires: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("select expires_at from webhook_deliveries")
            .fetch_one(&h.db.pool)
            .await
            .expect("expiry");
    // Strictly past what arrival-anchoring would give. Signed at now+240 with
    // a 300s tolerance, the signature is good until now+540; an arrival
    // anchor would expire at now+300. Asserting past 300 is what makes the
    // two distinguishable -- an earlier version compared against 240, which
    // both satisfy, and so pinned nothing.
    let floor = chrono::Utc::now().timestamp() + TOLERANCE;
    assert!(
        expires.timestamp() > floor,
        "expiry {} is not past the arrival-anchored {floor}, so the record dies before the signature",
        expires.timestamp()
    );

    assert_eq!(
        deliver(&h, &path, payload, &headers).await,
        StatusCode::CONFLICT,
        "a future-dated signature was replayable"
    );

    finish!(h);
}

// --- the notification centre ----------------------------------------------

/// A GET carrying the session cookie a login returned.
async fn get_with_cookie(h: &Harness, uri: &str, cookie: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .header("cookie", format!("outturn_session={cookie}"))
        .body(Body::empty())
        .expect("request");
    h.send(req).await
}

#[tokio::test]
async fn the_queue_returns_what_is_waiting_on_the_reader() {
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let cookie = h
        .login_as("queue@test.invalid", None, Some((workspace, "admin")))
        .await;

    // Raised through the store, since nothing posts these yet.
    let store = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");
    {
        use outturn::api::actions::ActionStore as _;
        store
            .raise(
                workspace,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({"question": "approve?"}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise");
    }

    let (status, body) = get_with_cookie(&h, "/v1/action-items", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["items"].as_array().expect("items").len(), 1);
    assert_eq!(v["items"][0]["kind"], "approval.charge");

    let (status, body) = get_with_cookie(&h, "/v1/action-items/count", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["count"], 1);
    assert_eq!(v["capped"], false);

    h.db.cleanup().await;
}

#[tokio::test]
async fn the_queue_refuses_a_caller_with_no_session() {
    // Authenticated but not authorised is the design; unauthenticated is still
    // refused, and this is what says the first did not cost the second.
    let h = harness().await;

    let (status, _) = h.get("/v1/action-items", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = h.get("/v1/action-items/count", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    h.db.cleanup().await;
}

#[tokio::test]
async fn the_queue_shows_nothing_from_a_workspace_the_reader_left() {
    let h = harness().await;
    let mine = h.make_workspace("Mine", "mine").await;
    let theirs = h.make_workspace("Theirs", "theirs").await;
    let cookie = h
        .login_as("reader@test.invalid", None, Some((mine, "admin")))
        .await;
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");

    let store = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    {
        use outturn::api::actions::ActionStore as _;
        // Addressed to this very person, in a workspace they hold no role in.
        store
            .raise(
                theirs,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise elsewhere");
    }

    let (status, body) = get_with_cookie(&h, "/v1/action-items/count", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        v["count"], 0,
        "an item from a workspace the reader has no role in reached them"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_waiting_queue_read_wakes_when_something_is_raised() {
    // The long poll, and the reason the bus exists. Parks, then something is
    // raised for this person, and the parked read returns it well inside the
    // 25-second timeout -- so it woke on the notification rather than timing
    // out.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let cookie = h
        .login_as("waiter@test.invalid", None, Some((workspace, "admin")))
        .await;
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");

    let pool = h.db.pool.clone();
    let raiser = tokio::spawn(async move {
        // Long enough that the read is certainly parked before this lands.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let store = outturn::api::actions::PostgresActionStore::new(pool);
        use outturn::api::actions::ActionStore as _;
        store
            .raise(
                workspace,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise");
    });

    let started = std::time::Instant::now();
    let (status, body) = get_with_cookie(&h, "/v1/action-items?wait=true", &cookie).await;
    let waited = started.elapsed();
    raiser.await.expect("raiser");

    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["items"].as_array().expect("items").len(), 1);
    // Woken rather than timed out. Not an assertion about how fast it was --
    // only that it did not sit out the full park.
    assert!(
        waited < std::time::Duration::from_secs(20),
        "the read waited {waited:?}, so it timed out rather than being woken"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_waiting_read_returns_the_current_queue_when_nothing_happens() {
    // A timeout answers with what is there rather than empty: the queue is a
    // set, so "nothing changed" still has a current value.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let cookie = h
        .login_as("quiet@test.invalid", None, Some((workspace, "admin")))
        .await;
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");

    let store = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    {
        use outturn::api::actions::ActionStore as _;
        store
            .raise(
                workspace,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise");
    }

    // Raised before the read parks, so no notification arrives while it waits.
    // It should still answer with the item rather than an empty page.
    let (status, body) = get_with_cookie(&h, "/v1/action-items?wait=true", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["items"].as_array().expect("items").len(), 1);

    h.db.cleanup().await;
}

// --- approvals ------------------------------------------------------------
//
// The whole path: a conversation is held, somebody is asked, they answer, and
// the parked turn is given back. See docs/approvals.md.

/// A POST carrying the session cookie a login returned.
async fn post_with_cookie(
    h: &Harness,
    uri: &str,
    cookie: &str,
    body: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("cookie", format!("outturn_session={cookie}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    h.send(req).await
}

/// An agent and a conversation with one message in it, so a turn exists.
async fn a_held_conversation(h: &Harness, workspace: Uuid, cookie: &str) -> (Uuid, Uuid) {
    let agent = h
        .agents
        .create(
            workspace,
            outturn::api::agent::CreateAgent {
                name: "Front Desk".into(),
                slug: "front-desk".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("create agent");

    let req = Request::builder()
        .method("POST")
        .uri("/v1/agent-sessions")
        .header("cookie", format!("outturn_session={cookie}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "agent_id": agent.id, "title": "t" }).to_string(),
        ))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::CREATED, "session: {body}");
    let session: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (agent.id, session)
}

#[tokio::test]
async fn an_approval_holds_the_conversation_and_asks_somebody() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("admin role");

    let (status, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session,
            "requires": "charge",
            "reason": "£180 to payment account PA-4471",
            "roles": [role],
            "payload": { "amount_pence": 18000, "booking_id": "b-8812" }
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let raised: Value = serde_json::from_str(&body).expect("json");

    // A suspended hold, which nothing in this codebase took before.
    let strength: String = sqlx::query_scalar("select strength from inhibitors where id = $1")
        .bind(
            raised["inhibitor_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .fetch_one(&h.db.pool)
        .await
        .expect("hold");
    assert_eq!(strength, "suspended");

    // And it is in the asked role's queue, with what they need to decide.
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let queue: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(queue["items"].as_array().unwrap().len(), 1);
    assert_eq!(queue["items"][0]["kind"], "approval.charge");
    assert_eq!(queue["items"][0]["payload"]["amount_pence"], 18000);

    h.db.cleanup().await;
}

#[tokio::test]
async fn an_approval_addressed_to_nobody_is_refused() {
    // It would park the conversation for ever, and the orphan is only findable
    // by somebody who thinks to look.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    let (status, _) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session,
            "requires": "charge",
            "reason": "£180",
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Nothing was held.
    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0, "a refused approval left a hold behind");

    h.db.cleanup().await;
}

#[tokio::test]
async fn an_approval_needs_a_reason_somebody_can_act_on() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (status, _) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "  ", "roles": [role]
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_conversation_in_another_workspace_cannot_be_held() {
    // Otherwise a caller could park somebody else's conversation by id.
    let h = harness().await;
    let mine = h.make_workspace("Mine", "mine").await;
    let theirs = h.make_workspace("Theirs", "theirs").await;
    let their_admin = h
        .login_as("them@test.invalid", None, Some((theirs, "admin")))
        .await;
    let (_, their_session) = a_held_conversation(&h, theirs, &their_admin).await;

    let my_admin = h
        .login_as("me@test.invalid", None, Some((mine, "admin")))
        .await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(mine)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (status, _) = post_with_cookie(
        &h,
        "/v1/approvals",
        &my_admin,
        &serde_json::json!({
            "session_id": their_session,
            "requires": "charge",
            "reason": "£180",
            "roles": [role]
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    h.db.cleanup().await;
}

#[tokio::test]
async fn approving_releases_the_hold_and_gives_the_turn_back() {
    // The end of the path, and the reason parking exists.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180", "roles": [role]
        })
        .to_string(),
    )
    .await;
    let raised: Value = serde_json::from_str(&body).expect("json");
    let item = raised["id"].as_str().unwrap();
    let hold: Uuid = raised["inhibitor_id"].as_str().unwrap().parse().unwrap();

    // A turn parked under this conversation, as the worker would have left one.
    let job = outturn::jobs::enqueue(
        &h.db.pool,
        workspace,
        "chat.turn",
        serde_json::json!({
            "workspace_id": workspace, "session_id": session,
            "agent_id": agent, "message_id": Uuid::now_v7(),
        }),
        None,
        Some(&format!("session:{session}")),
        outturn::jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &["chat.turn"],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    outturn::jobs::park(&h.db.pool, job, claimed[0].job.lease_token)
        .await
        .expect("park");

    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answered: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(answered["resumed"], 1, "the parked turn was not given back");

    // The hold is gone and the turn is claimable again.
    let still: i64 = sqlx::query_scalar("select count(*) from inhibitors where id = $1")
        .bind(hold)
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(still, 0, "approving left the hold on");
    let state: String = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(job)
        .fetch_one(&h.db.pool)
        .await
        .expect("state");
    assert_eq!(state, "pending");

    // And it has left the queue.
    let (_, body) = get_with_cookie(&h, "/v1/action-items/count", &admin).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["count"], 0);

    h.db.cleanup().await;
}

#[tokio::test]
async fn declining_lets_the_conversation_carry_on() {
    // This used to assert the opposite, on the reasoning that "nothing has
    // changed about whether the work may proceed". True of the *work*, and the
    // grant is what enforces it -- a decline mints none, so the gate refuses the
    // same call again. But the hold is session-scoped, so leaving it up
    // suspended every later turn in the conversation, and the item was
    // `Cancelled` so nobody could answer it again and nothing else moves a job
    // out of `parked`. Declining one charge ended the conversation, recoverable
    // only by an operator releasing the hold by id.
    //
    // What stops the agent asking again immediately is `DECLINED_GUIDANCE` in
    // the transcript, not the hold.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£4000", "roles": [role]
        })
        .to_string(),
    )
    .await;
    let raised: Value = serde_json::from_str(&body).expect("json");
    let item = raised["id"].as_str().unwrap();
    let hold: Uuid = raised["inhibitor_id"].as_str().unwrap().parse().unwrap();

    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":false,"note":"too much for a first-time guest"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Zero parked turns to give back: this fixture holds a conversation without
    // one. What matters here is the hold, checked below -- `resumed` is
    // exercised against a real parked job in `tests/action_queue.rs`.
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["resumed"], 0);

    let still: i64 = sqlx::query_scalar("select count(*) from inhibitors where id = $1")
        .bind(hold)
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        still, 0,
        "a declined approval must not leave the session suspended"
    );

    // And nothing was granted, which is what keeps the work from happening.
    let grants: i64 =
        sqlx::query_scalar("select count(*) from approval_grants where workspace_id = $1")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("grants");
    assert_eq!(grants, 0, "a decline grants nothing");

    h.db.cleanup().await;
}

#[tokio::test]
async fn answering_twice_is_refused() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180", "roles": [role]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (first, _) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(first, StatusCode::OK);

    // Answered once already: the second is told it lost rather than silently
    // overwriting the first decision.
    let (second, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":false}"#,
    )
    .await;
    assert_eq!(second, StatusCode::CONFLICT, "{body}");

    h.db.cleanup().await;
}

#[tokio::test]
async fn somebody_without_the_authority_cannot_answer() {
    // The two-user case: one may approve, the other may not, and it is an
    // authority rather than a prop.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("manager@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    // A role with everything an operator needs and no approvals:answer.
    let (status, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &admin,
        r#"{"name":"desk","authorities":["sessions:read","sessions:create","agents:read"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let desk_role: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180",
            "roles": [desk_role]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let clerk = h
        .login_as("clerk@test.invalid", None, Some((workspace, "desk")))
        .await;

    // It is in their queue -- they are who was asked --
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &clerk).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "the asked role did not see it"
    );

    // -- and they still may not answer it.
    let (status, _) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &clerk,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    h.db.cleanup().await;
}

#[tokio::test]
async fn an_approval_somebody_was_not_asked_about_is_not_theirs_to_answer() {
    // Targeting would be decorative if any holder of approvals:answer could
    // answer anything.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("manager@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    // Addressed to a role nobody in this test holds.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &admin,
        r#"{"name":"finance","authorities":["approvals:answer"]}"#,
    )
    .await;
    let finance: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180", "roles": [finance]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Another person who holds approvals:answer through a different role, and
    // who has an approval of their own waiting -- so their queue is not empty.
    // Without that, a lookup that ignored the id entirely would still refuse
    // this, and the test would pass while proving nothing.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &admin,
        r#"{"name":"ops","authorities":["approvals:answer","sessions:read"]}"#,
    )
    .await;
    let ops_role: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let other = h
        .login_as("ops@test.invalid", None, Some((workspace, "ops")))
        .await;

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "refund", "reason": "£20",
            "roles": [ops_role]
        })
        .to_string(),
    )
    .await;
    let theirs = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(theirs, item, "the two approvals must be different items");
    // Left unanswered on purpose. Their queue has to be non-empty *at the
    // moment of the refusal below*, or a lookup that ignored the id and took
    // whatever was first would find nothing and refuse for the wrong reason --
    // which is exactly what an earlier version of this test did.
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &other).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let queue = serde_json::from_str::<Value>(&body).unwrap();
    assert_eq!(
        queue["items"].as_array().unwrap().len(),
        1,
        "the second reader should have exactly their own waiting: {body}"
    );
    assert_eq!(queue["items"][0]["id"], theirs);

    let (status, _) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &other,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "somebody answered an approval they were not asked about"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn answering_needs_the_authority_in_the_items_own_workspace() {
    // The queue read is global by design, so an item can come from a workspace
    // the token was not minted for -- and the authority has to be resolved
    // there rather than against whichever workspace the caller happens to be
    // signed into. Otherwise holding approvals:answer in one workspace
    // authorises payments in every other one the person belongs to.
    let h = harness().await;
    let armed = h.make_workspace("Armed", "armed").await;
    let unarmed = h.make_workspace("Unarmed", "unarmed").await;

    // Each workspace's own admin, since a session is created in the workspace
    // the token names.
    let armed_admin = h
        .login_as("armed@test.invalid", None, Some((armed, "admin")))
        .await;
    let unarmed_admin = h
        .login_as("unarmed@test.invalid", None, Some((unarmed, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, unarmed, &unarmed_admin).await;

    // The role carrying approvals:answer lives in `armed`.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &armed_admin,
        r#"{"name":"finance","authorities":["approvals:answer","sessions:read"]}"#,
    )
    .await;
    assert!(body.contains("finance"), "{body}");

    // And the targeted role in `unarmed` carries no such authority.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &unarmed_admin,
        r#"{"name":"desk","authorities":["sessions:read"]}"#,
    )
    .await;
    let desk: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &unarmed_admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£4000",
            "roles": [desk]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // One person in both workspaces: finance in `armed`, desk in `unarmed`.
    let split = h
        .login_as("split@test.invalid", None, Some((armed, "finance")))
        .await;
    let split_id: Uuid = sqlx::query_scalar(
        "select u.id from users u join user_identities i on i.user_id = u.id \
         where i.provider_subject = 'split@test.invalid'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("the split user");
    h.users
        .grant_workspace_role(split_id, unarmed, "desk")
        .await
        .expect("grant desk in the other workspace");

    // It is in their queue -- the read is global and they hold the targeted
    // role there.
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &split).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "the global read should have reached the other workspace: {body}"
    );

    // And they must not be able to answer it: their approvals:answer is in
    // `armed`, and this item is `unarmed`'s.
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &split,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an authority held in another workspace authorised this: {body}"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_gate_declaration_with_no_host_is_refused_at_publish() {
    // The file would say an operation is gated and nothing could gate it: the
    // gate rows are written per declared host, so a version naming none stores
    // nothing, `gates_for_turn` returns the empty set, and the charge goes out.
    // That is the quiet failure the whole mechanism is arranged against, and the
    // fix is one line in the publish -- so it is refused rather than accepted.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;

    let body = serde_json::json!({
        "slug": "charging",
        "name": "Charging",
        "body": "One operation. Read charge.md before using it.",
        "files": [{
            "path": "charge.md",
            "content": "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n",
        }],
    })
    .to_string();

    let (status, message) = post_with_cookie(&h, "/v1/skills", &admin, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{message}");
    assert!(
        message.contains("names no host"),
        "the refusal should say what is missing: {message}"
    );

    // And the same skill with a host publishes.
    let with_host = serde_json::json!({
        "slug": "charging",
        "name": "Charging",
        "body": "One operation. Read charge.md before using it.",
        // A public name, since a cluster-internal one needs the operator allowlist
        // and this test is about gates rather than about egress.
        "hosts": ["api.hollowbrook.test"],
        "files": [{
            "path": "charge.md",
            "content": "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n",
        }],
    })
    .to_string();
    let (status, message) = post_with_cookie(&h, "/v1/skills", &admin, &with_host).await;
    assert_eq!(status, StatusCode::CREATED, "{message}");

    // Stored as the egress matcher spells it, which is what the gateway compares
    // against -- `normalise_host` strips a port and lower-cases, so a gate written
    // from the host as typed would never match the request.
    let host: String = sqlx::query_scalar(
        "select host from skill_version_gates \
         where path = 'charge.md' and requires = 'charge'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("a gate row");
    assert_eq!(host, "api.hollowbrook.test");

    h.db.cleanup().await;
}

#[tokio::test]
async fn approve_new_hosts_gates_a_hand_added_host_and_not_a_skills_own() {
    // The setting, end to end: what it turns into, and what it exempts. A host a
    // skill's declaration opened was consented to when the skill was installed --
    // in an act that named the skill and the host together -- so asking again per
    // conversation would be asking the same question somewhere worse. A host
    // somebody added by hand says agents *may* reach it, not that any particular
    // use of it was reviewed, and that is the case the setting is for.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@test.invalid", None, Some((workspace, "admin")))
        .await;

    // One host from a skill, one added by hand.
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "brought",
            "name": "Brought By A Skill",
            "body": "Reaches api.brought.test.",
            "hosts": ["api.brought.test"],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/skills/{skill}/hosts/approve"),
        &admin,
        "{}",
    )
    .await;
    assert!(
        status.is_success(),
        "approving the skill's host: {status} {body}"
    );

    let (status, body) = post_with_cookie(
        &h,
        "/v1/egress-rules",
        &admin,
        &serde_json::json!({ "host": "api.byhand.test" }).to_string(),
    )
    .await;
    assert!(
        status.is_success(),
        "adding a rule by hand: {status} {body}"
    );

    // Off by default: nothing is gated.
    let rules = outturn::api::egress::rules_for(&h.db.pool, workspace)
        .await
        .expect("rules");
    let hosts: Vec<String> = rules.iter().map(|r| r.host.clone()).collect();
    assert!(hosts.contains(&"api.byhand.test".to_string()), "{hosts:?}");
    assert!(hosts.contains(&"api.brought.test".to_string()), "{hosts:?}");

    let exempt = outturn::api::egress::hosts_from_skills(&h.db.pool, workspace)
        .await
        .expect("skill hosts");
    assert_eq!(
        exempt,
        vec!["api.brought.test".to_string()],
        "only the skill's host should be exempt"
    );

    // What the setting turns into.
    let gates = outturn::egress::gate::Gates::of(outturn::egress::gate::for_unreviewed_hosts(
        &hosts, &exempt,
    ));
    assert!(
        gates
            .covering("api.byhand.test", "GET", "/anything")
            .is_some(),
        "the hand-added host should need approving"
    );
    assert!(
        gates
            .covering("api.brought.test", "GET", "/anything")
            .is_none(),
        "the skill's own host was already consented to"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_skill_with_two_hosts_gates_both_of_them() {
    // The gate row was keyed on `(version_id, path, requires)` with no host,
    // while the write loops over the skill's hosts with `on conflict do
    // nothing` -- so the second host's gate was silently dropped and requests
    // to it went out unapproved. A gate failing open, with nothing saying so.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let charge_file = "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n";
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "two-hosts",
            "name": "Two hosts",
            "body": "One operation, two places it can go.",
            "hosts": ["api.one.test", "api.two.test"],
            "files": [{ "path": "charge.md", "content": charge_file }],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let hosts: Vec<String> = sqlx::query_scalar(
        "select g.host from skill_version_gates g \
           join skill_versions v on v.id = g.version_id \
           join skills s on s.id = v.skill_id \
          where s.slug = 'two-hosts' order by g.host",
    )
    .fetch_all(&h.db.pool)
    .await
    .expect("gates");

    assert_eq!(
        hosts,
        vec!["api.one.test".to_string(), "api.two.test".to_string()],
        "both hosts must be gated; a dropped one is a request nobody gates"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_body_only_edit_keeps_the_gates_its_files_declare() {
    // The quiet failure the whole mechanism is arranged against, found by review.
    //
    // Files carry forward when a version brings none of its own -- that is how
    // somebody fixes a typo in a skill's prose. The gates have to carry with them,
    // because the declaration lives *in* those files. They did not: the carry
    // branch keyed on `based_on`, which is the latest version of the *base* skill
    // and is null for every ordinary skill, so neither branch ran and the new
    // version had no gates at all. The file still said the operation needed a
    // charge, `gates_for_turn` returned the empty set, and every turn after the
    // edit was ungated with nothing logged and nothing refused.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@test.invalid", None, Some((workspace, "admin")))
        .await;

    let charge_file = "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n";
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "charging",
            "name": "Charging",
            "body": "One operation. Read charge.md.",
            "hosts": ["api.hollowbrook.test"],
            "files": [{ "path": "charge.md", "content": charge_file }],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let gates_for = |version: Uuid| {
        let pool = h.db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "select count(*) from skill_version_gates where version_id = $1",
            )
            .bind(version)
            .fetch_one(&pool)
            .await
            .expect("count")
        }
    };

    let first: Uuid = sqlx::query_scalar(
        "select id from skill_versions where skill_id = $1 order by ordinal desc limit 1",
    )
    .bind(skill)
    .fetch_one(&h.db.pool)
    .await
    .expect("the first version");
    assert_eq!(
        gates_for(first).await,
        1,
        "the first version should be gated"
    );

    // A reworded body and no files: the ordinary way a typo is fixed.
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/skills/{skill}/versions"),
        &admin,
        &serde_json::json!({
            "body": "One operation. Read charge.md before charging anybody.",
            "hosts": ["api.hollowbrook.test"],
        })
        .to_string(),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");

    let second: Uuid = sqlx::query_scalar(
        "select id from skill_versions where skill_id = $1 order by ordinal desc limit 1",
    )
    .bind(skill)
    .fetch_one(&h.db.pool)
    .await
    .expect("the second version");
    assert_ne!(second, first, "a version should have been appended");

    // The files carried forward, so the declaration did too.
    let files: i64 =
        sqlx::query_scalar("select count(*) from skill_version_files where version_id = $1")
            .bind(second)
            .fetch_one(&h.db.pool)
            .await
            .expect("files");
    assert_eq!(files, 1, "the file should have carried forward");
    assert_eq!(
        gates_for(second).await,
        1,
        "the gate its file declares was dropped, so every later turn is ungated"
    );

    // And what the gate binds, which is the half a count of gates cannot see.
    // A gate whose binds did not travel is not ungated -- it still refuses -- but
    // every grant taken out under it is keyed on the path alone, so one approved
    // charge covers a charge for any amount. Counting gates alone passed while
    // that was true.
    let binds: Vec<String> = sqlx::query_scalar(
        "select field from skill_version_gate_binds where version_id = $1 order by position",
    )
    .bind(second)
    .fetch_all(&h.db.pool)
    .await
    .expect("binds");
    assert_eq!(
        binds,
        vec!["amount_pence".to_string()],
        "the bound fields did not carry forward, so later grants bind nothing"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_wildcard_host_is_never_exempt_from_the_ceiling() {
    // Approving a skill's hosts takes `settings:update`, which is also what turns
    // the ceiling on -- so whoever sets it can exempt a host from it, and
    // `normalise_host` permits a wildcard over a domain. Declaring `*.example.com`
    // in a skill and approving it would exempt every host under it from every turn.
    // (`*.com` is refused outright, since `com` has no domain of its own.)
    //
    // Not an escalation across an authority boundary, but a wider door than the
    // setting reads as having, and the same hazard the `covers` section of
    // docs/approvals.md is about: approving the instance you were shown is not
    // approving the class it belongs to.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@test.invalid", None, Some((workspace, "admin")))
        .await;

    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "wide",
            "name": "Wide Open",
            "body": "Reaches anything.",
            // `*.com` is already refused -- `com` has no domain -- so the widest
            // form actually reachable is a wildcard over a real domain.
            "hosts": ["*.example.com", "api.named.test"],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/skills/{skill}/hosts/approve"),
        &admin,
        "{}",
    )
    .await;
    assert!(status.is_success(), "{status} {body}");

    let exempt = outturn::api::egress::hosts_from_skills(&h.db.pool, workspace)
        .await
        .expect("skill hosts");
    assert!(
        exempt.contains(&"api.named.test".to_string()),
        "a named host a skill brought should be exempt: {exempt:?}"
    );
    assert!(
        !exempt.iter().any(|h| h.contains('*')),
        "a wildcard was exempted: {exempt:?}"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn the_roster_says_which_agents_a_caller_may_talk_to() {
    use outturn::api::scope::ScopeStore;

    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let operator = h
        .login_as("op@acme.example", None, Some((acme, "operator")))
        .await;
    let viewer = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
        .await;

    let make = |slug: &'static str| {
        let h = &h;
        let admin = &admin;
        async move {
            let body = format!(r#"{{"name":"{slug}","slug":"{slug}"}}"#);
            let (_, made) = h.post("/v1/agents", Some(admin), &body).await;
            serde_json::from_str::<serde_json::Value>(&made).unwrap()["id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap()
        }
    };
    let billing = make("billing").await;
    let support = make("support").await;
    // Accepts a session and then fails its first turn, so it is offered to
    // nobody, whoever they are.
    let retired = make("retired").await;
    let (status, body) = h
        .send(
            Request::builder()
                .method("PATCH")
                .uri(format!("/v1/agents/{retired}"))
                .header("authorization", format!("Bearer {admin}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"enabled":false}"#))
                .expect("request"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let chat_with = |token: String| {
        let h = &h;
        async move {
            let (status, body) = h.get("/v1/agents", Some(&token)).await;
            assert_eq!(status, StatusCode::OK, "body: {body}");
            let listed = items(&body);
            // The whole roster either way: narrowing hides conversations, not
            // which agents exist.
            assert_eq!(listed.len(), 3);
            listed
                .iter()
                .filter(|a| a["can_chat"] == true)
                .map(|a| a["id"].as_str().unwrap().parse::<Uuid>().unwrap())
                .collect::<Vec<_>>()
        }
    };

    // What the roster offers is what starting a conversation allows, agent by
    // agent -- the point of the field is that the two cannot disagree. Asked
    // before narrowing and again after, since each refuses for its own reason.
    let agrees = |token: String| {
        let h = &h;
        let chat_with = &chat_with;
        async move {
            let offered = chat_with(token.clone()).await;
            for agent in [billing, support, retired] {
                let body = format!(r#"{{"agent_id":"{agent}","title":"t"}}"#);
                let (status, made) = h.post("/v1/agent-sessions", Some(&token), &body).await;
                assert_eq!(
                    status == StatusCode::CREATED,
                    offered.contains(&agent),
                    "agent {agent}: offered {}, starting one answered {status}: {made}",
                    offered.contains(&agent),
                );
            }
        }
    };
    agrees(operator.clone()).await;

    let mut both = chat_with(operator.clone()).await;
    both.sort();
    let mut expected = vec![billing, support];
    expected.sort();
    assert_eq!(both, expected, "unnarrowed, every enabled agent is offered");

    let op_id: Uuid =
        sqlx::query_scalar("select user_id from user_identities where provider_subject = $1")
            .bind("op@acme.example")
            .fetch_one(&h.db.pool)
            .await
            .expect("the operator's account");
    h.scopes
        .set(acme, op_id, &[billing])
        .await
        .expect("set scope");
    assert_eq!(chat_with(operator.clone()).await, vec![billing]);
    agrees(operator).await;

    // Somebody who may not start conversations at all is offered none.
    assert!(chat_with(viewer).await.is_empty());
}

/// A running turn trades its gateway token in before it runs out.
///
/// A turn has no upper bound while it works -- onboarding in long-horizon mode
/// can run for hours -- and a five-minute token failed an eight-minute turn on
/// its next `fetch_url`. The runtime now asks for a fresh one once less than
/// fifteen minutes remain, and these are the rules the API holds it to.
#[tokio::test]
async fn a_running_turn_can_refresh_its_gateway_token() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    let session_id: Uuid = session["id"].as_str().expect("id").parse().expect("uuid");

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
    let job = assignment["job_id"].as_str().expect("job").to_string();
    let lease = assignment["lease_token"]
        .as_str()
        .expect("lease")
        .to_string();
    let original = assignment["gateway_token"]
        .as_str()
        .expect("token")
        .to_string();
    assert!(
        assignment["gateway_token_expires_at"].is_string(),
        "the runtime is told when its token runs out: {body}"
    );

    let refresh = |lease: String, token: String| {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/v1/work/{job}/token"))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {runtime}"))
            .header(outturn::api::work::LEASE_HEADER, lease)
            .body(Body::from(
                serde_json::json!({ "gateway_token": token }).to_string(),
            ))
            .expect("request");
        h.send(req)
    };

    // The ordinary case: the new token says exactly what the old one did.
    let (status, body) = refresh(lease.clone(), original.clone()).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let refreshed: serde_json::Value = serde_json::from_str(&body).expect("refreshed");
    let before = h.gateway_validator.validate(&original).expect("original");
    let after = h
        .gateway_validator
        .validate(refreshed["gateway_token"].as_str().expect("token"))
        .expect("refreshed token is a gateway token");
    assert_eq!(after.subject, before.subject);
    assert_eq!(after.workspace_id, before.workspace_id);
    assert_eq!(
        after.egress_commitment, before.egress_commitment,
        "a reissued token may claim no more reach than the one it replaced"
    );
    assert_eq!(after.gate_commitment, before.gate_commitment);

    // Somebody else's lease: this pod is not running the turn.
    let (status, _) = refresh(Uuid::now_v7().to_string(), original.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A browser token cannot be traded in for a turn token.
    let (status, _) = refresh(lease.clone(), admin.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Another conversation's turn token, under this turn's lease. A runtime
    // holding two turns must not be able to swap one tenant's reach for
    // another's.
    let other = h
        .minter
        .mint_turn(
            Uuid::now_v7(),
            acme,
            before.egress_commitment.expect("egress"),
            before.gate_commitment.expect("gates"),
        )
        .expect("mint");
    let (status, _) = refresh(lease.clone(), other).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // An expired token is refused rather than read. A path honouring expired
    // credentials is the exception that outlives its reason.
    let expired = h
        .minter
        .mint_with_lifetime(
            outturn::auth::AUDIENCE_GATEWAY,
            session_id,
            acme,
            &[Role::Turn.to_string()],
            before.egress_commitment,
            before.gate_commitment,
            std::time::Duration::from_secs(1),
        )
        .expect("mint");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let (status, _) = refresh(lease, expired).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Takes one turn and returns its job id, lease, and session id.
async fn a_leased_turn(h: &Harness) -> (String, String, Uuid, String) {
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    let session_id: Uuid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.post(
        &format!("/v1/agent-sessions/{session_id}/messages"),
        Some(&admin),
        r#"{"content":"hello"}"#,
    )
    .await;
    let runtime = h.runtime_token(acme);
    let (_, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    let a: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    (
        a["job_id"].as_str().unwrap().to_string(),
        a["lease_token"].as_str().unwrap().to_string(),
        session_id,
        runtime,
    )
}

/// Reports a turn over a stream that stays open while the lease is taken away,
/// then sends `last` (or nothing) and closes -- the shape of a real turn whose
/// pod stalled long enough to be reaped.
async fn report_while_the_lease_is_lost(h: &Harness, last: Option<&'static str>) -> StatusCode {
    let (job, lease, _, runtime) = a_leased_turn(h).await;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
        .expect("request");

    let pool = h.db.pool.clone();
    let job_id: Uuid = job.parse().unwrap();
    let feed = async move {
        // Past the endpoint's upfront lease check, then reaped and handed on.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        sqlx::query("update jobs set lease_token = $2 where id = $1")
            .bind(job_id)
            .bind(Uuid::now_v7())
            .execute(&pool)
            .await
            .expect("hand the lease on");
        if let Some(line) = last {
            tx.send(Ok(bytes::Bytes::from(format!("{line}\n"))))
                .await
                .ok();
        }
        drop(tx);
    };
    let ((status, _), ()) = tokio::join!(h.send(req), feed);
    status
}

/// A turn that finished after its lease was handed on is refused, not a fault.
///
/// Found on a real session: the pod stalled, was reaped, and reported its
/// result once the model finally answered -- and got a 500, which reads as the
/// API being broken rather than as "this turn is someone else's now".
#[tokio::test]
async fn a_report_whose_lease_lapsed_midway_is_refused_cleanly() {
    let h = harness_or_skip!();
    let status = report_while_the_lease_is_lost(
        &h,
        Some(r#"{"kind":"done","content":"late","prompt_tokens":1,"completion_tokens":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// The same, where the turn failed rather than finished -- and nothing is said
/// about the failure, because the session is now another pod's to speak for.
#[tokio::test]
async fn a_failed_turn_whose_lease_lapsed_says_nothing_to_the_session() {
    let h = harness_or_skip!();
    let before: i64 = sqlx::query_scalar("select count(*) from events where kind = 'chat.error'")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");

    let status = report_while_the_lease_is_lost(&h, None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let after: i64 = sqlx::query_scalar("select count(*) from events where kind = 'chat.error'")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        after, before,
        "a pod that no longer holds the turn told its reader it had failed"
    );
}

// --- sleep and timers -----------------------------------------------------
//
// An agent asking to be woken later. See `api::wake`.

/// A turn running for this conversation, leased as a runtime would hold it.
async fn a_running_turn(h: &Harness, workspace: Uuid, agent: Uuid, session: Uuid) -> (Uuid, Uuid) {
    outturn::jobs::enqueue(
        &h.db.pool,
        workspace,
        "chat.turn",
        serde_json::json!({
            "workspace_id": workspace, "session_id": session,
            "agent_id": agent, "message_id": Uuid::now_v7(),
        }),
        None,
        Some(&session.to_string()),
        outturn::jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &["chat.turn"],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    let job = &claimed[0].job;
    (job.id, job.lease_token.expect("lease"))
}

async fn ask_to_wait(h: &Harness, lease: Uuid, body: Value) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/work/wait")
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    h.send(req).await
}

/// Puts a conversation to sleep for ten minutes, the turn that asked finished.
async fn asleep(h: &Harness, workspace: Uuid, agent: Uuid, session: Uuid) {
    let (job, lease) = a_running_turn(h, workspace, agent, session).await;
    let (status, body) = ask_to_wait(
        h,
        lease,
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "sleep",
            "seconds": 600, "reason": "Waiting for the deposit to clear",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    outturn::jobs::complete(&h.db.pool, job, Some(lease))
        .await
        .expect("complete");
}

async fn notes(h: &Harness, session: Uuid) -> Vec<(Uuid, Value)> {
    sqlx::query_as(
        "select id, metadata from agent_messages \
         where session_id = $1 and metadata ? 'wake' order by id",
    )
    .bind(session)
    .fetch_all(&h.db.pool)
    .await
    .expect("notes")
}

#[tokio::test]
async fn a_sleep_holds_the_conversation_and_asks_its_owner() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    asleep(&h, workspace, agent, session).await;

    let held: Vec<String> = sqlx::query_scalar(
        "select strength from inhibitors where session_id = $1 and held_by = 'sleep'",
    )
    .bind(session)
    .fetch_all(&h.db.pool)
    .await
    .expect("holds");
    assert_eq!(held, vec!["suspended".to_string()]);

    // Served with the transcript, so a tab opened now shows it asleep.
    let (_, body) = get_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/messages"),
        &admin,
    )
    .await;
    let history: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        history["asleep"]["reason"], "Waiting for the deposit to clear",
        "{body}"
    );

    let (_, body) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let queue: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(queue["items"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(queue["items"][0]["kind"], "sleep");
    assert_eq!(
        queue["items"][0]["payload"]["reason"],
        "Waiting for the deposit to clear"
    );

    // Due in ten minutes rather than now.
    let later: bool = sqlx::query_scalar(
        "select run_after > now() + interval '9 minutes' from jobs where kind = 'chat.wake'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("wake job");
    assert!(later, "the wakeup is not due when the sleep ends");

    // Not answerable as an approval: that would lift the hold with no note.
    let item = queue["items"][0]["id"].as_str().unwrap();
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // And one sleep at a time.
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let (status, body) = ask_to_wait(
        &h,
        lease,
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "sleep",
            "seconds": 60, "reason": "again",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    h.db.cleanup().await;
}

/// The runtime speaks for the turn it holds, and only that one.
#[tokio::test]
async fn a_wait_is_refused_for_a_turn_the_runtime_does_not_hold() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let ask = |session: Uuid, seconds: u64| {
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "sleep",
            "seconds": seconds, "reason": "r",
        })
    };

    let (status, _) = ask_to_wait(&h, Uuid::now_v7(), ask(session, 60)).await;
    assert_eq!(status, StatusCode::CONFLICT, "a lease it does not hold");
    let (status, _) = ask_to_wait(&h, lease, ask(Uuid::now_v7(), 60)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "another conversation");
    let (status, _) = ask_to_wait(&h, lease, ask(session, 0)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "no time at all");
    let (status, _) = ask_to_wait(&h, lease, ask(session, 31 * 86_400)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "past the longest");

    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0, "a refused sleep held the conversation anyway");

    h.db.cleanup().await;
}

/// The case the design is for: messages sent while the agent slept are
/// answered by the wake, together, and the note comes after them.
#[tokio::test]
async fn waking_answers_what_arrived_meanwhile_in_one_turn() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    asleep(&h, workspace, agent, session).await;

    for text in ["any news?", "hello?"] {
        let (status, body) = post_with_cookie(
            &h,
            &format!("/v1/agent-sessions/{session}/messages"),
            &admin,
            &serde_json::json!({ "content": text }).to_string(),
        )
        .await;
        assert!(status.is_success(), "{status}: {body}");
    }

    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/wake"),
        &admin,
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["woke"], true);

    let written = notes(&h, session).await;
    assert_eq!(written.len(), 1);
    let (note, metadata) = &written[0];
    assert_eq!(metadata["wake"]["kind"], "sleep");
    assert_eq!(metadata["wake"]["woken_by"], "Test User");
    assert_eq!(metadata["wake"]["answers"].as_array().unwrap().len(), 2);

    // Both taken by the note, which sorts after them.
    let absorbed: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "select id, absorbed_by from agent_messages \
         where session_id = $1 and content in ('any news?', 'hello?') order by id",
    )
    .bind(session)
    .fetch_all(&h.db.pool)
    .await
    .expect("messages");
    assert_eq!(absorbed.len(), 2);
    for (id, by) in &absorbed {
        assert_eq!(*by, Some(*note));
        assert!(id < note, "the note was written before what it answers");
    }

    // One turn answers the note, ahead of background work, and the hold is gone.
    let priority: i32 = sqlx::query_scalar(
        "select priority from jobs where kind = 'chat.turn' and payload->>'message_id' = $1",
    )
    .bind(note.to_string())
    .fetch_one(&h.db.pool)
    .await
    .expect("wake turn");
    assert_eq!(priority, outturn::jobs::PRIORITY_REALTIME);
    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0);

    // The wakeup then comes due and finds nothing to do.
    sqlx::query("update jobs set run_after = now() where kind = 'chat.wake'")
        .execute(&h.db.pool)
        .await
        .expect("due");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::wake::WAKE],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    let actions = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    outturn::api::wake::fire(&h.db.pool, &actions, &claimed[0].job)
        .await
        .expect("fire");
    assert_eq!(notes(&h, session).await.len(), 1, "woken twice");

    // As does a second press.
    let (_, body) = post_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/wake"),
        &admin,
        "{}",
    )
    .await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["woke"], false);

    h.db.cleanup().await;
}

/// Coming due on its own, with nobody waiting, a sleep wakes as background work.
#[tokio::test]
async fn a_sleep_that_runs_its_course_wakes_on_its_own() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    asleep(&h, workspace, agent, session).await;

    sqlx::query("update jobs set run_after = now() where kind = 'chat.wake'")
        .execute(&h.db.pool)
        .await
        .expect("due");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::wake::WAKE],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    let actions = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    outturn::api::wake::fire(&h.db.pool, &actions, &claimed[0].job)
        .await
        .expect("fire");

    let written = notes(&h, session).await;
    assert_eq!(written.len(), 1);
    assert!(written[0].1["wake"]["woken_by"].is_null());
    let priority: i32 = sqlx::query_scalar(
        "select priority from jobs where kind = 'chat.turn' and payload->>'message_id' = $1",
    )
    .bind(written[0].0.to_string())
    .fetch_one(&h.db.pool)
    .await
    .expect("wake turn");
    assert_eq!(priority, outturn::jobs::PRIORITY_BACKGROUND);

    let (_, body) = get_with_cookie(&h, "/v1/action-items/count", &admin).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["count"], 0);

    h.db.cleanup().await;
}

/// A timer pauses nothing, so it holds nothing and answers nothing on
/// anybody's behalf: it only starts a turn when it fires.
#[tokio::test]
async fn a_timer_holds_nothing_and_fires_a_turn() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let at = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let (status, body) = ask_to_wait(
        &h,
        lease,
        serde_json::json!({
            "job_id": job, "session_id": session, "kind": "timer",
            "at": at, "reason": "Check the booking was confirmed",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    outturn::jobs::complete(&h.db.pool, job, Some(lease))
        .await
        .expect("complete");

    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0, "a timer held the conversation");

    let (status, _) = post_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/messages"),
        &admin,
        r#"{"content":"unrelated"}"#,
    )
    .await;
    assert!(status.is_success());

    sqlx::query("update jobs set run_after = now() where kind = 'chat.wake'")
        .execute(&h.db.pool)
        .await
        .expect("due");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &[outturn::api::wake::WAKE],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    let actions = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    outturn::api::wake::fire(&h.db.pool, &actions, &claimed[0].job)
        .await
        .expect("fire");

    let written = notes(&h, session).await;
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].1["wake"]["kind"], "timer");
    let absorbed: Option<Uuid> =
        sqlx::query_scalar("select absorbed_by from agent_messages where content = 'unrelated'")
            .fetch_one(&h.db.pool)
            .await
            .expect("message");
    assert_eq!(
        absorbed, None,
        "a timer answered a message that has a turn of its own"
    );

    h.db.cleanup().await;
}

/// Setting a timer never replaces one, so the agent is shown the others and can
/// cancel what it no longer wants -- and only in its own conversation.
#[tokio::test]
async fn timers_can_be_listed_and_cancelled_and_only_here() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let (job, lease) = a_running_turn(&h, workspace, agent, session).await;
    let ask = |body: Value| ask_to_wait(&h, lease, body);
    let answer = |body: &str| -> Value {
        let message: Value = serde_json::from_str(body).expect("json");
        serde_json::from_str(message["message"].as_str().expect("message")).expect("inner json")
    };

    let (status, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "timer",
        "seconds": 3600, "reason": "first",
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let first = answer(&body);
    assert!(first["other_timers"].as_array().unwrap().is_empty());
    assert!(first["when"].as_str().unwrap().contains(" at "), "{first}");

    let (_, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "timer",
        "seconds": 7200, "reason": "second",
    }))
    .await;
    let second = answer(&body);
    let others = second["other_timers"].as_array().unwrap();
    assert_eq!(
        others.len(),
        1,
        "the first timer was not mentioned: {second}"
    );
    assert_eq!(others[0]["id"], first["id"]);

    let (_, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "list",
    }))
    .await;
    assert_eq!(answer(&body)["timers"].as_array().unwrap().len(), 2);

    // Another conversation's timer is not this one's to cancel.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/agent-sessions",
        &admin,
        &serde_json::json!({ "agent_id": agent, "title": "other" }).to_string(),
    )
    .await;
    let elsewhere: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (other_job, other_lease) = a_running_turn(&h, workspace, agent, elsewhere).await;
    let (status, _) = ask_to_wait(
        &h,
        other_lease,
        serde_json::json!({
            "job_id": other_job, "session_id": elsewhere, "kind": "cancel", "id": first["id"],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "cancel", "id": first["id"],
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(answer(&body)["cancelled"]["reason"], "first");

    let (_, body) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "list",
    }))
    .await;
    let left = answer(&body);
    assert_eq!(left["timers"].as_array().unwrap().len(), 1);
    assert_eq!(left["timers"][0]["reason"], "second");

    // One already firing is past cancelling: its note is on its way.
    let second_id: Uuid = left["timers"][0]["id"].as_str().unwrap().parse().unwrap();
    sqlx::query("update jobs set state = 'running' where id = $1")
        .bind(second_id)
        .execute(&h.db.pool)
        .await
        .expect("firing");
    let (status, _) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "cancel", "id": second_id,
    }))
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a firing timer was cancelled"
    );

    // Twice is not found, rather than a second success.
    let (status, _) = ask(serde_json::json!({
        "job_id": job, "session_id": session, "kind": "cancel", "id": first["id"],
    }))
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    h.db.cleanup().await;
}

/// A page says there is another only when there is, and walking `next` visits
/// every row once. `next` used to be set on any page that had rows, so the last
/// page always sent a client back for an empty one.
#[tokio::test]
async fn a_list_says_there_is_more_only_when_there_is() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    for host in ["a.example", "b.example", "c.example"] {
        let (status, body) = h
            .post(
                "/v1/egress-rules",
                Some(&token),
                &serde_json::json!({ "host": host }).to_string(),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    let mut pages = 0;
    loop {
        let uri = match &after {
            Some(a) => format!("/v1/egress-rules?limit=2&after={a}"),
            None => "/v1/egress-rules?limit=2".to_string(),
        };
        let (_, body) = h.get(&uri, Some(&token)).await;
        let page: Value = serde_json::from_str(&body).expect("page");
        seen.extend(items(&body).into_iter().map(|r| r["host"].clone()));
        pages += 1;
        match page["next"].as_str() {
            Some(next) => after = Some(next.to_string()),
            None => break,
        }
        assert!(pages < 5, "paging never ended: {body}");
    }
    assert_eq!(
        pages, 2,
        "two pages of two for three rows, and no empty third"
    );
    assert_eq!(seen.len(), 3);

    // Exactly a page's worth is one page, not one and an empty one.
    let (_, body) = h.get("/v1/egress-rules?limit=3", Some(&token)).await;
    let page: Value = serde_json::from_str(&body).expect("page");
    assert_eq!(items(&body).len(), 3);
    assert!(
        page["next"].is_null(),
        "a full last page pointed past itself: {body}"
    );
}

/// The platform's own row is not a workspace to manage: it is not listed, and
/// it cannot be opened, renamed or deleted as one. Deleting it was refused
/// before this, by a trigger, as an internal error.
#[tokio::test]
async fn the_platform_workspace_is_not_managed_as_a_workspace() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("root@test.invalid", Some(Role::SystemAdmin), None)
        .await;
    let platform = outturn::api::usage::PLATFORM_WORKSPACE;

    let (status, body) = h.get("/v1/workspaces", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Vec<String> = items(&body)
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed, vec![acme.to_string()], "{body}");

    let (status, _) = h
        .get(&format!("/v1/workspaces/{platform}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/workspaces/{platform}"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"Renamed"}"#))
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the platform workspace was renamed"
    );
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/workspaces/{platform}"))
        .header("authorization", format!("Bearer {admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A real workspace still opens.
    let (status, _) = h.get(&format!("/v1/workspaces/{acme}"), Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);

    h.db.cleanup().await;
}

/// An approval says what is being approved: each field the grant will be keyed
/// on, with the value this request carries. A person used to be asked to
/// approve a charge and shown only the path it was posted to -- never its
/// amount -- so saying yes was saying yes to a number they had not seen.
///
/// Also the first test through `/v1/work/gated` itself, the way a runtime
/// reports a refusal.
#[tokio::test]
async fn an_approval_shows_the_values_it_approves() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let charge_file = "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [booking_id, amount_pence]\n---\n\n# charge\n";
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "charging",
            "name": "Charging",
            "body": "Read charge.md before charging anybody.",
            "hosts": ["api.hollowbrook.test"],
            "files": [{ "path": "charge.md", "content": charge_file }],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) =
        post_with_cookie(&h, &format!("/v1/skills/{skill}/hosts/approve"), &admin, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (agent, session) = a_held_conversation(&h, acme, &admin).await;
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent}/skills"))
        .header("cookie", format!("outturn_session={admin}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{skill}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The runtime reports the refusal, as it does when the gateway turns the
    // charge away.
    let (job, lease) = a_running_turn(&h, acme, agent, session).await;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/work/gated")
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "session_id": session,
                "job_id": job,
                "method": "POST",
                "host": "api.hollowbrook.test",
                "path": "/charges",
                "body": r#"{"amount_pence":78000,"booking_id":"bk_1","note":"not bound"}"#,
            })
            .to_string(),
        ))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let expected = serde_json::json!([
        { "field": "booking_id", "value": "bk_1" },
        { "field": "amount_pence", "value": 78000 },
    ]);

    // In the queue, where the inbox reads it.
    let (_, body) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let queue = items(&body);
    assert_eq!(queue.len(), 1, "{body}");
    assert_eq!(queue[0]["payload"]["binds"], expected, "{body}");

    // And with the conversation, where the banner reads it.
    let (_, body) = get_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/messages"),
        &admin,
    )
    .await;
    let history: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(history["awaiting"]["binds"], expected, "{body}");

    h.db.cleanup().await;
}
