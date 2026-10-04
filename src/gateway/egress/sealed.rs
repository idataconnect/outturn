//! Sealed credentials as the gateway reads them: by id, from its own database,
//! opened with its own key, and held in this replica's memory and nowhere else.
//! See docs/sealed-credentials.md.
//!
//! The cache is specified rather than borrowed, because the obvious one loses a
//! revocation. Reading a row and then inserting what was read races a
//! notification that lands in between: the stale entry goes in after the
//! eviction, and a revoked row never changes again, so nothing would ever evict
//! it. So every notification bumps the credential's generation and every clear
//! bumps the epoch, a load inserts only if neither moved while it read, entries
//! expire after a minute whatever happens, and while the listener is down
//! nothing is served from memory at all. The same rules as `api::role::postgres`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::Row;
use sqlx::postgres::{PgListener, PgPool};
use uuid::Uuid;

use crate::egress::seal::{Binding, Keys};

const CHANNEL: &str = "credentials_changed";

/// A backstop rather than the mechanism: what it bounds is a notification lost
/// in a way nothing here could see.
const ENTRY_TTL: Duration = Duration::from_secs(60);

/// A credential, opened.
pub struct Opened {
    pub binding: Binding,
    pub secret: zeroize::Zeroizing<Vec<u8>>,
    /// `Keys::fingerprint` of the secret, for showing beside the credential.
    pub fingerprint: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Ticket {
    epoch: u64,
    generation: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Uuid, (Instant, Arc<Opened>)>,
    generations: HashMap<Uuid, u64>,
    epoch: u64,
    /// True only while a listener is hearing changes.
    trusted: bool,
}

impl Cache {
    fn get(&self, id: Uuid, now: Instant) -> Option<Arc<Opened>> {
        if !self.trusted {
            return None;
        }
        self.entries
            .get(&id)
            .filter(|(at, _)| now.duration_since(*at) < ENTRY_TTL)
            .map(|(_, opened)| Arc::clone(opened))
    }

    fn ticket(&self, id: Uuid) -> Ticket {
        Ticket {
            epoch: self.epoch,
            generation: self.generations.get(&id).copied().unwrap_or(0),
        }
    }

    fn insert(&mut self, id: Uuid, ticket: Ticket, opened: Arc<Opened>, now: Instant) {
        if self.trusted && self.ticket(id) == ticket {
            self.entries.insert(id, (now, opened));
        }
    }

    fn invalidate(&mut self, id: Uuid) {
        self.entries.remove(&id);
        *self.generations.entry(id).or_default() += 1;
    }

    fn set_trusted(&mut self, trusted: bool) {
        self.trusted = trusted;
        self.entries.clear();
        self.generations.clear();
        self.epoch += 1;
    }
}

/// Where a gateway gets a sealed credential from.
#[derive(Default)]
pub struct Credentials {
    pool: Option<PgPool>,
    keys: Keys,
    cache: Arc<Mutex<Cache>>,
}

impl Credentials {
    pub fn from_env() -> Self {
        Self {
            pool: None,
            keys: Keys::from_env(),
            cache: Arc::default(),
        }
    }

    /// The database to read credentials from, and a listener on it that keeps
    /// the cache honest. Without one, no sealed credential is attached: there is
    /// nothing to read it from, and the request is refused.
    pub fn with_pool(mut self, pool: PgPool) -> Self {
        let cache = Arc::clone(&self.cache);
        let listening = pool.clone();
        tokio::spawn(async move {
            loop {
                match listen(&listening, &cache).await {
                    Ok(()) => tracing::warn!("credential listener lost its connection, restarting"),
                    Err(e) => tracing::error!(error = %e, "credential listener failed, restarting"),
                }
                if let Ok(mut c) = cache.lock() {
                    c.set_trusted(false);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        self.pool = Some(pool);
        self
    }

    /// The keys to open with, for a test that seals its own.
    pub fn with_keys(mut self, keys: Keys) -> Self {
        self.keys = keys;
        self
    }

    /// Reads, opens and returns a credential.
    ///
    /// Every failure is the same refusal to the caller: a credential that does
    /// not exist, was revoked, was sealed to a key this gateway does not hold,
    /// or was tampered with is in each case a credential this gateway does not
    /// have. Which one is logged here, for whoever runs it.
    pub async fn get(&self, id: Uuid) -> Result<Arc<Opened>, String> {
        let refused = || "this host's credential is not available".to_string();
        let Some(pool) = &self.pool else {
            tracing::warn!(credential_id = %id, "a sealed credential was asked for, and this gateway has no database to read it from");
            return Err(refused());
        };
        let ticket = {
            let c = self.cache.lock().map_err(|_| refused())?;
            if let Some(found) = c.get(id, Instant::now()) {
                return Ok(found);
            }
            c.ticket(id)
        };
        let row = sqlx::query(
            "select binding, sealed, key_id from credentials \
             where id = $1 and revoked_at is null and sealed is not null",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, credential_id = %id, "reading a sealed credential failed");
            refused()
        })?
        .ok_or_else(|| {
            tracing::info!(credential_id = %id, "no live credential with that id");
            refused()
        })?;
        let binding_bytes: Vec<u8> = row.get("binding");
        let sealed: Vec<u8> = row.get("sealed");
        let key_id: String = row.get("key_id");
        let secret = self
            .keys
            .open(&key_id, &sealed, &binding_bytes)
            .map_err(|why| {
                tracing::warn!(credential_id = %id, %why, "a sealed credential did not open");
                refused()
            })?;
        // Parsed as strictly as the API parsed it, from the bytes the tag was
        // just checked over -- so what is enforced is what was sealed.
        let binding = Binding::parse(&binding_bytes).map_err(|why| {
            tracing::warn!(credential_id = %id, %why, "a sealed credential's binding is not readable");
            refused()
        })?;
        if binding.credential != id {
            tracing::warn!(credential_id = %id, "a sealed credential is bound to another id");
            return Err(refused());
        }
        let fingerprint = self.keys.fingerprint(&key_id, &secret).unwrap_or_default();
        let opened = Arc::new(Opened {
            binding,
            secret,
            fingerprint,
        });
        if let Ok(mut c) = self.cache.lock() {
            c.insert(id, ticket, Arc::clone(&opened), Instant::now());
        }
        Ok(opened)
    }
}

/// Returns `Ok` when the connection is lost, so the caller distrusts the cache
/// before a fresh listener is made.
async fn listen(pool: &PgPool, cache: &Mutex<Cache>) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;
    if let Ok(mut c) = cache.lock() {
        c.set_trusted(true);
    }
    while let Some(notification) = listener.try_recv().await? {
        match notification.payload().parse::<Uuid>() {
            Ok(id) => {
                if let Ok(mut c) = cache.lock() {
                    c.invalidate(id);
                }
            }
            Err(_) => tracing::warn!("malformed credential change notification"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> Arc<Opened> {
        Arc::new(Opened {
            binding: Binding {
                credential: Uuid::nil(),
                kind: crate::egress::seal::Kind::Static,
                workspaces: vec!["*".into()],
                hosts: vec!["a.example.com".into()],
                header: Some("authorization".into()),
                token_url: None,
            },
            secret: zeroize::Zeroizing::new(b"k".to_vec()),
            fingerprint: String::new(),
        })
    }

    /// The race this exists for: a revocation landing between a read and its
    /// insert must not leave the pre-revocation secret cached.
    #[test]
    fn a_read_that_raced_a_revocation_is_not_kept() {
        let mut c = Cache::default();
        c.set_trusted(true);
        let id = Uuid::nil();
        let now = Instant::now();
        let ticket = c.ticket(id);
        c.invalidate(id);
        c.insert(id, ticket, opened(), now);
        assert!(c.get(id, now).is_none());
    }

    #[test]
    fn nothing_is_served_while_nobody_is_listening() {
        let mut c = Cache::default();
        c.set_trusted(true);
        let id = Uuid::nil();
        let now = Instant::now();
        c.insert(id, c.ticket(id), opened(), now);
        assert!(c.get(id, now).is_some());
        c.set_trusted(false);
        assert!(c.get(id, now).is_none());
        c.insert(id, c.ticket(id), opened(), now);
        assert!(c.get(id, now).is_none(), "nor stored");
    }

    #[test]
    fn an_entry_expires_whatever_is_heard() {
        let mut c = Cache::default();
        c.set_trusted(true);
        let id = Uuid::nil();
        let now = Instant::now();
        c.insert(id, c.ticket(id), opened(), now);
        assert!(c.get(id, now + ENTRY_TTL).is_none());
    }
}
