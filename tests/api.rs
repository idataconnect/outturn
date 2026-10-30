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
    /// What the app was built from, for a test that waits on its buses.
    state: Arc<ApiState>,
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
    /// The key both validate with, for a test standing up a gateway of its own
    /// that has to accept the turn tokens this app mints.
    public_key: [u8; 32],
    roles: Arc<dyn outturn::api::role::RoleStore>,
    /// The same instance the app holds. A second store would have a second
    /// cache, and without the listener a test runs without, writing through
    /// one would leave the other answering from before the write.
    scopes: Arc<dyn outturn::scope::ScopeStore>,
    /// The object store the app and its worker share, for a test that runs a
    /// sweep as the worker would.
    storage: Arc<dyn outturn::storage::StorageBackend>,
}

/// What the test's pretend runtime presents to the work endpoints.
const TEST_RUNTIME_KEY: &str = "test-runtime-key-test-runtime-key-test-runtime-key";

/// The gateway's seal key in these tests. Not a secret: a test's.
const SEAL_KEY: &str = "0505050505050505050505050505050505050505050505050505050505050505";

/// A chain answering assistant traffic, as every deployment that answers at
/// all has: a turn's model is the first of its class's chain, and a turn
/// with none is refused. Its model is `test-model`, which is what the tests
/// that read a turn's model expect.
async fn seed_assistant_chain(pool: &PgPool) {
    sqlx::raw_sql(
        "insert into provider_accounts (id, workspace_id, name, protocol, base_url) \
             values ('01a00000-0000-7000-8000-000000000001', null, 'Test', 'openai', 'http://models.example.com'); \
         insert into models (id, workspace_id, account_id, name, model) \
             values ('01a00000-0000-7000-8000-000000000002', null, '01a00000-0000-7000-8000-000000000001', 'Test model', 'test-model'); \
         insert into chains (id, workspace_id, name) \
             values ('01a00000-0000-7000-8000-000000000003', null, 'Test'); \
         insert into chain_links (chain_id, position, model_id) \
             values ('01a00000-0000-7000-8000-000000000003', 0, '01a00000-0000-7000-8000-000000000002'); \
         insert into chain_assignments (id, workspace_id, traffic_type, chain_id) \
             values ('01a00000-0000-7000-8000-000000000004', null, 'assistant', '01a00000-0000-7000-8000-000000000003');",
    )
    .execute(pool)
    .await
    .expect("seed the assistant chain");
}

async fn harness() -> Harness {
    harness_parking_for(outturn::api::LONG_POLL).await
}

/// A harness whose long polls park for `park`, for a test about parking: one
/// showing a poll was woken gives it a park nothing reaches, so returning at
/// all is the proof, and one showing a poll answers on timeout gives it a
/// short one instead of sitting through the real 25 seconds.
async fn harness_parking_for(park: std::time::Duration) -> Harness {
    harness_with(park, None, None).await
}

/// A harness whose worker has a gateway to summarize through, for a test
/// about what preparing a turn does while it waits on a model.
async fn harness_with_gateway(gateway_url: &str) -> Harness {
    harness_with(outturn::api::LONG_POLL, Some(gateway_url.to_string()), None).await
}

/// A harness whose uploads convert HEIC photos at `heic_url`.
async fn harness_converting_at(heic_url: &str) -> Harness {
    harness_with(outturn::api::LONG_POLL, None, Some(heic_url.to_string())).await
}

async fn harness_with(
    park: std::time::Duration,
    gateway_url: Option<String>,
    heic_url: Option<String>,
) -> Harness {
    // A private schema per test, so tests do not see each other's rows and can
    // run in parallel.
    let db = common::TestDb::new().await;
    let pool: PgPool = db.pool.clone();
    seed_assistant_chain(&pool).await;

    // One seed, two minters: the app's, and one the test uses to issue the
    // platform's own credentials the way the platform does.
    let seed = {
        use rand::Rng;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
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
    let scopes: Arc<dyn outturn::scope::ScopeStore> =
        Arc::new(outturn::scope::PostgresScopeStore::new(pool.clone()));

    // One store for the API and the worker, as one pod has: a skill file
    // published through the one is what an invoked skill reads through the
    // other.
    let storage: Arc<dyn outturn::storage::StorageBackend> =
        Arc::new(outturn::storage::MemoryStorage::new());
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
        Some(storage.clone()),
        validator,
        minter,
        runtime_key,
        pool.clone(),
        outturn::events::EventBus::spawn(pool.clone()),
        outturn::lifecycle::Shutdown::new(),
    )
    .with_long_poll(park)
    // No converter unless the test brings one, whatever this machine's
    // environment says.
    .with_heic_url(heic_url)
    // The public half of the seal key a test's gateway opens with.
    .with_seal_key(outturn::egress::seal::public_of(SEAL_KEY).expect("seal key"))
    // What the gateway would have bound, so a rule naming these is one the
    // API accepts. Every workspace, because each test makes its own.
    .with_bindings(
        outturn::egress::bindings::Bindings::parse(
            r#"{
                "OUTTURN_EGRESS_BOOKING_CLIENT_ID": {"workspaces": ["*"], "hosts": ["api.example.com"],
                    "token_url": "https://login.example.com/tenant-a/oauth2/token"},
                "OUTTURN_EGRESS_BOOKING_CLIENT_SECRET": {"workspaces": ["*"], "hosts": ["api.example.com"],
                    "token_url": "https://login.example.com/tenant-a/oauth2/token"}
            }"#,
        )
        .expect("readable"),
    ));

    // The endpoints runtimes use need the worker that prepares and records
    // turns, the same as a real API pod.
    state.set_worker(Arc::new(outturn::api::worker::Worker {
        pool: pool.clone(),
        agents: agents.clone(),
        templates: Arc::new(
            outturn::api::agent_template::PostgresAgentTemplateStore::new(pool.clone()),
        ),
        skills: skills.clone(),
        chat: chat.clone(),
        usage: usage.clone(),
        settings: settings.clone(),
        inhibitors: Arc::new(outturn::inhibitor::PostgresInhibitorStore::new(
            pool.clone(),
        )),
        actions: Arc::new(outturn::api::actions::PostgresActionStore::new(
            pool.clone(),
        )),
        // Without a gateway there is no summarizing, and the trim underneath
        // carries the whole of what most tests exercise. The few about what
        // happens while a summary is written point this at a fake one.
        minter: gateway_url
            .as_ref()
            .map(|_| Arc::new(TokenMinter::new(&seed).expect("minter"))),
        gateway_url,
        storage: Some(storage.clone()),
    }));

    Harness {
        app: routes(Arc::clone(&state)),
        state: Arc::clone(&state),
        users,
        workspaces,
        sessions,
        agents,
        skills,
        db,
        minter: test_minter,
        gateway_validator,
        public_key,
        roles,
        scopes,
        storage,
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

    async fn patch(&self, uri: &str, token: Option<&str>, body: &str) -> (StatusCode, String) {
        let mut req = Request::builder()
            .method("PATCH")
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

fn operator(email: &str, slug: &str) -> outturn::api::seed::Operator {
    outturn::api::seed::Operator {
        email: email.into(),
        display_name: "Operator".into(),
        password: "correct-horse".into(),
        organization: "i Data Connect!".into(),
        slug: slug.into(),
    }
}

fn sign(secret: &str, body: &str, at: i64) -> (String, String) {
    use hmac::{Hmac, KeyInit, Mac};
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

async fn send_as(
    h: &Harness,
    method: &str,
    uri: &str,
    token: &str,
    body: &str,
) -> (StatusCode, String) {
    h.send(
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
    )
    .await
}

/// The operator, and a workspace admin, of one workspace.
async fn template_people(h: &Harness) -> (Uuid, String, String) {
    let acme = h.make_workspace("Acme", "acme").await;
    let operator = h
        .login_as(
            "op@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    (acme, operator, admin)
}

async fn take_assignment(h: &Harness) -> (Uuid, Uuid, Value) {
    let (status, body) = h.post("/v1/work", Some(TEST_RUNTIME_KEY), "{}").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let assignment: Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job").parse().unwrap();
    let lease = assignment["lease_token"]
        .as_str()
        .expect("lease")
        .parse()
        .unwrap();
    (job, lease, assignment)
}

// Its own file, sharing this one's harness. Declared last, after the macros
// it uses.
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

#[path = "api/action_queue.rs"]
mod action_queue;
#[path = "api/agents.rs"]
mod agents;
#[path = "api/approvals.rs"]
mod approvals;
#[path = "api/auth.rs"]
mod auth;
#[path = "api/auto_approval.rs"]
mod auto_approval;
#[path = "api/credentials.rs"]
mod credentials;
#[path = "api/egress.rs"]
mod egress;
#[path = "api/files.rs"]
mod files;
#[path = "api/holds.rs"]
mod holds;
#[path = "api/leases.rs"]
mod leases;
#[path = "api/models.rs"]
mod models;
#[path = "api/notifications.rs"]
mod notifications;
#[path = "api/roles.rs"]
mod roles;
#[path = "api/scopes.rs"]
mod scopes;
#[path = "api/sessions.rs"]
mod sessions;
#[path = "api/settings.rs"]
mod settings;
#[path = "api/skills.rs"]
mod skills;
#[path = "api/sleep.rs"]
mod sleep;
#[path = "api/storage_quota.rs"]
mod storage_quota;
#[path = "api/templates.rs"]
mod templates;
#[path = "api/triggers.rs"]
mod triggers;
#[path = "api/turn_failures.rs"]
mod turn_failures;
#[path = "api/usage.rs"]
mod usage;
#[path = "api/users.rs"]
mod users;
#[path = "api/work.rs"]
mod work;
