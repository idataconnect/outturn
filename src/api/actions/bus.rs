//! One Postgres listener per pod, fanned out to every parked request.
//!
//! The same bargain `events::notify` makes, and for the same reason: a
//! connection per waiter would cap concurrent long polls at the pool size,
//! while this holds one however many clients are waiting.
//!
//! The hint carries addressing only. A client re-reads its queue from its own
//! account, so a coalesced or dropped notification costs a refetch rather than
//! a stale badge -- which is what makes it safe for the listener to reconnect
//! and lose whatever arrived while it was gone.

use std::time::Duration;

use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::postgres::CHANNEL;

/// Who a queue change concerns.
///
/// Roles and users rather than one target, because one item may be addressed to
/// several and a single announcement covers them all. A parked request keeps
/// its own roles and matches against both lists.
#[derive(Debug, Clone)]
pub struct ActionHint {
    pub workspace_id: Uuid,
    pub roles: Vec<Uuid>,
    pub users: Vec<Uuid>,
}

impl ActionHint {
    /// Whether this hint could have changed what the given person sees.
    ///
    /// Deliberately generous: a role they hold, or their own id. Being woken
    /// for a change that turns out not to affect them costs one query, and the
    /// alternative -- resolving membership inside the listener -- would put a
    /// database read on the path that every pod runs for every announcement.
    pub fn concerns(&self, user_id: Uuid, roles: &[Uuid]) -> bool {
        self.users.contains(&user_id) || self.roles.iter().any(|r| roles.contains(r))
    }
}

/// Fans one Postgres LISTEN connection out to every parked request.
#[derive(Clone)]
pub struct ActionBus {
    tx: broadcast::Sender<ActionHint>,
}

impl ActionBus {
    /// Spawns the listener task and returns a handle to subscribe to.
    ///
    /// Reconnects on its own. Notifications that arrive while it is
    /// disconnected are lost by design -- a waiter re-reads its queue when it
    /// wakes, so a missed hint costs a poll cycle rather than a wrong answer.
    pub fn spawn(pool: PgPool) -> Self {
        let (tx, _) = broadcast::channel(256);
        let bus = Self { tx: tx.clone() };

        tokio::spawn(async move {
            loop {
                match listen_loop(&pool, &tx).await {
                    Ok(()) => tracing::warn!("action listener ended, restarting"),
                    Err(e) => tracing::error!(error = %e, "action listener failed, restarting"),
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });

        bus
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ActionHint> {
        self.tx.subscribe()
    }
}

async fn listen_loop(pool: &PgPool, tx: &broadcast::Sender<ActionHint>) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;
    tracing::info!(channel = CHANNEL, "listening for action queue changes");

    loop {
        let notification = listener.recv().await?;
        match serde_json::from_str::<Payload>(notification.payload()) {
            // A send error just means nobody is parked right now.
            Ok(p) => {
                let _ = tx.send(ActionHint {
                    workspace_id: p.workspace_id,
                    roles: p.roles,
                    users: p.users,
                });
            }
            Err(e) => tracing::warn!(error = %e, "malformed action notification"),
        }
    }
}

#[derive(serde::Deserialize)]
struct Payload {
    workspace_id: Uuid,
    #[serde(default)]
    roles: Vec<Uuid>,
    #[serde(default)]
    users: Vec<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    #[test]
    fn a_hint_naming_the_person_concerns_them() {
        let hint = ActionHint {
            workspace_id: id(1),
            roles: vec![],
            users: vec![id(2)],
        };
        assert!(hint.concerns(id(2), &[]));
    }

    #[test]
    fn a_hint_naming_a_role_they_hold_concerns_them() {
        let hint = ActionHint {
            workspace_id: id(1),
            roles: vec![id(3)],
            users: vec![],
        };
        assert!(hint.concerns(id(2), &[id(3)]));
    }

    #[test]
    fn a_hint_for_somebody_else_does_not() {
        let hint = ActionHint {
            workspace_id: id(1),
            roles: vec![id(4)],
            users: vec![id(5)],
        };
        assert!(!hint.concerns(id(2), &[id(3)]));
    }

    #[test]
    fn a_hint_naming_nothing_concerns_nobody() {
        // `deliver` refuses an empty delivery, so this should not arrive -- but
        // a hint that matched everybody would wake every parked request on the
        // pod, which is the failure worth being certain about.
        let hint = ActionHint {
            workspace_id: id(1),
            roles: vec![],
            users: vec![],
        };
        assert!(!hint.concerns(id(2), &[id(3)]));
    }

    #[test]
    fn a_malformed_payload_is_not_read_as_a_wildcard() {
        // The listener logs and drops these; what matters is that a payload
        // missing its lists deserialises to empty rather than failing open.
        let p: Payload =
            serde_json::from_str(r#"{"workspace_id":"00000000-0000-0000-0000-000000000001"}"#)
                .expect("absent lists default");
        assert!(p.roles.is_empty() && p.users.is_empty());
    }
}
