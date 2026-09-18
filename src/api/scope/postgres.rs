use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sqlx::{PgPool, Row, postgres::PgListener};
use uuid::Uuid;

use super::{Reach, ScopeError, ScopeStore};

const CHANNEL: &str = "outturn_scopes";

/// Who is narrowed to what, for one workspace.
type WorkspaceMap = HashMap<Uuid, HashSet<Uuid>>;

pub struct PostgresScopeStore {
    pool: PgPool,
    /// Filled on first use, dropped for a workspace when any of its scopes
    /// change -- on this pod directly, on every other through the notification
    /// a write sends. One entry per workspace rather than per person: a
    /// workspace has tens of people and hundreds of scopes, and loading the lot
    /// once is cheaper than a query per person per request.
    cache: Arc<Mutex<HashMap<Uuid, Arc<WorkspaceMap>>>>,
}

fn internal(e: sqlx::Error) -> ScopeError {
    ScopeError::Internal(e.to_string())
}

impl PostgresScopeStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Listens for scope changes made by any pod and forgets that workspace.
    ///
    /// Clears everything on reconnect, because a notification missed while
    /// disconnected would otherwise leave a stale entry until the next write --
    /// and a stale entry here is somebody reading what they should not.
    pub fn spawn_invalidation(&self) {
        let pool = self.pool.clone();
        let cache = Arc::clone(&self.cache);
        tokio::spawn(async move {
            loop {
                match listen(&pool, &cache).await {
                    Ok(()) => tracing::warn!("scope listener ended, restarting"),
                    Err(e) => tracing::error!(error = %e, "scope listener failed, restarting"),
                }
                if let Ok(mut c) = cache.lock() {
                    c.clear();
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    }

    fn forget(&self, workspace_id: Uuid) {
        if let Ok(mut c) = self.cache.lock() {
            c.remove(&workspace_id);
        }
    }

    async fn announce(&self, workspace_id: Uuid) {
        self.forget(workspace_id);
        if let Err(e) = sqlx::query("select pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(workspace_id.to_string())
            .execute(&self.pool)
            .await
        {
            tracing::warn!(error = %e, "could not announce a scope change");
        }
    }

    async fn load(&self, workspace_id: Uuid) -> Result<Arc<WorkspaceMap>, ScopeError> {
        if let Some(found) = self.cache.lock().ok().and_then(|c| c.get(&workspace_id).cloned()) {
            return Ok(found);
        }

        let rows = sqlx::query(
            "select user_id, agent_id from user_agent_scopes where workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut map: WorkspaceMap = HashMap::new();
        for row in &rows {
            map.entry(row.get("user_id"))
                .or_default()
                .insert(row.get("agent_id"));
        }

        let map = Arc::new(map);
        if let Ok(mut c) = self.cache.lock() {
            c.insert(workspace_id, Arc::clone(&map));
        }
        Ok(map)
    }
}

#[async_trait]
impl ScopeStore for PostgresScopeStore {
    async fn reach(&self, workspace_id: Uuid, user_id: Uuid) -> Result<Reach, ScopeError> {
        let map = self.load(workspace_id).await?;
        Ok(Reach::of(map.get(&user_id).cloned().unwrap_or_default()))
    }

    async fn set(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
        agents: &[Uuid],
    ) -> Result<(), ScopeError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;

        // Replaced rather than merged: the caller is saying what this person
        // may reach, not adding to it, and a partial update would leave them
        // with whatever an earlier call happened to set.
        sqlx::query("delete from user_agent_scopes where workspace_id = $1 and user_id = $2")
            .bind(workspace_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;

        for agent_id in agents {
            sqlx::query(
                "insert into user_agent_scopes (workspace_id, user_id, agent_id) \
                 values ($1, $2, $3) on conflict do nothing",
            )
            .bind(workspace_id)
            .bind(user_id)
            .bind(agent_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| match &e {
                // The agent is in another workspace, or gone. Said as a bad
                // request rather than an internal error: the caller named
                // something that is not theirs to name.
                sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                    ScopeError::Invalid("no such agent in this workspace".into())
                }
                _ => internal(e),
            })?;
        }

        tx.commit().await.map_err(internal)?;
        self.announce(workspace_id).await;
        Ok(())
    }

    async fn listing(&self, workspace_id: Uuid) -> Result<Vec<(Uuid, Vec<Uuid>)>, ScopeError> {
        let map = self.load(workspace_id).await?;
        let mut out: Vec<(Uuid, Vec<Uuid>)> = map
            .iter()
            .map(|(user, agents)| {
                let mut agents: Vec<Uuid> = agents.iter().copied().collect();
                agents.sort();
                (*user, agents)
            })
            .collect();
        // Ids are UUIDv7, so this is oldest account first -- an order that does
        // not change between calls, which a map's own would.
        out.sort_by_key(|(user, _)| *user);
        Ok(out)
    }
}

async fn listen(pool: &PgPool, cache: &Mutex<HashMap<Uuid, Arc<WorkspaceMap>>>) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;
    loop {
        let notification = listener.recv().await?;
        match notification.payload().parse::<Uuid>() {
            Ok(workspace_id) => {
                if let Ok(mut c) = cache.lock() {
                    c.remove(&workspace_id);
                }
            }
            Err(_) => tracing::warn!(
                payload = notification.payload(),
                "a scope notification did not name a workspace"
            ),
        }
    }
}
