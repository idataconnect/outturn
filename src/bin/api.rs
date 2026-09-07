use std::sync::Arc;

use axum::Router;
use axum::http::{Method, header};
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing_subscriber::EnvFilter;

use outturn::api::tenant::{PostgresTenantStore, TenantStore};
use outturn::api::agent::{AgentStore, PostgresAgentStore};
use outturn::api::chat::{ChatStore, PostgresChatStore};
use outturn::api::role::{PostgresRoleStore, RoleStore};
use outturn::api::usage::{PostgresUsageStore, UsageStore};
use outturn::api::settings::{PostgresSettingsStore, SettingsStore};
use outturn::api::worker::Worker;
use outturn::api::session::{PostgresSessionStore, SessionStore};
use outturn::api::user::{PostgresUserStore, UserStore};
use outturn::api::{self, ApiState, seed};
use outturn::auth::{TokenMinter, TokenValidator};
use outturn::db;
use outturn::events::EventBus;
use outturn::lifecycle::{self, Health};

/// Credentialed requests cannot use a wildcard origin, so allowed origins are
/// listed explicitly. OUTTURN_CORS_ORIGINS is a comma-separated list; the
/// default covers the Vite dev server.
fn cors() -> CorsLayer {
    let origins = std::env::var("OUTTURN_CORS_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:3000".into());

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

    let tenants: Arc<dyn TenantStore> = Arc::new(PostgresTenantStore::new(pool.clone()));
    let users: Arc<dyn UserStore> = Arc::new(PostgresUserStore::new(pool.clone()));
    let sessions: Arc<dyn SessionStore> = Arc::new(PostgresSessionStore::new(pool.clone()));
    let agents: Arc<dyn AgentStore> = Arc::new(PostgresAgentStore::new(pool.clone()));
    let chat: Arc<dyn ChatStore> = Arc::new(PostgresChatStore::new(pool.clone()));
    // Roles are resolved on every request and cached per tenant; the listener
    // is what makes an edit on one pod reach the cache on every other.
    let role_store = PostgresRoleStore::new(pool.clone());
    role_store.spawn_invalidation();
    let roles: Arc<dyn RoleStore> = Arc::new(role_store);
    let usage: Arc<dyn UsageStore> = Arc::new(PostgresUsageStore::new(pool.clone()));
    let settings: Arc<dyn SettingsStore> = Arc::new(PostgresSettingsStore::new(pool.clone()));

    seed::dev_seed(&users, &tenants, &roles).await.expect("dev seed");

    let validator = TokenValidator::from_env(outturn::auth::AUDIENCE_API).expect("token validator");
    let minter = TokenMinter::from_env().expect("token minter");
    let runtime_key = outturn::auth::RuntimeKey::from_env().expect("runtime key");

    // One LISTEN connection fans out to every parked long poll.
    let bus = EventBus::spawn(pool.clone());

    let state = Arc::new(ApiState::new(
        tenants,
        users,
        sessions,
        agents.clone(),
        chat.clone(),
        roles,
        usage.clone(),
        settings.clone(),
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
        chat: chat.clone(),
        usage,
        settings,
    });
    // A turn is claimed here and reported by whichever runtime ran it, and
    // nothing joins those but a lease. When a runtime dies mid-turn the job is
    // returned to the queue by this rather than by the pod that vanished.
    Arc::clone(&worker).spawn_reaper(health.shutdown_signal());
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
