use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::{PgListener, PgPool};
use uuid::Uuid;

use crate::auth::Authority;

use super::{
    CreateRole, RoleError, RoleStore, RoleTemplate, UpdateRole, WorkspaceRole,
    validate_authorities, validate_name,
};

/// Postgres channel a role change is announced on. The payload is the workspace.
const CHANNEL: &str = "outturn_roles";

/// Role name to authorities, for one workspace.
type WorkspaceMap = HashMap<String, HashSet<Authority>>;

/// How long an entry is served without a notification to say otherwise. A
/// backstop rather than the mechanism: what it bounds is a notification lost
/// in a way nothing here could see.
const ENTRY_TTL: Duration = Duration::from_secs(60);

/// Resolved roles by workspace. Filled on first use, dropped for a workspace
/// when any of its roles change -- on this pod directly, on every other pod
/// through the notification a write sends.
///
/// Reading a workspace and then inserting what was read races an invalidation
/// that lands in between: the stale entry goes in after the eviction, and
/// nothing evicts it again until the workspace's roles next change. So every
/// invalidation bumps the workspace's generation, every clear bumps the epoch,
/// and a load inserts only if neither moved while it was reading.
struct RoleCache {
    entries: HashMap<Uuid, (Instant, Arc<WorkspaceMap>)>,
    generations: HashMap<Uuid, u64>,
    epoch: u64,
    /// False while a listener exists but cannot hear invalidations. Nothing is
    /// served or stored then: a pod deaf to role changes must not go on
    /// trusting what it holds.
    trusted: bool,
}

/// What a load saw before it read, so the insert can tell whether anything
/// was invalidated meanwhile.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Ticket {
    epoch: u64,
    generation: u64,
}

impl RoleCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            generations: HashMap::new(),
            epoch: 0,
            trusted: true,
        }
    }

    fn get(&self, workspace_id: Uuid, now: Instant) -> Option<Arc<WorkspaceMap>> {
        if !self.trusted {
            return None;
        }
        self.entries
            .get(&workspace_id)
            .filter(|(at, _)| now.duration_since(*at) < ENTRY_TTL)
            .map(|(_, map)| Arc::clone(map))
    }

    fn ticket(&self, workspace_id: Uuid) -> Ticket {
        Ticket {
            epoch: self.epoch,
            generation: self.generations.get(&workspace_id).copied().unwrap_or(0),
        }
    }

    /// Stores what a load read, unless the workspace was invalidated, the cache
    /// cleared, or the listener lost since the load took its ticket.
    fn insert(&mut self, workspace_id: Uuid, ticket: Ticket, map: Arc<WorkspaceMap>, now: Instant) {
        if self.trusted && self.ticket(workspace_id) == ticket {
            self.entries.insert(workspace_id, (now, map));
        }
    }

    fn invalidate(&mut self, workspace_id: Uuid) {
        self.entries.remove(&workspace_id);
        *self.generations.entry(workspace_id).or_default() += 1;
    }

    /// The listener is gone: forget everything and serve nothing until it is
    /// back.
    fn distrust(&mut self) {
        self.trusted = false;
        self.clear();
    }

    /// The listener is listening again. Whatever changed while it was not was
    /// never heard, so this starts empty too.
    fn trust(&mut self) {
        self.trusted = true;
        self.clear();
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.generations.clear();
        self.epoch += 1;
    }
}

pub struct PostgresRoleStore {
    pool: PgPool,
    cache: Arc<Mutex<RoleCache>>,
}

fn internal(e: sqlx::Error) -> RoleError {
    RoleError::Internal(e.to_string())
}

fn poisoned() -> RoleError {
    RoleError::Internal("role cache lock poisoned".into())
}

impl PostgresRoleStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cache: Arc::new(Mutex::new(RoleCache::new())),
        }
    }

    /// Listens for role changes made by any pod and forgets what it knew
    /// about that workspace. Until the first LISTEN is in place, and from the
    /// moment the connection is seen to drop until it is back, the cache is
    /// empty and bypassed: every lookup reads Postgres. A notification missed
    /// while disconnected would otherwise cost a stale entry until the next
    /// write, which for a revoked authority is a grant that never ends.
    ///
    /// `try_recv` rather than `recv`, because `recv` reconnects transparently
    /// and so never says that notifications were lost.
    pub fn spawn_invalidation(&self) {
        let pool = self.pool.clone();
        let cache = Arc::clone(&self.cache);
        if let Ok(mut c) = cache.lock() {
            c.distrust();
        }
        tokio::spawn(async move {
            loop {
                match listen(&pool, &cache).await {
                    Ok(()) => tracing::warn!("role listener lost its connection, restarting"),
                    Err(e) => tracing::error!(error = %e, "role listener failed, restarting"),
                }
                if let Ok(mut c) = cache.lock() {
                    c.distrust();
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    fn forget(&self, workspace_id: Uuid) {
        if let Ok(mut c) = self.cache.lock() {
            c.invalidate(workspace_id);
        }
    }

    /// Tells every pod, this one included, that a workspace's roles changed.
    async fn announce(&self, workspace_id: Uuid) {
        self.forget(workspace_id);
        if let Err(e) = sqlx::query("select pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(workspace_id.to_string())
            .execute(&self.pool)
            .await
        {
            tracing::warn!(error = %e, "could not announce a role change");
        }
    }

    async fn load(&self, workspace_id: Uuid) -> Result<Arc<WorkspaceMap>, RoleError> {
        let ticket = {
            let c = self.cache.lock().map_err(|_| poisoned())?;
            if let Some(found) = c.get(workspace_id, Instant::now()) {
                return Ok(found);
            }
            c.ticket(workspace_id)
        };
        let rows = sqlx::query(
            "select r.name, a.authority \
             from roles r \
             left join role_authorities a on a.workspace_id = r.workspace_id and a.role_id = r.id \
             where r.workspace_id = $1",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut map: WorkspaceMap = HashMap::new();
        for row in &rows {
            let entry = map.entry(row.get::<String, _>("name")).or_default();
            if let Some(a) = row
                .get::<Option<String>, _>("authority")
                .as_deref()
                .and_then(Authority::parse)
            {
                entry.insert(a);
            }
        }
        let map = Arc::new(map);
        if let Ok(mut c) = self.cache.lock() {
            c.insert(workspace_id, ticket, Arc::clone(&map), Instant::now());
        }
        Ok(map)
    }

    async fn read(&self, workspace_id: Uuid, id: Uuid) -> Result<WorkspaceRole, RoleError> {
        let row = sqlx::query(
            "select r.id, r.workspace_id, r.name, r.description, \
                    coalesce(array_agg(a.authority order by a.authority) \
                             filter (where a.authority is not null), '{}') as authorities, \
                    (select count(*) from user_workspace_roles g \
                      where g.workspace_id = r.workspace_id and g.role_id = r.id) as holders \
             from roles r \
             left join role_authorities a on a.workspace_id = r.workspace_id and a.role_id = r.id \
             where r.workspace_id = $1 and r.id = $2 \
             group by r.id, r.workspace_id, r.name, r.description",
        )
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(RoleError::NotFound)?;
        Ok(read_role(&row))
    }
}

fn read_role(row: &sqlx::postgres::PgRow) -> WorkspaceRole {
    WorkspaceRole {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        description: row.get("description"),
        authorities: row.get("authorities"),
        holders: row.get("holders"),
    }
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// Returns `Ok` when the connection is lost, so the caller distrusts the cache
/// before a fresh listener is made.
async fn listen(pool: &PgPool, cache: &Mutex<RoleCache>) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(CHANNEL).await?;
    if let Ok(mut c) = cache.lock() {
        c.trust();
    }
    while let Some(notification) = listener.try_recv().await? {
        match notification.payload().parse::<Uuid>() {
            Ok(workspace_id) => {
                if let Ok(mut c) = cache.lock() {
                    c.invalidate(workspace_id);
                }
            }
            Err(_) => tracing::warn!("malformed role change notification"),
        }
    }
    Ok(())
}

/// Writes a role's authorities, replacing what was there.
async fn write_authorities(
    tx: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    role_id: Uuid,
    authorities: &[Authority],
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from role_authorities where workspace_id = $1 and role_id = $2")
        .bind(workspace_id)
        .bind(role_id)
        .execute(&mut *tx)
        .await?;
    for a in authorities {
        sqlx::query(
            "insert into role_authorities (workspace_id, role_id, authority) values ($1, $2, $3)",
        )
        .bind(workspace_id)
        .bind(role_id)
        .bind(a.as_str())
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

#[async_trait]
impl RoleStore for PostgresRoleStore {
    async fn list(
        &self,
        workspace_id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<WorkspaceRole>, RoleError> {
        let rows = sqlx::query(
            "select r.id, r.workspace_id, r.name, r.description, \
                    coalesce(array_agg(a.authority order by a.authority) \
                             filter (where a.authority is not null), '{}') as authorities, \
                    (select count(*) from user_workspace_roles g \
                      where g.workspace_id = r.workspace_id and g.role_id = r.id) as holders \
             from roles r \
             left join role_authorities a on a.workspace_id = r.workspace_id and a.role_id = r.id \
             where r.workspace_id = $1 and ($2::uuid is null or r.id > $2) \
             group by r.id, r.workspace_id, r.name, r.description \
             order by r.id limit $3",
        )
        .bind(workspace_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows.iter().map(read_role).collect())
    }

    async fn get(&self, workspace_id: Uuid, id: Uuid) -> Result<WorkspaceRole, RoleError> {
        self.read(workspace_id, id).await
    }

    async fn create(
        &self,
        workspace_id: Uuid,
        input: CreateRole,
    ) -> Result<WorkspaceRole, RoleError> {
        let name = validate_name(&input.name)?;
        let authorities = validate_authorities(&input.authorities)?;
        let id = Uuid::now_v7();

        let mut tx = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "insert into roles (workspace_id, id, name, description) values ($1, $2, $3, $4)",
        )
        .bind(workspace_id)
        .bind(id)
        .bind(&name)
        .bind(input.description.trim())
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                RoleError::Duplicate(name.clone())
            } else {
                internal(e)
            }
        })?;
        write_authorities(&mut tx, workspace_id, id, &authorities)
            .await
            .map_err(internal)?;
        tx.commit().await.map_err(internal)?;

        self.announce(workspace_id).await;
        self.read(workspace_id, id).await
    }

    async fn update(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        input: UpdateRole,
    ) -> Result<WorkspaceRole, RoleError> {
        let name = input.name.as_deref().map(validate_name).transpose()?;
        let authorities = input
            .authorities
            .as_deref()
            .map(validate_authorities)
            .transpose()?;

        let mut tx = self.pool.begin().await.map_err(internal)?;
        let updated = sqlx::query(
            "update roles set \
                 name = coalesce($3, name), \
                 description = coalesce($4, description) \
             where workspace_id = $1 and id = $2",
        )
        .bind(workspace_id)
        .bind(id)
        .bind(name.as_deref())
        .bind(input.description.as_deref().map(str::trim))
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                RoleError::Duplicate(name.clone().unwrap_or_default())
            } else {
                internal(e)
            }
        })?;
        if updated.rows_affected() == 0 {
            return Err(RoleError::NotFound);
        }
        if let Some(authorities) = &authorities {
            write_authorities(&mut tx, workspace_id, id, authorities)
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;

        self.announce(workspace_id).await;
        self.read(workspace_id, id).await
    }

    async fn delete(&self, workspace_id: Uuid, id: Uuid) -> Result<(), RoleError> {
        let role = self.read(workspace_id, id).await?;
        if role.holders > 0 {
            return Err(RoleError::InUse(role.holders));
        }
        sqlx::query("delete from roles where workspace_id = $1 and id = $2")
            .bind(workspace_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        self.announce(workspace_id).await;
        Ok(())
    }

    async fn authorities_for(
        &self,
        workspace_id: Uuid,
        roles: &[String],
    ) -> Result<HashSet<Authority>, RoleError> {
        let map = self.load(workspace_id).await?;
        Ok(roles
            .iter()
            .filter_map(|r| map.get(r))
            .flat_map(|set| set.iter().copied())
            .collect())
    }

    async fn roles_with(
        &self,
        workspace_id: Uuid,
        authority: Authority,
    ) -> Result<Vec<Uuid>, RoleError> {
        // Queried rather than read from the per-workspace cache, which is keyed
        // by role name because that is what a token carries. This wants ids, and
        // a second index on the same cache would be a second thing to invalidate.
        let rows = sqlx::query(
            "select r.id from roles r \
               join role_authorities a on a.workspace_id = r.workspace_id and a.role_id = r.id \
              where r.workspace_id = $1 and a.authority = $2 \
              order by r.id",
        )
        .bind(workspace_id)
        .bind(authority.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }

    async fn templates(&self) -> Result<Vec<RoleTemplate>, RoleError> {
        let rows = sqlx::query(
            "select t.name, t.description, a.authority \
               from role_templates t \
               left join role_template_authorities a on a.template_name = t.name \
              order by t.position, t.name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut order: Vec<String> = Vec::new();
        let mut built: HashMap<String, RoleTemplate> = HashMap::new();
        for row in &rows {
            let name: String = row.get("name");
            let entry = built.entry(name.clone()).or_insert_with(|| {
                order.push(name.clone());
                RoleTemplate {
                    name: name.clone(),
                    description: row.get("description"),
                    authorities: Vec::new(),
                }
            });

            let Some(raw) = row.get::<Option<String>, _>("authority") else {
                continue;
            };
            // Said out loud rather than dropped quietly. A template is a
            // deployment's to edit, so a typo here is a role that comes out
            // narrower than whoever wrote it meant, with nothing to show for it.
            match Authority::parse(raw.trim()) {
                None => tracing::warn!(
                    template = %name,
                    authority = %raw,
                    "role template names an authority this build does not have; ignoring it"
                ),
                Some(a) if !a.workspace_assignable() => tracing::warn!(
                    template = %name,
                    authority = %raw,
                    "role template names an authority reserved to the platform; ignoring it"
                ),
                Some(a) => entry.authorities.push(a),
            }
        }

        Ok(order.into_iter().filter_map(|n| built.remove(&n)).collect())
    }

    async fn seed_defaults(&self, workspace_id: Uuid) -> Result<(), RoleError> {
        let existing: i64 =
            sqlx::query_scalar("select count(*) from roles where workspace_id = $1")
                .bind(workspace_id)
                .fetch_one(&self.pool)
                .await
                .map_err(internal)?;
        if existing > 0 {
            return Ok(());
        }
        let templates = self.templates().await?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        for template in &templates {
            let id = Uuid::now_v7();
            sqlx::query(
                "insert into roles (workspace_id, id, name, description) values ($1, $2, $3, $4)",
            )
            .bind(workspace_id)
            .bind(id)
            .bind(&template.name)
            .bind(&template.description)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
            write_authorities(&mut tx, workspace_id, id, &template.authorities)
                .await
                .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)?;
        self.announce(workspace_id).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(role: &str) -> Arc<WorkspaceMap> {
        Arc::new(HashMap::from([(role.to_string(), HashSet::new())]))
    }

    #[test]
    fn an_invalidation_between_read_and_insert_keeps_the_stale_read_out() {
        let mut cache = RoleCache::new();
        let ws = Uuid::now_v7();
        let now = Instant::now();

        let ticket = cache.ticket(ws);
        // The read happens here; the role changes before it is stored.
        cache.invalidate(ws);
        cache.insert(ws, ticket, map("stale"), now);
        assert!(cache.get(ws, now).is_none());

        let ticket = cache.ticket(ws);
        cache.insert(ws, ticket, map("fresh"), now);
        assert!(cache.get(ws, now).unwrap().contains_key("fresh"));
    }

    #[test]
    fn another_workspace_changing_does_not_block_an_insert() {
        let mut cache = RoleCache::new();
        let ws = Uuid::now_v7();
        let now = Instant::now();
        let ticket = cache.ticket(ws);
        cache.invalidate(Uuid::now_v7());
        cache.insert(ws, ticket, map("r"), now);
        assert!(cache.get(ws, now).is_some());
    }

    #[test]
    fn a_lost_listener_clears_and_serves_nothing_until_it_is_back() {
        let mut cache = RoleCache::new();
        let ws = Uuid::now_v7();
        let now = Instant::now();
        let ticket = cache.ticket(ws);
        cache.insert(ws, ticket, map("r"), now);
        assert!(cache.get(ws, now).is_some());

        let in_flight = cache.ticket(ws);
        cache.distrust();
        assert!(cache.get(ws, now).is_none());
        let ticket = cache.ticket(ws);
        cache.insert(ws, ticket, map("r"), now);
        assert!(cache.get(ws, now).is_none(), "nothing is stored while deaf");

        cache.trust();
        // A load that began before the drop cannot land after the recovery.
        cache.insert(ws, in_flight, map("stale"), now);
        assert!(cache.get(ws, now).is_none());
        let ticket = cache.ticket(ws);
        cache.insert(ws, ticket, map("r"), now);
        assert!(cache.get(ws, now).is_some());
    }

    #[test]
    fn an_entry_expires() {
        let mut cache = RoleCache::new();
        let ws = Uuid::now_v7();
        let now = Instant::now();
        let ticket = cache.ticket(ws);
        cache.insert(ws, ticket, map("r"), now);
        assert!(cache.get(ws, now + ENTRY_TTL).is_none());
    }
}
