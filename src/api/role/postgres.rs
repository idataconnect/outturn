use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

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

pub struct PostgresRoleStore {
    pool: PgPool,
    /// Resolved roles by workspace. Filled on first use, dropped for a workspace
    /// when any of its roles change -- on this pod directly, on every other
    /// pod through the notification a write sends.
    cache: Arc<Mutex<HashMap<Uuid, Arc<WorkspaceMap>>>>,
}

fn internal(e: sqlx::Error) -> RoleError {
    RoleError::Internal(e.to_string())
}

impl PostgresRoleStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Listens for role changes made by any pod and forgets what it knew
    /// about that workspace. Reconnects if the connection drops; a notification
    /// missed while disconnected costs a stale entry until the next write,
    /// which is the same as a cache with no invalidation at all -- so the
    /// listener also clears everything when it reconnects.
    pub fn spawn_invalidation(&self) {
        let pool = self.pool.clone();
        let cache = Arc::clone(&self.cache);
        tokio::spawn(async move {
            loop {
                match listen(&pool, &cache).await {
                    Ok(()) => tracing::warn!("role listener ended, restarting"),
                    Err(e) => tracing::error!(error = %e, "role listener failed, restarting"),
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
        if let Some(found) = self
            .cache
            .lock()
            .ok()
            .and_then(|c| c.get(&workspace_id).cloned())
        {
            return Ok(found);
        }
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
            c.insert(workspace_id, Arc::clone(&map));
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

async fn listen(
    pool: &PgPool,
    cache: &Mutex<HashMap<Uuid, Arc<WorkspaceMap>>>,
) -> Result<(), sqlx::Error> {
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
            Err(_) => tracing::warn!("malformed role change notification"),
        }
    }
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
    async fn list(&self, workspace_id: Uuid) -> Result<Vec<WorkspaceRole>, RoleError> {
        let rows = sqlx::query(
            "select r.id, r.workspace_id, r.name, r.description, \
                    coalesce(array_agg(a.authority order by a.authority) \
                             filter (where a.authority is not null), '{}') as authorities, \
                    (select count(*) from user_workspace_roles g \
                      where g.workspace_id = r.workspace_id and g.role_id = r.id) as holders \
             from roles r \
             left join role_authorities a on a.workspace_id = r.workspace_id and a.role_id = r.id \
             where r.workspace_id = $1 \
             group by r.id, r.workspace_id, r.name, r.description \
             order by r.name",
        )
        .bind(workspace_id)
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
