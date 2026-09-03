use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{AgentSession, ChatError, ChatStore, CreateSession, History, Message, Usage};

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
        role: row.get("role"),
        content: row.get("content"),
        metadata: row.get("metadata"),
        // Only the history read reconstructs this; elsewhere the content is
        // whatever is stored, which by then accounts for every delta.
        delta_next: row.try_get("delta_next").unwrap_or(0),
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

    /// The transcript as of one event cursor.
    ///
    /// Deliberately a single statement: the cursor and the content it accounts
    /// for must come from the same snapshot, or a client polling from the
    /// cursor would either replay a delta already folded into the content or
    /// skip one that was not.
    ///
    /// A reply that is still streaming has no stored content yet, so its text
    /// is assembled from the deltas visible in this snapshot. That is what
    /// makes reconnecting mid-turn resume rather than restart.
    async fn messages(&self, session_id: Uuid) -> Result<History, ChatError> {
        let rows = sqlx::query(
            "with bound as ( \
                 select coalesce( \
                     (select id from events where session_id = $1 order by id desc limit 1), \
                     '00000000-0000-0000-0000-000000000000'::uuid \
                 ) as cursor \
             ), \
             streamed as ( \
                 select (e.payload->>'message_id')::uuid as message_id, \
                        count(*)::int as delta_next, \
                        string_agg(e.payload->>'text', '' order by e.id) as text \
                 from events e, bound \
                 where e.session_id = $1 and e.kind = 'chat.delta' and e.id <= bound.cursor \
                 group by 1 \
             ) \
             select m.id, m.session_id, m.role, m.metadata, \
                    case when m.content = '' then coalesce(s.text, '') else m.content end \
                        as content, \
                    coalesce(s.delta_next, 0) as delta_next, \
                    m.model, m.prompt_tokens, m.completion_tokens, \
                    bound.cursor \
             from agent_messages m \
             cross join bound \
             left join streamed s on s.message_id = m.id \
             where m.session_id = $1 \
             order by m.id",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        // With no messages there is nothing a replay could corrupt, so a nil
        // cursor is safe -- and avoids a second query racing a message that
        // arrives between the two.
        let cursor = rows.first().map(|r| r.get("cursor")).unwrap_or_else(Uuid::nil);

        Ok(History {
            messages: rows.iter().map(read_message).collect(),
            cursor,
        })
    }

    async fn set_message_content(
        &self,
        message_id: Uuid,
        content: &str,
        model: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<Message, ChatError> {
        let row = sqlx::query(
            "update agent_messages \
             set content = $2, model = coalesce($3, model), metadata = $4 \
             where id = $1 \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens",
        )
        .bind(message_id)
        .bind(content)
        .bind(model)
        .bind(metadata)
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

    async fn claim_placeholder(
        &self,
        replies_to: Uuid,
        session_id: Uuid,
    ) -> Result<Message, ChatError> {
        // The unique index on replies_to is what makes this idempotent: a
        // retry of the same turn collides and takes the row it already made,
        // rather than leaving the first behind. Doing it in one statement
        // means a worker that dies mid-way leaves nothing half-done.
        let row = sqlx::query(
            "insert into agent_messages (id, session_id, role, content, replies_to) \
             values ($1, $2, 'assistant', '', $3) \
             on conflict (replies_to) where replies_to is not null \
             do update set replies_to = excluded.replies_to \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens",
        )
        .bind(Uuid::now_v7())
        .bind(session_id)
        .bind(replies_to)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        Ok(read_message(&row))
    }

    async fn discard_placeholder(&self, replies_to: Uuid) -> Result<(), ChatError> {
        // Only while still empty: a turn that failed after writing its reply
        // must not have that reply deleted.
        sqlx::query("delete from agent_messages where replies_to = $1 and content = ''")
            .bind(replies_to)
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
        // An empty assistant message is legitimate only while a job is
        // filling it. One with no live job means a turn died without
        // cleaning up, and writing past it would bury it in the transcript
        // where it is replayed to the model on every later turn.
        let abandoned: Option<Uuid> = sqlx::query_scalar(
            "select m.id from agent_messages m \
             left join jobs j \
                    on (j.payload->>'message_id')::uuid = m.replies_to \
                   and j.state in ('pending', 'running') \
             where m.session_id = $1 and m.role = 'assistant' and m.content = '' \
               and j.id is null \
             limit 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;

        if abandoned.is_some() {
            return Err(ChatError::Abandoned(session_id));
        }

        // Ordering rides on the UUIDv7 key, so there is no sequence to derive
        // and concurrent appends cannot contend over one.
        let row = sqlx::query(
            "insert into agent_messages \
                 (id, session_id, role, content, model, prompt_tokens, completion_tokens) \
             values ($1, $2, $3, $4, $5, $6, $7) \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens",
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
