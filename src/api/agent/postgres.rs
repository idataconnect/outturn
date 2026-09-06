use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

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
        tenant_id: row.get("tenant_id"),
        name: row.get("name"),
        slug: row.get("slug"),
        description: row.get("description"),
        system_prompt: row.get("system_prompt"),
        policy: row.get("policy"),
        enabled: row.get("enabled"),
    }
}

#[async_trait]
impl AgentStore for PostgresAgentStore {
    async fn list(&self, tenant_id: Uuid) -> Result<Vec<Agent>, AgentError> {
        let rows = sqlx::query(
            "select id, tenant_id, name, slug, description, system_prompt, policy, enabled from agents where tenant_id = $1 order by name",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows.iter().map(read_agent).collect())
    }

    async fn get(&self, tenant_id: Uuid, id: Uuid) -> Result<Agent, AgentError> {
        // Scoped by tenant as well as id: an agent belonging to another tenant
        // must read as absent, not as forbidden, so ids cannot be probed.
        let row = sqlx::query(
            "select id, tenant_id, name, slug, description, system_prompt, policy, enabled from agents where tenant_id = $1 and id = $2",
        )
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(AgentError::NotFound)?;

        Ok(read_agent(&row))
    }

    async fn create(&self, tenant_id: Uuid, input: CreateAgent) -> Result<Agent, AgentError> {
        validate_name(&input.name)?;
        validate_slug(&input.slug)?;

        let row = sqlx::query(
            "insert into agents (id, tenant_id, name, slug, description, system_prompt, policy) \
             values ($1, $2, $3, $4, $5, $6, coalesce($7, '{}'::jsonb)) \
             returning id, tenant_id, name, slug, description, system_prompt, policy, enabled",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_id)
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
        tenant_id: Uuid,
        id: Uuid,
        input: UpdateAgent,
    ) -> Result<Agent, AgentError> {
        if let Some(name) = &input.name {
            validate_name(name)?;
        }

        // coalesce leaves omitted fields untouched, so a partial update cannot
        // blank out what the caller did not send.
        let row = sqlx::query(
            "update agents set \
                 name = coalesce($3, name), \
                 description = coalesce($4, description), \
                 system_prompt = coalesce($5, system_prompt), \
                 policy = coalesce($6, policy), \
                 enabled = coalesce($7, enabled), \
                 updated_at = now() \
             where tenant_id = $1 and id = $2 \
             returning id, tenant_id, name, slug, description, system_prompt, policy, enabled",
        )
        .bind(tenant_id)
        .bind(id)
        .bind(input.name.as_deref().map(str::trim))
        .bind(input.description.as_deref().map(str::trim))
        .bind(input.system_prompt.as_deref())
        .bind(input.policy)
        .bind(input.enabled)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(AgentError::NotFound)?;

        Ok(read_agent(&row))
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), AgentError> {
        let result = sqlx::query("delete from agents where tenant_id = $1 and id = $2")
            .bind(tenant_id)
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
