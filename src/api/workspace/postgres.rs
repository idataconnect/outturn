use async_trait::async_trait;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{CreateWorkspace, Workspace, WorkspaceError, WorkspaceStore, validate};

pub struct PostgresWorkspaceStore {
    pool: PgPool,
}

impl PostgresWorkspaceStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Postgres reports a unique-constraint breach as SQLSTATE 23505; the slug is
/// the only unique column on this table.
fn map_sqlx_error(e: sqlx::Error, slug: &str) -> WorkspaceError {
    if let sqlx::Error::Database(ref db) = e
        && db.code().as_deref() == Some("23505")
    {
        return WorkspaceError::DuplicateSlug(slug.to_string());
    }
    WorkspaceError::Internal(e.to_string())
}

#[async_trait]
impl WorkspaceStore for PostgresWorkspaceStore {
    async fn list(&self) -> Result<Vec<Workspace>, WorkspaceError> {
        sqlx::query_as::<_, Workspace>("select id, name, slug from workspaces order by name")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| WorkspaceError::Internal(e.to_string()))
    }

    async fn get(&self, id: Uuid) -> Result<Workspace, WorkspaceError> {
        sqlx::query_as::<_, Workspace>("select id, name, slug from workspaces where id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| WorkspaceError::Internal(e.to_string()))?
            .ok_or(WorkspaceError::NotFound)
    }

    async fn create(&self, input: CreateWorkspace) -> Result<Workspace, WorkspaceError> {
        validate(&input)?;
        let name = input.name.trim();

        sqlx::query_as::<_, Workspace>(
            "insert into workspaces (id, name, slug) values ($1, $2, $3) \
             returning id, name, slug",
        )
        .bind(Uuid::now_v7())
        .bind(name)
        .bind(&input.slug)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_sqlx_error(e, &input.slug))
    }

    async fn rename(&self, id: Uuid, name: &str) -> Result<Workspace, WorkspaceError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(WorkspaceError::Invalid("name must not be empty".into()));
        }
        sqlx::query_as::<_, Workspace>(
            "update workspaces set name = $2, updated_at = now() where id = $1 \
             returning id, name, slug",
        )
        .bind(id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| WorkspaceError::Internal(e.to_string()))?
        .ok_or(WorkspaceError::NotFound)
    }

    async fn delete(&self, id: Uuid) -> Result<(), WorkspaceError> {
        let result = sqlx::query("delete from workspaces where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| WorkspaceError::Internal(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(WorkspaceError::NotFound);
        }
        Ok(())
    }
}
