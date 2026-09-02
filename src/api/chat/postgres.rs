use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{AgentSession, ChatError, ChatStore, CreateSession, Message, Usage};

pub struct PostgresChatStore {
    pool: PgPool,
}

impl PostgresChatStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> ChatError {
    ChatError::Internal(e.to_string())
}

fn read_session(row: &sqlx::postgres::PgRow) -> AgentSession {
    AgentSession {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        agent_id: row.get("agent_id"),
        title: row.get("title"),
    }
}

fn read_message(row: &sqlx::postgres::PgRow) -> Message {
    Message {
        id: row.get("id"),
        session_id: row.get("session_id"),
        seq: row.get("seq"),
        role: row.get("role"),
        content: row.get("content"),
        model: row.get("model"),
        prompt_tokens: row.get("prompt_tokens"),
        completion_tokens: row.get("completion_tokens"),
    }
}

#[async_trait]
impl ChatStore for PostgresChatStore {
    async fn create_session(
        &self,
        tenant_id: Uuid,
        user_id: Uuid,
        input: CreateSession,
    ) -> Result<AgentSession, ChatError> {
        // The agent lookup is tenant-scoped, so a session cannot be opened
        // against another tenant's agent by supplying its id.
        let exists: Option<Uuid> =
            sqlx::query_scalar("select id from agents where id = $1 and tenant_id = $2")
                .bind(input.agent_id)
                .bind(tenant_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        if exists.is_none() {
            return Err(ChatError::NotFound);
        }

        let row = sqlx::query(
            "insert into agent_sessions (id, tenant_id, agent_id, user_id, title) \
             values ($1, $2, $3, $4, $5) \
             returning id, tenant_id, agent_id, title",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_id)
        .bind(input.agent_id)
        .bind(user_id)
        .bind(input.title.trim())
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        Ok(read_session(&row))
    }

    async fn list_sessions(&self, tenant_id: Uuid) -> Result<Vec<AgentSession>, ChatError> {
        let rows = sqlx::query(
            "select id, tenant_id, agent_id, title from agent_sessions \
             where tenant_id = $1 order by created_at desc",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows.iter().map(read_session).collect())
    }

    async fn get_session(
        &self,
        tenant_id: Uuid,
        session_id: Uuid,
    ) -> Result<AgentSession, ChatError> {
        let row = sqlx::query(
            "select id, tenant_id, agent_id, title from agent_sessions \
             where tenant_id = $1 and id = $2",
        )
        .bind(tenant_id)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(ChatError::NotFound)?;

        Ok(read_session(&row))
    }

    async fn delete_session(&self, tenant_id: Uuid, session_id: Uuid) -> Result<(), ChatError> {
        let result =
            sqlx::query("delete from agent_sessions where tenant_id = $1 and id = $2")
                .bind(tenant_id)
                .bind(session_id)
                .execute(&self.pool)
                .await
                .map_err(internal)?;

        if result.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        Ok(())
    }

    async fn messages(&self, session_id: Uuid) -> Result<Vec<Message>, ChatError> {
        let rows = sqlx::query(
            "select id, session_id, seq, role, content, model, prompt_tokens, \
                    completion_tokens \
             from agent_messages where session_id = $1 order by seq",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows.iter().map(read_message).collect())
    }

    async fn set_message_content(
        &self,
        message_id: Uuid,
        content: &str,
        model: Option<&str>,
    ) -> Result<Message, ChatError> {
        let row = sqlx::query(
            "update agent_messages set content = $2, model = coalesce($3, model) \
             where id = $1 \
             returning id, session_id, seq, role, content, model, prompt_tokens, \
                       completion_tokens",
        )
        .bind(message_id)
        .bind(content)
        .bind(model)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(ChatError::NotFound)?;

        Ok(read_message(&row))
    }

    async fn delete_message(&self, message_id: Uuid) -> Result<(), ChatError> {
        sqlx::query("delete from agent_messages where id = $1")
            .bind(message_id)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(())
    }

    async fn append_message(
        &self,
        session_id: Uuid,
        role: &str,
        content: &str,
        model: Option<&str>,
        usage: Usage,
    ) -> Result<Message, ChatError> {
        // The sequence is derived inside the insert so two concurrent appends
        // cannot pick the same number; the unique constraint is the backstop.
        let row = sqlx::query(
            "insert into agent_messages \
                 (id, session_id, seq, role, content, model, prompt_tokens, completion_tokens) \
             select $1, $2, \
                    coalesce((select max(seq) from agent_messages where session_id = $2), 0) + 1, \
                    $3, $4, $5, $6, $7 \
             returning id, session_id, seq, role, content, model, prompt_tokens, \
                       completion_tokens",
        )
        .bind(Uuid::now_v7())
        .bind(session_id)
        .bind(role)
        .bind(content)
        .bind(model)
        .bind(usage.prompt_tokens)
        .bind(usage.completion_tokens)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        Ok(read_message(&row))
    }
}
