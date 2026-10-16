use std::time::Duration;

use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::{broadcast, watch};
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
/// this keeps it at one regardless of how many clients are waiting. One type
/// for every channel, so the connection, the restarts and the readiness
/// signal are written once; a bus says only which channel it listens on and
/// what a payload becomes.
pub struct Bus<H> {
    tx: broadcast::Sender<H>,
    /// Whether the listener is subscribed right now. Notifications sent while
    /// it is not are lost, which re-reading on waking makes safe for a client
    /// -- but a caller that needs one to arrive has to know when it can be
    /// heard.
    listening: watch::Receiver<bool>,
}

/// Derived by hand: the derive would ask for `H: Clone`, and cloning a bus is
/// cloning its handles, not a hint.
impl<H> Clone for Bus<H> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            listening: self.listening.clone(),
        }
    }
}

impl<H: Clone + Send + 'static> Bus<H> {
    /// Spawns the listener task and returns a handle to subscribe to.
    ///
    /// The task reconnects on its own if the connection drops. Notifications
    /// that arrive while it is disconnected are lost by design: whoever
    /// waits re-reads from their own cursor when they wake, so a missed hint
    /// costs a poll cycle rather than a wrong answer.
    pub fn listen<P: serde::de::DeserializeOwned + 'static>(
        pool: PgPool,
        channel: &'static str,
        hint: fn(P) -> H,
    ) -> Self {
        let (tx, _) = broadcast::channel(256);
        let (listening_tx, listening) = watch::channel(false);
        let bus = Self {
            tx: tx.clone(),
            listening,
        };

        tokio::spawn(async move {
            loop {
                let result = listen_loop(&pool, channel, &tx, &listening_tx, hint).await;
                listening_tx.send_replace(false);
                match result {
                    Ok(()) => tracing::warn!(channel, "listener ended, restarting"),
                    Err(e) => tracing::error!(channel, error = %e, "listener failed, restarting"),
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });

        bus
    }

    pub fn subscribe(&self) -> broadcast::Receiver<H> {
        self.tx.subscribe()
    }

    /// Resolves once the listener is subscribed, so a notification sent after
    /// this returns will be heard. Immediately, if it already is.
    pub async fn listening(&self) {
        let mut listening = self.listening.clone();
        // An error means the task holding the sender is gone, so nothing
        // will ever be heard; returning beats waiting for ever.
        let _ = listening.wait_for(|on| *on).await;
    }

    /// How many requests are parked on this bus right now. A request
    /// subscribes before it parks, so a notification sent once this counts
    /// it is one it will hear.
    pub fn waiters(&self) -> usize {
        self.tx.receiver_count()
    }
}

async fn listen_loop<H, P: serde::de::DeserializeOwned>(
    pool: &PgPool,
    channel: &'static str,
    tx: &broadcast::Sender<H>,
    listening: &watch::Sender<bool>,
    hint: fn(P) -> H,
) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(channel).await?;
    listening.send_replace(true);
    tracing::info!(channel, "listening");

    loop {
        let notification = listener.recv().await?;
        match serde_json::from_str::<P>(notification.payload()) {
            // A send error just means nobody is parked right now.
            Ok(payload) => {
                let _ = tx.send(hint(payload));
            }
            Err(e) => tracing::warn!(channel, error = %e, "malformed notification"),
        }
    }
}

/// The bus every event insert announces on.
pub type EventBus = Bus<EventHint>;

impl Bus<EventHint> {
    pub fn spawn(pool: PgPool) -> Self {
        Self::listen(pool, CHANNEL, |p: Payload| EventHint {
            workspace_id: p.workspace_id,
            session_id: p.session_id,
        })
    }
}

#[derive(serde::Deserialize)]
struct Payload {
    workspace_id: Uuid,
    session_id: Option<Uuid>,
}
