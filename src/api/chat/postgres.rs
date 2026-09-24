use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{
    AgentSession, ChatError, ChatStore, CreateSession, Delivery, History, Message, Placeholder,
    Usage,
};

pub struct PostgresChatStore {
    pool: PgPool,
}

impl PostgresChatStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// One statement behind both the whole transcript and a page of it.
    ///
    /// `limit` absent means everything, which is what a turn is built from;
    /// present means the newest `limit` messages older than `before`, which is
    /// what a reader is served. A null limit is unlimited in postgres, so the
    /// same statement serves both without a second spelling of it.
    ///
    /// The window is chosen first and everything else hangs off it. That is
    /// the point of the shape rather than tidiness: the delta aggregation and
    /// the per-message job lookup are the expensive parts, so scoping them to
    /// the window is what makes a page cheaper than the transcript. Bounding
    /// the returned rows alone would still scan every event the session ever
    /// emitted, which is most of the cost.
    ///
    /// The deltas are bounded by the window's own id range rather than by a
    /// partial index on `kind`. A delta is written after the message it
    /// belongs to, and both are UUIDv7, so no delta for a message in the
    /// window can sort below the window's oldest message -- which makes the
    /// lower bound free of a scan and, unlike an index, free of a second copy
    /// of the table. A partial index was measured and does not earn its keep:
    /// deltas are ~84% of all events here, so indexing them indexes almost
    /// everything, and the cost is the heap access an index path still pays.
    /// Bounding by id took the same read from 2,000 buffers to 209.
    ///
    /// Deliberately a single statement: the cursor and the content it accounts
    /// for must come from the same snapshot, or a client polling from the
    /// cursor would either replay a delta already folded into the content or
    /// skip one that was not. `has_more` rides along for the same reason --
    /// answered separately it could disagree with the rows beside it.
    ///
    /// A reply that is still streaming has no stored content yet, so its text
    /// is assembled from the deltas visible in this snapshot. That is what
    /// makes reconnecting mid-turn resume rather than restart.
    async fn read_history(
        &self,
        session_id: Uuid,
        before: Option<Uuid>,
        limit: Option<i64>,
    ) -> Result<History, ChatError> {
        let rows = sqlx::query(
            "with bound as ( \
                 select coalesce( \
                     (select id from events where session_id = $1 order by id desc limit 1), \
                     '00000000-0000-0000-0000-000000000000'::uuid \
                 ) as cursor \
             ), \
             win as ( \
                 select id from agent_messages \
                 where session_id = $1 \
                   and ($2::uuid is null or id < $2) \
                 order by id desc \
                 limit $3::bigint \
             ), \
             more as ( \
                 select exists ( \
                     select 1 from agent_messages \
                     where session_id = $1 \
                       and id < (select id from win order by id limit 1) \
                 ) as has_more \
             ), \
             streamed as ( \
                 select (e.payload->>'message_id')::uuid as message_id, \
                        (count(*) filter (where e.kind = 'chat.delta'))::int as delta_next, \
                        coalesce(string_agg(e.payload->>'text', '' order by e.id) \
                            filter (where e.kind = 'chat.delta'), '') as text, \
                        jsonb_agg(jsonb_build_object('kind', e.kind, 'payload', e.payload) \
                            order by e.id) as events \
                 from events e, bound \
                 where e.session_id = $1 \
                   and e.kind in ('chat.delta', 'chat.tool', 'chat.tool_result') \
                   and e.id <= bound.cursor \
                   and e.id > (select id from win order by id limit 1) \
                   and (e.payload->>'message_id')::uuid in ( \
                       select m.id from agent_messages m \
                       join win w on w.id = m.id \
                       where m.role = 'assistant' and m.content = '' \
                         and not m.metadata ? 'tool_calls') \
                 group by 1 \
             ) \
             select m.id, m.session_id, m.role, m.metadata, \
                    case when m.content = '' then coalesce(s.text, '') else m.content end \
                        as content, \
                    coalesce(s.delta_next, 0) as delta_next, s.events as streamed, \
                    m.model, m.prompt_tokens, m.completion_tokens, \
                    m.replies_to, m.absorbed_by, \
                    case when m.role = 'user' then ( \
                        select j.state from jobs j \
                        where j.kind = 'chat.turn' \
                          and (j.payload->>'message_id')::uuid = m.id \
                        order by j.id desc limit 1) end as job_state, \
                    bound.cursor, more.has_more \
             from agent_messages m \
             join win w on w.id = m.id \
             cross join bound \
             cross join more \
             left join streamed s on s.message_id = m.id \
             order by m.id",
        )
        .bind(session_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        // With no messages there is nothing a replay could corrupt, so a nil
        // cursor is safe -- and avoids a second query racing a message that
        // arrives between the two. Nothing read means nothing older either:
        // `more` has no window to compare against.
        let cursor = rows
            .first()
            .map(|r| r.get("cursor"))
            .unwrap_or_else(Uuid::nil);
        let has_more = rows.first().map(|r| r.get("has_more")).unwrap_or(false);

        Ok(History {
            messages: rows
                .iter()
                .map(|row| {
                    let mut message = read_message(row);
                    if let Ok(Some(events)) =
                        row.try_get::<Option<serde_json::Value>, _>("streamed")
                    {
                        message.metadata = replay(message.metadata, &events);
                    }
                    message
                })
                .collect(),
            cursor,
            has_more,
        })
    }
}

/// A reply still being written, rebuilt from the events it has produced.
///
/// Its row is empty until the turn ends, so the text comes back from the
/// deltas -- and the tool calls, and the order they fell between the text,
/// come back from the same events, folded the way the browser folds them
/// live. Without this a reload mid-turn showed the words and lost every round
/// of work between them, until the turn finished and put them all back.
fn replay(metadata: serde_json::Value, events: &serde_json::Value) -> serde_json::Value {
    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut parts: Vec<serde_json::Value> = Vec::new();
    for event in events.as_array().into_iter().flatten() {
        let payload = &event["payload"];
        match event["kind"].as_str() {
            Some("chat.delta") => {
                let text = payload["text"].as_str().unwrap_or_default();
                match parts.last_mut() {
                    Some(last) if last["type"] == "text" => {
                        let joined = format!("{}{text}", last["text"].as_str().unwrap_or_default());
                        last["text"] = serde_json::json!(joined);
                    }
                    _ => parts.push(serde_json::json!({ "type": "text", "text": text })),
                }
            }
            Some("chat.tool") => {
                let call = &payload["call"];
                if calls.iter().any(|c| c["id"] == call["id"]) {
                    continue;
                }
                parts.push(serde_json::json!({ "type": "call", "id": call["id"] }));
                calls.push(call.clone());
            }
            Some("chat.tool_result") => {
                if let Some(call) = calls.iter_mut().find(|c| c["id"] == payload["id"]) {
                    call["details"] = payload["details"].clone();
                    call["is_error"] = payload["is_error"].clone();
                }
            }
            _ => {}
        }
    }
    if calls.is_empty() {
        return metadata;
    }
    let mut metadata = match metadata {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    metadata.insert("tool_calls".into(), serde_json::Value::Array(calls));
    metadata.insert("parts".into(), serde_json::Value::Array(parts));
    serde_json::Value::Object(metadata)
}

fn internal(e: sqlx::Error) -> ChatError {
    ChatError::Internal(e.to_string())
}

fn read_session(row: &sqlx::postgres::PgRow) -> AgentSession {
    AgentSession {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        agent_id: row.get("agent_id"),
        user_id: row.get("user_id"),
        title: row.get("title"),
        account: row.try_get("account").ok().flatten(),
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
        replies_to: row.try_get("replies_to").ok().flatten(),
        absorbed_by: row.try_get("absorbed_by").ok().flatten(),
        job_state: row.try_get("job_state").ok().flatten(),
    }
}

#[async_trait]
impl ChatStore for PostgresChatStore {
    async fn create_session(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
        input: CreateSession,
    ) -> Result<AgentSession, ChatError> {
        // The agent lookup is workspace-scoped, so a session cannot be opened
        // against another workspace's agent by supplying its id.
        let exists: Option<Uuid> =
            sqlx::query_scalar("select id from agents where id = $1 and workspace_id = $2")
                .bind(input.agent_id)
                .bind(workspace_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;
        if exists.is_none() {
            return Err(ChatError::NotFound);
        }

        let row = sqlx::query(
            "insert into agent_sessions (id, workspace_id, agent_id, user_id, title, account) \
             values ($1, $2, $3, $4, $5, $6) \
             returning id, workspace_id, agent_id, user_id, title, account",
        )
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(input.agent_id)
        .bind(user_id)
        .bind(input.title.trim())
        .bind(
            input
                .account
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty()),
        )
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        Ok(read_session(&row))
    }

    async fn list_sessions(&self, workspace_id: Uuid) -> Result<Vec<AgentSession>, ChatError> {
        let rows = sqlx::query(
            "select id, workspace_id, agent_id, user_id, title, account from agent_sessions \
             where workspace_id = $1 order by created_at desc",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows.iter().map(read_session).collect())
    }

    async fn get_session(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
    ) -> Result<AgentSession, ChatError> {
        let row = sqlx::query(
            "select id, workspace_id, agent_id, user_id, title, account from agent_sessions \
             where workspace_id = $1 and id = $2",
        )
        .bind(workspace_id)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(ChatError::NotFound)?;

        Ok(read_session(&row))
    }

    async fn rename_session(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        title: &str,
    ) -> Result<AgentSession, ChatError> {
        let row = sqlx::query(
            "update agent_sessions set title = $3, updated_at = now() \
             where workspace_id = $1 and id = $2 \
             returning id, workspace_id, agent_id, user_id, title, account",
        )
        .bind(workspace_id)
        .bind(session_id)
        .bind(title.trim())
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(ChatError::NotFound)?;

        Ok(read_session(&row))
    }

    async fn delete_session(&self, workspace_id: Uuid, session_id: Uuid) -> Result<(), ChatError> {
        let result = sqlx::query("delete from agent_sessions where workspace_id = $1 and id = $2")
            .bind(workspace_id)
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
        self.read_history(session_id, None, None).await
    }

    async fn messages_page(
        &self,
        session_id: Uuid,
        before: Option<Uuid>,
        limit: i64,
    ) -> Result<History, ChatError> {
        // Summaries are served, not hidden. The reader's complaint was never
        // that a summary is here -- it is that an unmarked one reads as
        // something the agent said to them. AGENTS.md is explicit that the
        // mark is what fixes that, and for two reasons: "both so a reader can
        // see what happened, and so the next compaction knows it is compacting
        // a summary". Withholding it would leave a person unable to see that
        // their conversation had been compacted at all, which is the quiet
        // bound that document warns against twice.
        //
        // The mark travels in `metadata` and the client renders it as a
        // summary. Filtering here also short-changed the page: the rows come
        // back under a SQL `limit`, so dropping some after the fact returns
        // fewer than asked for while `has_more` still counts them.
        self.read_history(session_id, before, Some(limit)).await
    }

    async fn set_message_content(
        &self,
        message_id: Uuid,
        content: &str,
        model: Option<&str>,
        provider: Option<&str>,
        usage: Usage,
        metadata: serde_json::Value,
    ) -> Result<Message, ChatError> {
        let row = sqlx::query(
            "update agent_messages \
             set content = $2, model = coalesce($3, model), metadata = $4, \
                 provider = coalesce($5, provider), \
                 prompt_tokens = coalesce($6, prompt_tokens), \
                 completion_tokens = coalesce($7, completion_tokens), \
                 cache_read_tokens = coalesce($8, cache_read_tokens), \
                 cache_write_tokens = coalesce($9, cache_write_tokens), \
                 reasoning_tokens = coalesce($10, reasoning_tokens) \
             where id = $1 \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens, replies_to, absorbed_by",
        )
        .bind(message_id)
        .bind(content)
        .bind(model)
        .bind(metadata)
        .bind(provider)
        .bind(usage.prompt_tokens)
        .bind(usage.completion_tokens)
        .bind(usage.cache_read_tokens)
        .bind(usage.cache_write_tokens)
        .bind(usage.reasoning_tokens)
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
    ) -> Result<Placeholder, ChatError> {
        // The unique index on replies_to is what makes this idempotent: a
        // retry of the same turn collides and takes the row it already made,
        // rather than leaving the first behind. Doing it in one statement
        // means a worker that dies mid-way leaves nothing half-done.
        //
        // `xmax = 0` distinguishes the insert from the conflict, which is what
        // lets the caller announce the reply once rather than once per attempt.
        let row = sqlx::query(
            "insert into agent_messages (id, session_id, role, content, replies_to) \
             values ($1, $2, 'assistant', '', $3) \
             on conflict (replies_to) where replies_to is not null \
             do update set replies_to = excluded.replies_to \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens, replies_to, absorbed_by, \
                       (xmax = 0) as created",
        )
        .bind(Uuid::now_v7())
        .bind(session_id)
        .bind(replies_to)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        Ok(Placeholder {
            created: row.try_get("created").unwrap_or(true),
            message: read_message(&row),
        })
    }

    async fn was_absorbed(&self, message_id: Uuid) -> Result<bool, ChatError> {
        let absorbed: Option<Uuid> =
            sqlx::query_scalar("select absorbed_by from agent_messages where id = $1")
                .bind(message_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?
                .flatten();
        Ok(absorbed.is_some())
    }

    async fn stop_session(&self, session_id: Uuid, reason: &str) -> Result<(), ChatError> {
        // The first stop wins. A session already stopped keeps the reason it
        // was stopped for: the second hold to arrive did not stop anything, and
        // overwriting would tell the next turn the wrong story about why its
        // reply broke off.
        sqlx::query(
            "update agent_sessions              set stopped_at = now(), stopped_reason = $2, updated_at = now()              where id = $1 and stopped_at is null",
        )
        .bind(session_id)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn clear_stop(&self, session_id: Uuid) -> Result<Option<super::Stopped>, ChatError> {
        // Returns what it cleared, so a turn can tell the model why the reply
        // above it stops mid-sentence. Nothing comes back when the session was
        // not stopped, which is the ordinary case and not an error.
        // The old reason, read from a subquery rather than from `returning`:
        // `returning` hands back the row as it now is, and as it now is the
        // reason has just been set to null.
        let was: Option<(Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "update agent_sessions s \
             set stopped_at = null, stopped_reason = null, updated_at = now() \
             from (select id, stopped_reason, stopped_at from agent_sessions where id = $1) old \
             where s.id = old.id and s.stopped_at is not null \
             returning old.stopped_reason, old.stopped_at",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        Ok(was.map(|(reason, at)| super::Stopped {
            reason: reason.unwrap_or_default(),
            at,
        }))
    }

    async fn stopped_reason(&self, session_id: Uuid) -> Result<Option<String>, ChatError> {
        let reason: Option<Option<String>> = sqlx::query_scalar(
            "select stopped_reason from agent_sessions where id = $1 and stopped_at is not null",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        Ok(reason.flatten())
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
        delivery: Delivery,
        user_id: Option<Uuid>,
    ) -> Result<Message, ChatError> {
        // An empty assistant message is legitimate while a job is filling
        // it, and afterwards if the job finished -- a reply can legitimately
        // be nothing but a tool call, and the model saying nothing after is
        // its choice, not a fault. What is abandoned is an empty reply whose
        // turn neither runs nor ever finished: a turn that died without
        // cleaning up, which writing past would bury in the transcript to be
        // replayed to the model on every later turn.
        //
        // This once refused any empty reply with no live job, which wedged a
        // session for good the first time a tool failed and the model went
        // quiet: the reply was empty, the job had succeeded, and every later
        // message was refused.
        //
        // Every state that means "this turn was accounted for" belongs in the
        // list below, not only the ones that mean it went well. A turn stopped
        // before its first token leaves exactly the shape this looks for -- an
        // empty reply, no running job -- and reading that as abandonment
        // wedges the session in the same way, by a different route.
        let abandoned: Option<Uuid> = sqlx::query_scalar(
            "select m.id from agent_messages m \
             left join jobs j \
                    on (j.payload->>'message_id')::uuid = m.replies_to \
                   and j.state in ('pending', 'running', 'succeeded', 'cancelled') \
             where m.session_id = $1 and m.role = 'assistant' and m.content = '' \
               and coalesce(jsonb_array_length(m.metadata->'tool_calls'), 0) = 0 \
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
                 (id, session_id, role, content, model, prompt_tokens, completion_tokens, \
                  delivery, user_id) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens, replies_to, absorbed_by",
        )
        .bind(Uuid::now_v7())
        .bind(session_id)
        .bind(role)
        .bind(content)
        .bind(model)
        .bind(usage.prompt_tokens)
        .bind(usage.completion_tokens)
        .bind(delivery.as_str())
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        Ok(read_message(&row))
    }
}

#[cfg(test)]
mod tests {
    use super::replay;
    use serde_json::json;

    #[test]
    fn a_reload_mid_turn_keeps_the_rounds_in_order() {
        let events = json!([
            { "kind": "chat.delta", "payload": { "text": "Let me " } },
            { "kind": "chat.delta", "payload": { "text": "look." } },
            { "kind": "chat.tool", "payload": { "call": { "id": "c1", "name": "fetch_url" } } },
            { "kind": "chat.tool_result", "payload": { "id": "c1", "details": "200", "is_error": false } },
            // A poll overlapping a reload can see the same call twice.
            { "kind": "chat.tool", "payload": { "call": { "id": "c1", "name": "fetch_url" } } },
            { "kind": "chat.delta", "payload": { "text": "\n\nIt says" } },
        ]);

        let metadata = replay(json!({}), &events);

        assert_eq!(
            metadata["parts"],
            json!([
                { "type": "text", "text": "Let me look." },
                { "type": "call", "id": "c1" },
                { "type": "text", "text": "\n\nIt says" },
            ])
        );
        assert_eq!(
            metadata["tool_calls"],
            json!([{ "id": "c1", "name": "fetch_url", "details": "200", "is_error": false }])
        );
    }

    #[test]
    fn text_alone_leaves_the_metadata_as_it_was() {
        let events = json!([{ "kind": "chat.delta", "payload": { "text": "hi" } }]);
        assert_eq!(replay(json!({}), &events), json!({}));
    }
}
