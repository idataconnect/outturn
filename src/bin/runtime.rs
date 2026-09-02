use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use outturn::lifecycle::{self, Health};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let health = Health::new();

    let app = Router::new()
        .merge(lifecycle::routes(health.clone()));

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".into());
    let listener = TcpListener::bind(&addr).await.unwrap();
    tracing::info!("listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(lifecycle::shutdown_signal(health))
        .await
        .unwrap();
    tracing::info!("shutdown complete");
}
