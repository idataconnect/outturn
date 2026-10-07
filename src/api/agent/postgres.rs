use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

/// A query built from constant pieces -- the column lists below -- and never
/// from anything a caller sent, which is what `AssertSqlSafe` asks to be told.
macro_rules! sql {
    ($($t:tt)*) => {
        sqlx::AssertSqlSafe(format!($($t)*))
    };
}

use super::{
    Agent, AgentError, AgentStore, CreateAgent, UpdateAgent, validate_name, validate_slug,
};

pub struct PostgresAgentStore {
    pool: PgPool,
}

impl PostgresAgentStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> AgentError {
    AgentError::Internal(e.to_string())
}

fn read_agent(row: &sqlx::postgres::PgRow) -> Agent {
    Agent {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        slug: row.get("slug"),
        description: row.get("description"),
        system_prompt: row.get("system_prompt"),
        policy: row.get("policy"),
        enabled: row.get("enabled"),
        template_id: row.get("template_id"),
        workspace_addition: row.get("workspace_addition"),
    }
}

/// What every read of an agent selects, so a column added is added once.
pub(crate) const COLUMNS: &str = "id, workspace_id, name, slug, description, system_prompt, \
     policy, enabled, template_id, workspace_addition";

#[async_trait]
impl AgentStore for PostgresAgentStore {
    async fn list(
        &self,
        workspace_id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Agent>, AgentError> {
        let rows = sqlx::query(sql!(
            "select {COLUMNS} from agents \
                 where workspace_id = $1 and ($2::uuid is null or id > $2) order by id limit $3"
        ))
        .bind(workspace_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows.iter().map(read_agent).collect())
    }

    async fn get(&self, workspace_id: Uuid, id: Uuid) -> Result<Agent, AgentError> {
        // Scoped by workspace as well as id: an agent belonging to another workspace
        // must read as absent, not as forbidden, so ids cannot be probed.
        let row = sqlx::query(sql!(
            "select {COLUMNS} from agents where workspace_id = $1 and id = $2"
        ))
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(AgentError::NotFound)?;

        Ok(read_agent(&row))
    }

    async fn create(&self, workspace_id: Uuid, input: CreateAgent) -> Result<Agent, AgentError> {
        validate_name(&input.name)?;
        validate_slug(&input.slug)?;

        let row = sqlx::query(sql!(
            "insert into agents (id, workspace_id, name, slug, description, system_prompt, policy) \
             values ($1, $2, $3, $4, $5, $6, coalesce($7, '{{}}'::jsonb)) \
             returning {COLUMNS}"
        ))
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(input.name.trim())
        .bind(&input.slug)
        .bind(input.description.trim())
        .bind(&input.system_prompt)
        .bind(&input.policy)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
                AgentError::DuplicateSlug(input.slug.clone())
            }
            _ => internal(e),
        })?;

        Ok(read_agent(&row))
    }

    async fn update(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        input: UpdateAgent,
    ) -> Result<Agent, AgentError> {
        if let Some(name) = &input.name {
            validate_name(name)?;
        }

        // coalesce leaves omitted fields untouched, so a partial update cannot
        // blank out what the caller did not send.
        let row = sqlx::query(sql!(
            "update agents set \
                 name = coalesce($3, name), \
                 description = coalesce($4, description), \
                 system_prompt = coalesce($5, system_prompt), \
                 policy = coalesce($6, policy), \
                 enabled = coalesce($7, enabled), \
                 workspace_addition = coalesce($8, workspace_addition), \
                 updated_at = now() \
             where workspace_id = $1 and id = $2 \
             returning {COLUMNS}"
        ))
        .bind(workspace_id)
        .bind(id)
        .bind(input.name.as_deref().map(str::trim))
        .bind(input.description.as_deref().map(str::trim))
        .bind(input.system_prompt.as_deref())
        .bind(input.policy)
        .bind(input.enabled)
        .bind(input.workspace_addition.as_deref())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(AgentError::NotFound)?;

        Ok(read_agent(&row))
    }

    async fn delete(&self, workspace_id: Uuid, id: Uuid) -> Result<(), AgentError> {
        let result = sqlx::query("delete from agents where workspace_id = $1 and id = $2")
            .bind(workspace_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(internal)?;

        if result.rows_affected() == 0 {
            return Err(AgentError::NotFound);
        }
        Ok(())
    }
}
