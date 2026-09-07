use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use outturn::auth::TokenValidator;
use outturn::gateway;
use outturn::gateway::llm::provider::{LlmProvider, mock::MockProvider, openai::OpenAiProvider};
use outturn::lifecycle::{self, Health};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let health = Health::new();
    let auth = TokenValidator::from_env(outturn::auth::AUDIENCE_GATEWAY)
        .expect("failed to initialize token validator");

    let mut providers: Vec<Arc<dyn LlmProvider>> = Vec::new();

    // One protocol, pointed wherever OPENAI_BASE_URL says: api.openai.com in
    // production, a local ollama in development, anything else that speaks the
    // same wire format without needing its own implementation.
    if let Some(openai) = OpenAiProvider::from_env() {
        tracing::info!("openai-protocol provider enabled");
        providers.push(Arc::new(openai));
    }

    if providers.is_empty() {
        tracing::warn!("no real providers configured, falling back to mock");
        providers.push(Arc::new(MockProvider::new()));
    }

    // The breaker is shared state, so it needs the database. Optional on
    // purpose: without DATABASE_URL the gateway behaves exactly as it did
    // before the breaker existed, calling every provider in turn. A gateway
    // that refused to start without Postgres would make the credential tier
    // depend on the storage tier for no gain.
    let mut state = gateway::GatewayState::new(providers, auth);
    match outturn::db::connect_from_env().await {
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
