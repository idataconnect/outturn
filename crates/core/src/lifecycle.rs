use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{Router, extract::State, http::StatusCode, routing::get};
use tokio::signal;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct Health {
    ready: Arc<AtomicBool>,
    shutdown: Shutdown,
}

/// That the process has been told to stop, as a state rather than a moment.
///
/// It was a bare `Notify` woken with `notify_waiters`, which reaches only what
/// is waiting at that instant. A loop busy elsewhere when the signal came --
/// the runtime's puller, mid-way through asking the API for work -- missed it
/// for good, and went on asking for and running new turns until the pod was
/// killed. Now the flag stays set: `wait` returns at once once it is, and
/// `is_set` can be asked between steps.
#[derive(Clone, Default)]
pub struct Shutdown {
    set: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn trigger(&self) {
        self.set.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    /// `wait`, as a future that owns what it needs, for a caller that holds
    /// it past the borrow it was made from -- a long poll's select.
    pub fn wait_owned(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let this = self.clone();
        async move { this.wait().await }
    }

    /// Returns once shutdown has been asked for, at once if it already was.
    pub async fn wait(&self) {
        loop {
            // Registered before the flag is read, so a trigger between the
            // two still wakes this.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

impl Health {
    pub fn new() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            shutdown: Shutdown::new(),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::SeqCst);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    pub fn shutdown_signal(&self) -> Shutdown {
        self.shutdown.clone()
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
        signal::ctrl_c()
            .await
            .expect("failed to install ctrl+c handler");
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
    health.shutdown.trigger();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this type exists for: a signal sent while nobody was waiting
    /// was gone, so a loop busy elsewhere never learned it should stop.
    #[tokio::test]
    async fn a_shutdown_asked_before_anyone_waits_is_still_seen() {
        let shutdown = Shutdown::new();
        shutdown.trigger();
        assert!(shutdown.is_set());
        // Returns at once; were it to wait, this test would never end.
        shutdown.wait().await;
        shutdown.wait_owned().await;
    }

    #[tokio::test]
    async fn a_waiter_is_woken_by_a_shutdown_asked_later() {
        let shutdown = Shutdown::new();
        let waiting = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { shutdown.wait().await }
        });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_set());
        shutdown.trigger();
        waiting.await.expect("the waiter ended");
    }
}
