use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{Router, extract::State, http::StatusCode, routing::get};
use tokio::signal;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct Health {
    ready: Arc<AtomicBool>,
    shutdown: Arc<Notify>,
}

impl Health {
    pub fn new() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            shutdown: Arc::new(Notify::new()),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::SeqCst);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    pub fn shutdown_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.shutdown)
    }
}

impl Default for Health {
    fn default() -> Self {
        Self::new()
    }
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn readyz(State(health): State<Health>) -> StatusCode {
    if health.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

pub fn routes(health: Health) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(health)
}

pub async fn shutdown_signal(health: Health) {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install ctrl+c handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received, draining connections");
    health.set_ready(false);
    health.shutdown.notify_waiters();
}
