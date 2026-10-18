use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use outturn_core::auth::TokenValidator;
use outturn_core::lifecycle::{self, Health};
use outturn_gateway as gateway;
use outturn_gateway::llm::provider::LlmProvider;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let health = Health::new();
    let auth = TokenValidator::from_env(outturn_core::auth::AUDIENCE_GATEWAY)
        .expect("failed to initialize token validator");

    // Every model the gateway asks is configured in the database: a chain of
    // models at provider accounts, per class of traffic (docs/models.md).
    // Nothing is configured here, and a class no chain answers is refused
    // rather than sent to a provider nobody chose for it.
    if std::env::var("OPENAI_BASE_URL").is_ok() {
        tracing::warn!(
            "OPENAI_BASE_URL is set and no longer read: give the model an account and a chain on \
             Platform -> Models instead"
        );
    }
    let providers: Vec<Arc<dyn LlmProvider>> = Vec::new();

    // The breaker is shared state, so it needs the database. Optional on
    // purpose: without DATABASE_URL the gateway behaves exactly as it did
    // before the breaker existed, calling every provider in turn. A gateway
    // that refused to start without Postgres would make the credential tier
    // depend on the storage tier for no gain.
    let mut state = gateway::GatewayState::new(providers, auth);
    match outturn_core::db::connect_from_env().await {
        Ok(pool) => {
            tracing::info!("provider circuit breaker enabled");
            state = state.with_health(pool);
        }
        Err(e) => tracing::warn!(error = %e, "no database; provider health is not shared"),
    }
    let state = Arc::new(state);

    let app = Router::new()
        .merge(gateway::routes(state))
        .merge(lifecycle::routes(health.clone()));

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".into());
    let listener = TcpListener::bind(&addr).await.unwrap();
    tracing::info!("listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(lifecycle::shutdown_signal(health))
        .await
        .unwrap();
    tracing::info!("shutdown complete");
}
