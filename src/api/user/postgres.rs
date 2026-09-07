use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::auth::{Role, password};

use super::{
    CreateUser, Identity, PROVIDER_PASSWORD, TenantMembership, User, UserError, UserStore, validate,
};

pub struct PostgresUserStore {
    pool: PgPool,
}

impl PostgresUserStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn system_roles(&self, user_id: Uuid) -> Result<Vec<Role>, UserError> {
        let rows = sqlx::query("select role from user_system_roles where user_id = $1")
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;
        Ok(collect_roles(rows))
    }

    async fn identities(&self, user_id: Uuid) -> Result<Vec<Identity>, UserError> {
        let rows = sqlx::query(
            "select id, provider, provider_subject, verified_at \
             from user_identities where user_id = $1 order by created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows.iter().map(read_identity).collect())
    }
}

fn internal(e: sqlx::Error) -> UserError {
    UserError::Internal(e.to_string())
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// Rows whose role text does not parse are dropped: the check constraints keep
/// the tables clean, and an unknown role must never widen access.
fn collect_roles(rows: Vec<sqlx::postgres::PgRow>) -> Vec<Role> {
    rows.iter()
        .filter_map(|r| r.get::<String, _>("role").parse().ok())
        .collect()
}

fn read_identity(row: &sqlx::postgres::PgRow) -> Identity {
    Identity {
        id: row.get("id"),
        provider: row.get("provider"),
        subject: row.get("provider_subject"),
        verified: row
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("verified_at")
            .is_some(),
    }
}

fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

#[async_trait]
impl UserStore for PostgresUserStore {
    async fn list(&self) -> Result<Vec<User>, UserError> {
        let rows = sqlx::query(
            "select u.id, u.display_name, \
                    coalesce(array_agg(sr.role) filter (where sr.role is not null), '{}') as roles \
             from users u \
             left join user_system_roles sr on sr.user_id = u.id \
             group by u.id, u.display_name \
             order by u.display_name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut users = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: Uuid = row.get("id");
            users.push(User {
                id,
                display_name: row.get("display_name"),
                identities: self.identities(id).await?,
                system_roles: row
                    .get::<Vec<String>, _>("roles")
                    .iter()
                    .filter_map(|s| s.parse().ok())
                    .collect(),
            });
        }
        Ok(users)
    }

    async fn list_for_tenant(&self, tenant_id: Uuid) -> Result<Vec<User>, UserError> {
        let rows = sqlx::query(
            "select u.id, u.display_name, \
                    coalesce(array_agg(sr.role) filter (where sr.role is not null), '{}') as roles \
             from users u \
             left join user_system_roles sr on sr.user_id = u.id \
             where exists (select 1 from user_tenant_roles tr \
                           where tr.user_id = u.id and tr.tenant_id = $1) \
             group by u.id, u.display_name \
             order by u.display_name",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let mut users = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: Uuid = row.get("id");
            users.push(User {
                id,
                display_name: row.get("display_name"),
                identities: self.identities(id).await?,
                system_roles: row
                    .get::<Vec<String>, _>("roles")
                    .iter()
                    .filter_map(|s| s.parse().ok())
                    .collect(),
            });
        }
        Ok(users)
    }

    async fn rename(&self, id: Uuid, display_name: &str) -> Result<User, UserError> {
        let name = display_name.trim();
        if name.is_empty() {
            return Err(UserError::Invalid("display name must not be empty".into()));
        }
        let updated = sqlx::query("update users set display_name = $2, updated_at = now() where id = $1")
            .bind(id)
            .bind(name)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        if updated.rows_affected() == 0 {
            return Err(UserError::NotFound);
        }
        self.get(id).await
    }

    async fn get(&self, id: Uuid) -> Result<User, UserError> {
        let row = sqlx::query("select id, display_name from users where id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or(UserError::NotFound)?;

        Ok(User {
            id: row.get("id"),
            display_name: row.get("display_name"),
            identities: self.identities(id).await?,
            system_roles: self.system_roles(id).await?,
        })
    }

    async fn create(&self, input: CreateUser) -> Result<User, UserError> {
        validate(&input)?;
        let hash = password::hash(&input.password).map_err(|e| UserError::Internal(e.to_string()))?;
        let email = normalize_email(&input.email);
        let user_id = Uuid::now_v7();

        // The account and its first identity must appear together: an account
        // with no way to sign in is unreachable.
        let mut tx = self.pool.begin().await.map_err(internal)?;

        sqlx::query("insert into users (id, display_name) values ($1, $2)")
            .bind(user_id)
            .bind(input.display_name.trim())
            .execute(&mut *tx)
            .await
            .map_err(internal)?;

        sqlx::query(
            "insert into user_identities \
                 (id, user_id, provider, provider_subject, password_hash) \
             values ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(PROVIDER_PASSWORD)
        .bind(&email)
        .bind(&hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                UserError::DuplicateEmail(email.clone())
            } else {
                internal(e)
            }
        })?;

        tx.commit().await.map_err(internal)?;

        self.get(user_id).await
    }

    async fn delete(&self, id: Uuid) -> Result<(), UserError> {
        let result = sqlx::query("delete from users where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(internal)?;

        if result.rows_affected() == 0 {
            return Err(UserError::NotFound);
        }
        Ok(())
    }

    async fn authenticate(&self, email: &str, password_input: &str) -> Result<User, UserError> {
        // Authentication resolves an identity, then returns the account it
        // belongs to: which address was used does not change what follows.
        let row = sqlx::query(
            "select user_id, password_hash from user_identities \
             where provider = $1 and provider_subject = $2",
        )
        .bind(PROVIDER_PASSWORD)
        .bind(normalize_email(email))
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(UserError::BadCredentials)?;

        let stored: Option<String> = row.get("password_hash");
        let stored = stored.ok_or(UserError::BadCredentials)?;

        let matches = password::verify(password_input, &stored)
            .map_err(|e| UserError::Internal(e.to_string()))?;
        if !matches {
            return Err(UserError::BadCredentials);
        }

        self.get(row.get("user_id")).await
    }

    async fn memberships(&self, user_id: Uuid) -> Result<Vec<TenantMembership>, UserError> {
        // A system admin may sign in to any tenant, so every tenant is listed
        // even where no explicit grant row exists.
        let is_system_admin = self.system_roles(user_id).await?.contains(&Role::SystemAdmin);

        let sql = if is_system_admin {
            "select t.id, t.name, t.slug, \
                    coalesce(array_agg(r.name order by r.name) filter (where r.name is not null), '{}') as roles \
             from tenants t \
             left join user_tenant_roles g on g.tenant_id = t.id and g.user_id = $1 \
             left join roles r on r.tenant_id = g.tenant_id and r.id = g.role_id \
             group by t.id, t.name, t.slug \
             order by t.name"
        } else {
            "select t.id, t.name, t.slug, array_agg(r.name order by r.name) as roles \
             from tenants t \
             join user_tenant_roles g on g.tenant_id = t.id and g.user_id = $1 \
             join roles r on r.tenant_id = g.tenant_id and r.id = g.role_id \
             group by t.id, t.name, t.slug \
             order by t.name"
        };

        let rows = sqlx::query(sql)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;

        Ok(rows
            .iter()
            .map(|r| TenantMembership {
                tenant_id: r.get("id"),
                name: r.get("name"),
                slug: r.get("slug"),
                roles: r.get::<Vec<String>, _>("roles"),
            })
            .collect())
    }

    async fn roles_for_tenant(&self, user_id: Uuid, tenant_id: Uuid) -> Result<Vec<String>, UserError> {
        let mut roles: Vec<String> = sqlx::query_scalar(
            "select r.name from user_tenant_roles g \
             join roles r on r.tenant_id = g.tenant_id and r.id = g.role_id \
             where g.user_id = $1 and g.tenant_id = $2 \
             order by r.name",
        )
        .bind(user_id)
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        // Platform roles apply in whichever tenant the user selects.
        for role in self.system_roles(user_id).await? {
            let name = role.to_string();
            if !roles.contains(&name) {
                roles.push(name);
            }
        }
        Ok(roles)
    }

    async fn add_password_identity(
        &self,
        user_id: Uuid,
        email: &str,
        password_input: &str,
    ) -> Result<Identity, UserError> {
        if password_input.len() < 8 {
            return Err(UserError::Invalid(
                "password must be at least 8 characters".into(),
            ));
        }
        let hash =
            password::hash(password_input).map_err(|e| UserError::Internal(e.to_string()))?;
        let email = normalize_email(email);

        let row = sqlx::query(
            "insert into user_identities \
                 (id, user_id, provider, provider_subject, password_hash) \
             values ($1, $2, $3, $4, $5) \
             returning id, provider, provider_subject, verified_at",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(PROVIDER_PASSWORD)
        .bind(&email)
        .bind(&hash)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                UserError::DuplicateEmail(email.clone())
            } else {
                internal(e)
            }
        })?;

        Ok(read_identity(&row))
    }

    async fn remove_identity(&self, user_id: Uuid, identity_id: Uuid) -> Result<(), UserError> {
        // Removing the last identity would leave an account nobody can sign in
        // to, so the delete is conditional on another one remaining.
        let result = sqlx::query(
            "delete from user_identities \
             where id = $1 and user_id = $2 \
               and (select count(*) from user_identities where user_id = $2) > 1",
        )
        .bind(identity_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(internal)?;

        if result.rows_affected() == 0 {
            // Either it does not exist, or it is the only one left.
            let remaining: i64 =
                sqlx::query_scalar("select count(*) from user_identities where user_id = $1")
                    .bind(user_id)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(internal)?;
            if remaining <= 1 {
                return Err(UserError::Invalid(
                    "cannot remove the only way to sign in".into(),
                ));
            }
            return Err(UserError::IdentityNotFound);
        }
        Ok(())
    }

    async fn grant_system_role(&self, user_id: Uuid, role: Role) -> Result<(), UserError> {
        sqlx::query(
            "insert into user_system_roles (user_id, role) values ($1, $2) \
             on conflict do nothing",
        )
        .bind(user_id)
        .bind(role.to_string())
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn grant_tenant_role(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        role: &str,
    ) -> Result<(), UserError> {
        let inserted = sqlx::query(
            "insert into user_tenant_roles (user_id, tenant_id, role_id) \
             select $1, r.tenant_id, r.id from roles r \
             where r.tenant_id = $2 and r.name = $3 \
             on conflict do nothing",
        )
        .bind(user_id)
        .bind(tenant_id)
        .bind(role)
        .execute(&self.pool)
        .await
        .map_err(|e| {
            // The grant references the user; a user that does not exist is
            // reported as such rather than as a store fault.
            if matches!(&e, sqlx::Error::Database(db) if db.code().as_deref() == Some("23503")) {
                UserError::NotFound
            } else {
                internal(e)
            }
        })?;
        // Zero rows either means the grant already existed or the role does
        // not. Telling them apart takes a second look, and only matters for
        // the error message.
        if inserted.rows_affected() == 0 {
            let exists: bool = sqlx::query_scalar(
                "select exists(select 1 from roles where tenant_id = $1 and name = $2)",
            )
            .bind(tenant_id)
            .bind(role)
            .fetch_one(&self.pool)
            .await
            .map_err(internal)?;
            if !exists {
                return Err(UserError::Invalid(format!("this workspace has no role named {role}")));
            }
        }
        Ok(())
    }

    async fn revoke_tenant_role(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        role: &str,
    ) -> Result<(), UserError> {
        sqlx::query(
            "delete from user_tenant_roles g \
             using roles r \
             where r.tenant_id = g.tenant_id and r.id = g.role_id \
               and g.user_id = $1 and g.tenant_id = $2 and r.name = $3",
        )
        .bind(user_id)
        .bind(tenant_id)
        .bind(role)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn is_empty(&self) -> Result<bool, UserError> {
        let count: i64 = sqlx::query_scalar("select count(*) from users")
            .fetch_one(&self.pool)
            .await
            .map_err(internal)?;
        Ok(count == 0)
    }
}
