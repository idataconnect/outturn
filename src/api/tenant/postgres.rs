use async_trait::async_trait;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{CreateTenant, Tenant, TenantError, TenantStore, validate};

pub struct PostgresTenantStore {
    pool: PgPool,
}

impl PostgresTenantStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Postgres reports a unique-constraint breach as SQLSTATE 23505; the slug is
/// the only unique column on this table.
fn map_sqlx_error(e: sqlx::Error, slug: &str) -> TenantError {
    if let sqlx::Error::Database(ref db) = e {
        if db.code().as_deref() == Some("23505") {
            return TenantError::DuplicateSlug(slug.to_string());
        }
    }
    TenantError::Internal(e.to_string())
}

#[async_trait]
impl TenantStore for PostgresTenantStore {
    async fn list(&self) -> Result<Vec<Tenant>, TenantError> {
        sqlx::query_as::<_, Tenant>("select id, name, slug from tenants order by name")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| TenantError::Internal(e.to_string()))
    }

    async fn get(&self, id: Uuid) -> Result<Tenant, TenantError> {
        sqlx::query_as::<_, Tenant>("select id, name, slug from tenants where id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| TenantError::Internal(e.to_string()))?
            .ok_or(TenantError::NotFound)
    }

    async fn create(&self, input: CreateTenant) -> Result<Tenant, TenantError> {
        validate(&input)?;
        let name = input.name.trim();

        sqlx::query_as::<_, Tenant>(
            "insert into tenants (id, name, slug) values ($1, $2, $3) \
             returning id, name, slug",
        )
        .bind(Uuid::now_v7())
        .bind(name)
        .bind(&input.slug)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(e, &input.slug))
    }

    async fn rename(&self, id: Uuid, name: &str) -> Result<Tenant, TenantError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TenantError::Invalid("name must not be empty".into()));
        }
        sqlx::query_as::<_, Tenant>(
            "update tenants set name = $2, updated_at = now() where id = $1 \
             returning id, name, slug",
        )
        .bind(id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| TenantError::Internal(e.to_string()))?
        .ok_or(TenantError::NotFound)
    }

    async fn delete(&self, id: Uuid) -> Result<(), TenantError> {
        let result = sqlx::query("delete from tenants where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| TenantError::Internal(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(TenantError::NotFound);
        }
        Ok(())
    }
}
