use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use outturn::auth::TokenValidator;
use outturn::gateway;
use outturn::gateway::llm::provider::{LlmProvider, mock::MockProvider, ollama::OllamaProvider};
use outturn::lifecycle::{self, Health};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let health = Health::new();
    let auth = TokenValidator::from_env().expect("failed to initialize token validator");

    let mut providers: Vec<Arc<dyn LlmProvider>> = Vec::new();

    if let Some(ollama) = OllamaProvider::from_env() {
        tracing::info!("ollama provider enabled");
        providers.push(Arc::new(ollama));
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
