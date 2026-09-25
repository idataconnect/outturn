use std::sync::Arc;

use axum::Router;
use axum::http::{Method, header};
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing_subscriber::EnvFilter;

use outturn::api::agent::{AgentStore, PostgresAgentStore};
use outturn::api::chat::{ChatStore, PostgresChatStore};
use outturn::api::role::{PostgresRoleStore, RoleStore};
use outturn::api::session::{PostgresSessionStore, SessionStore};
use outturn::api::settings::{PostgresSettingsStore, SettingsStore};
use outturn::api::usage::{PostgresUsageStore, UsageStore};
use outturn::api::user::{PostgresUserStore, UserStore};
use outturn::api::worker::Worker;
use outturn::api::workspace::{PostgresWorkspaceStore, WorkspaceStore};
use outturn::api::{self, ApiState, seed};
use outturn::auth::{TokenMinter, TokenValidator};
use outturn::db;
use outturn::events::EventBus;
use outturn::lifecycle::{self, Health};
use outturn::runtime::storage::{S3Storage, StorageBackend};

/// Credentialed requests cannot use a wildcard origin, so allowed origins are
/// listed explicitly. OUTTURN_CORS_ORIGINS is a comma-separated list; the
/// default covers the Vite dev server.
fn cors() -> CorsLayer {
    let origins =
        std::env::var("OUTTURN_CORS_ORIGINS").unwrap_or_else(|_| "http://localhost:3000".into());

    let parsed: Vec<_> = origins
        .split(',')
        .filter_map(|o| o.trim().parse().ok())
        .collect();

    // Methods and headers must be listed rather than Any: the wildcard is
    // rejected for credentialed requests.
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(parsed))
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let health = Health::new();

    let pool = db::connect_from_env().await.expect("database connection");
    tracing::info!("connected to database");

    // The API owns the schema: it is the only service that migrates.
    db::migrate(&pool).await.expect("migrations");
    tracing::info!("migrations applied");

    let workspaces: Arc<dyn WorkspaceStore> = Arc::new(PostgresWorkspaceStore::new(pool.clone()));
    let users: Arc<dyn UserStore> = Arc::new(PostgresUserStore::new(pool.clone()));
    let sessions: Arc<dyn SessionStore> = Arc::new(PostgresSessionStore::new(pool.clone()));
    let agents: Arc<dyn AgentStore> = Arc::new(PostgresAgentStore::new(pool.clone()));
    let chat: Arc<dyn ChatStore> = Arc::new(PostgresChatStore::new(pool.clone()));
    // Roles are resolved on every request and cached per workspace; the listener
    // is what makes an edit on one pod reach the cache on every other.
    let role_store = PostgresRoleStore::new(pool.clone());
    role_store.spawn_invalidation();
    let roles: Arc<dyn RoleStore> = Arc::new(role_store);
    // The same for agent scopes. The store handed to the state is the one the
    // listener clears, which is the whole point: a second store would hold a
    // second cache that nothing ever invalidated.
    let scope_store = outturn::api::scope::PostgresScopeStore::new(pool.clone());
    scope_store.spawn_invalidation();
    let scopes: Arc<dyn outturn::api::scope::ScopeStore> = Arc::new(scope_store);
    let usage: Arc<dyn UsageStore> = Arc::new(PostgresUsageStore::new(pool.clone()));
    let settings: Arc<dyn SettingsStore> = Arc::new(PostgresSettingsStore::new(pool.clone()));

    // The bucket the runtime uses, so uploads land where the agent looks. No
    // in-memory fallback here: an upload that only this pod could see would
    // be a file the agent cannot find, which is worse than an honest 503.
    let storage: Option<Arc<dyn StorageBackend>> = match std::env::var("OUTTURN_S3_ENDPOINT") {
        Ok(endpoint) => {
            let bucket = std::env::var("OUTTURN_S3_BUCKET").unwrap_or_else(|_| "outturn".into());
            match S3Storage::new(
                &endpoint,
                &bucket,
                &std::env::var("OUTTURN_S3_ACCESS_KEY").unwrap_or_default(),
                &std::env::var("OUTTURN_S3_SECRET_KEY").unwrap_or_default(),
                String::new(),
            ) {
                Ok(store) => {
                    tracing::info!(endpoint, bucket, "object storage enabled for uploads");
                    Some(Arc::new(store))
                }
                Err(e) => {
                    tracing::error!(error = %e, "object storage misconfigured; uploads disabled");
                    None
                }
            }
        }
        Err(_) => {
            tracing::warn!("no OUTTURN_S3_ENDPOINT; uploads disabled");
            None
        }
    };

    // Read once at boot so a template naming something this build cannot honour
    // is complained about here, rather than at whatever hour the next workspace
    // happens to be created.
    match roles.templates().await {
        Ok(t) => tracing::info!(count = t.len(), "role templates loaded"),
        Err(e) => tracing::error!(error = %e, "could not read role templates"),
    }

    let skills: Arc<dyn outturn::api::skill::SkillStore> =
        Arc::new(outturn::api::skill::PostgresSkillStore::new(pool.clone()));

    seed::dev_seed(&users, &workspaces, &roles)
        .await
        .expect("dev seed");

    let validator = TokenValidator::from_env(outturn::auth::AUDIENCE_API).expect("token validator");
    let minter = TokenMinter::from_env().expect("token minter");
    // A second one for the namer, which mints its own gateway tokens the
    // way a turn's are minted.
    let namer_minter = Arc::new(TokenMinter::from_env().expect("token minter"));
    let runtime_key = outturn::auth::RuntimeKey::from_env().expect("runtime key");

    // One LISTEN connection fans out to every parked long poll.
    let bus = EventBus::spawn(pool.clone());

    let storage_for_extraction = storage.clone();

    let state = Arc::new(ApiState::new(
        workspaces,
        users,
        sessions,
        agents.clone(),
        skills.clone(),
        chat.clone(),
        roles,
        usage.clone(),
        settings.clone(),
        scopes,
        storage,
        validator,
        minter,
        runtime_key,
        pool.clone(),
        bus,
        health.shutdown_signal(),
    ));

    // Turns are prepared and recorded here, and run by whichever runtime asks
    // for one. Nothing is pushed: a runtime with room comes and takes work, so
    // this tier never has to guess which pod could have taken it.
    let worker = Arc::new(Worker {
        pool: pool.clone(),
        agents: agents.clone(),
        skills: skills.clone(),
        chat: chat.clone(),
        usage,
        settings,
        inhibitors: Arc::new(outturn::api::inhibitor::PostgresInhibitorStore::new(
            pool.clone(),
        )),
        // A summary is a model call, so it needs both a way to reach the
        // gateway and a token to present. Either missing means the trim
        // does the work alone, which is what it is for.
        // Its own, like the namer's: the one above is moved into the API
        // state, and a second is cheaper than restructuring what holds it.
        minter: Some(Arc::new(TokenMinter::from_env().expect("token minter"))),
        gateway_url: std::env::var("OUTTURN_GATEWAY_URL").ok(),
    });
    // A turn is claimed here and reported by whichever runtime ran it, and
    // nothing joins those but a lease. When a runtime dies mid-turn the job is
    // returned to the queue by this rather than by the pod that vanished.
    Arc::clone(&worker).spawn_reaper(health.shutdown_signal());

    // Turns that start because the clock said so. Runs in this tier rather
    // than the runtime, because it creates work rather than doing it -- and it
    // is safe in more than one pod: a due schedule is taken with `for update
    // skip locked`, so two loops ticking together cannot both fire it.
    tokio::spawn(outturn::api::schedule::worker::run(
        pool.clone(),
        health.shutdown_signal(),
    ));

    // Documents become readable in the background, where they are configured
    // to. Absent OUTTURN_TIKA_URL this logs once and does nothing further.
    if let Some(store) = storage_for_extraction.clone() {
        outturn::api::extract::spawn(pool.clone(), store, health.shutdown_signal());
    }
    // Unnamed conversations are given a title after their first turn, by a
    // model reached through the gateway. Absent OUTTURN_GATEWAY_URL this
    // logs once and sessions stay "New Session" until somebody names them.
    // Not fatal: an agent that names its model runs without it. But every
    // agent that does not will fail its turns, and nothing is named, so the
    // one place an operator is sure to look is told at once.
    if std::env::var("OUTTURN_DEFAULT_MODEL").is_err() {
        tracing::warn!(
            "OUTTURN_DEFAULT_MODEL is not set: agents that name no model will refuse \
             their turns, and sessions will not be named"
        );
    }

    outturn::api::naming::spawn(
        pool.clone(),
        chat.clone(),
        namer_minter,
        health.shutdown_signal(),
    );
    state.set_worker(worker);

    let app = Router::new()
        .merge(lifecycle::routes(health.clone()))
        .merge(api::routes(state))
        .layer(cors());

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = TcpListener::bind(&addr).await.unwrap();
    tracing::info!("listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(lifecycle::shutdown_signal(health))
        .await
        .unwrap();
    tracing::info!("shutdown complete");
}
