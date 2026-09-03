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
    let auth = TokenValidator::from_env().expect("failed to initialize token validator");

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

    let state = Arc::new(gateway::GatewayState::new(providers, auth));

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
