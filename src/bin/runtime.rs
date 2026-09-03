use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use outturn::auth::{TokenMinter, TokenValidator};
use outturn::lifecycle::{self, Health};
use outturn::runtime::component::AgentRunner;
use outturn::runtime::router::{self, RuntimeState};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let health = Health::new();

    // Read once at startup: it is the same bytes for every session, and
    // compiling it per turn would be waste.
    let agent_module = std::fs::read(
        std::env::var("OUTTURN_AGENT_MODULE")
            .unwrap_or_else(|_| "/usr/local/share/outturn/agent_default.wasm".into()),
    )
    .expect("agent component");

    let validator = TokenValidator::from_env().expect("token validator");
    let (minter, _public) = TokenMinter::from_env().expect("token minter");

    let state = Arc::new(RuntimeState {
        auth: validator,
        minter,
        gateway_url: std::env::var("OUTTURN_GATEWAY_URL")
            .unwrap_or_else(|_| "http://outturn-gateway:8081".into()),
        runner: Arc::new(AgentRunner::new().expect("agent runner")),
        agent_module: Arc::new(agent_module),
    });

    let app = Router::new()
        .merge(lifecycle::routes(health.clone()))
        .merge(router::routes(state));

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".into());
    let listener = TcpListener::bind(&addr).await.unwrap();
    tracing::info!("listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(lifecycle::shutdown_signal(health))
        .await
        .unwrap();
    tracing::info!("shutdown complete");
}
