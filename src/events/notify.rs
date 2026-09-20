use std::time::Duration;

use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Postgres channel every event insert announces on.
pub const CHANNEL: &str = "outturn_events";

/// A hint that something was appended. Carries no payload beyond addressing:
/// listeners re-query from their own cursor, so a missed or coalesced
/// notification costs a poll cycle rather than an update.
#[derive(Debug, Clone, Copy)]
pub struct EventHint {
    pub workspace_id: Uuid,
    pub session_id: Option<Uuid>,
}

/// Fans one Postgres LISTEN connection out to every parked request.
///
/// A connection per waiter would cap concurrent long polls at the pool size;
/// this keeps it at one regardless of how many clients are waiting.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EventHint>,
}

impl EventBus {
    /// Spawns the listener task and returns a handle to subscribe to.
    ///
    /// The task reconnects on its own if the connection drops. Notifications
    /// that arrive while it is disconnected are lost by design — the cursor in
    /// each request is what makes that safe.
    pub fn spawn(pool: PgPool) -> Self {
        let (tx, _) = broadcast::channel(256);
        let bus = Self { tx: tx.clone() };

        tokio::spawn(async move {
            loop {
                match listen_loop(&pool, &tx).await {
                    Ok(()) => tracing::warn!("event listener ended, restarting"),
                    Err(e) => tracing::error!(error = %e, "event listener failed, restarting"),
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });

        bus
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventHint> {
        self.tx.subscribe()
    }
}

async fn listen_loop(pool: &PgPool, tx: &broadcast::Sender<EventHint>) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;
    tracing::info!(channel = CHANNEL, "listening for events");

    loop {
        let notification = listener.recv().await?;
        match serde_json::from_str::<Payload>(notification.payload()) {
            // A send error just means nobody is parked right now.
            Ok(p) => {
                let _ = tx.send(EventHint {
                    workspace_id: p.workspace_id,
                    session_id: p.session_id,
                });
            }
            Err(e) => tracing::warn!(error = %e, "malformed event notification"),
        }
    }
}

#[derive(serde::Deserialize)]
struct Payload {
    workspace_id: Uuid,
    session_id: Option<Uuid>,
}
