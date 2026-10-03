use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{
    AgentSession, ChatError, ChatStore, CreateSession, DECLINED_GUIDANCE, Delivery, History,
    Message, Placeholder, Usage,
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
                        jsonb_agg(jsonb_build_object( \
                            'kind', e.kind, 'payload', e.payload, 'at', e.created_at) \
                            order by e.id) as events \
                 from events e, bound \
                 where e.session_id = $1 \
                   and e.kind in ('chat.delta', 'chat.reasoning', 'chat.tool', \
                                  'chat.tool_result', 'chat.steer') \
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
                    m.replies_to, m.absorbed_by, m.attempt, m.finished_at, \
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

        // What this conversation is waiting on, if anything. Read here rather
        // than left to the live event, which a reloaded tab has already missed:
        // through the hold, because the hold is what says a request is still
        // open, and scoped to this session so a workspace's other pending
        // approvals are not somebody else's business.
        //
        // Through `actions::approval_on_session` and `gated::answerable` rather
        // than a query and a projection of its own. Those existed here first and
        // were a hand-copy: the `kind like 'approval.%'` filter was added to
        // this one and not to the shared method, so the reload banner and the
        // live banner disagreed about what an approval is. Two spellings that
        // must stay byte-identical is not a thing to keep.
        //
        // The workspace comes from the session, because a transcript read is
        // reached by session id alone.
        let workspace_id: Option<Uuid> =
            sqlx::query_scalar("select workspace_id from agent_sessions where id = $1")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(internal)?;

        use super::super::actions::ActionStore as _;
        let awaiting = match workspace_id {
            Some(workspace_id) => {
                super::super::actions::PostgresActionStore::new(self.pool.clone())
                    .approval_on_session(workspace_id, session_id)
                    .await
                    .map_err(|e| ChatError::Internal(e.to_string()))?
                    .map(|item| super::super::gated::answerable(&item))
            }
            None => None,
        };

        // And whether it is asleep, for the same reason.
        let asleep = match workspace_id {
            Some(workspace_id) => super::super::wake::asleep(&self.pool, workspace_id, session_id)
                .await
                .map_err(internal)?,
            None => None,
        };

        Ok(History {
            awaiting,
            asleep,
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

impl PostgresChatStore {
    /// Writes an interrupted attempt down from what it streamed, and says
    /// whether there was anything to write.
    ///
    /// The row of a reply still streaming is empty; its words, calls and
    /// thoughts are events until the turn finishes. A turn that never finishes
    /// -- its lease reaped, its pod gone -- would otherwise stay that way, and
    /// every predicate that reads the row (`said_something`, the discard, the
    /// abandoned-placeholder guard) would call it empty while the reader had
    /// watched it work. Folded by `replay`, the same fold a reload mid-turn uses,
    /// so what is kept is what was on screen.
    ///
    /// Marked `interrupted`, because it is not a reply that ended: it is the
    /// record of one that stopped, and the attempt after it is the answer.
    async fn seal_interrupted(&self, message_id: Uuid) -> Result<bool, ChatError> {
        let events: serde_json::Value = sqlx::query_scalar(
            "select coalesce(jsonb_agg(jsonb_build_object( \
                        'kind', e.kind, 'payload', e.payload, 'at', e.created_at) \
                        order by e.id), '[]'::jsonb) \
             from agent_messages m \
             join events e on e.session_id = m.session_id \
             where m.id = $1 \
               and e.kind in ('chat.delta', 'chat.reasoning', 'chat.tool', \
                              'chat.tool_result', 'chat.steer') \
               and (e.payload->>'message_id')::uuid = m.id",
        )
        .bind(message_id)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        let said = events.as_array().is_some_and(|events| {
            events.iter().any(|e| {
                matches!(
                    e["kind"].as_str(),
                    Some("chat.delta" | "chat.tool" | "chat.reasoning")
                )
            })
        });
        if !said {
            return Ok(false);
        }

        let content: String = events
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["kind"] == "chat.delta")
            .filter_map(|e| e["payload"]["text"].as_str())
            .collect();
        let mut metadata = replay(serde_json::json!({}), &events);
        metadata["interrupted"] = serde_json::Value::Bool(true);

        // `finished_at is null`: a turn that did finish in the meantime wrote
        // the better record, and this one must not replace it.
        sqlx::query(
            "update agent_messages \
             set content = $2, metadata = metadata || $3, finished_at = now() \
             where id = $1 and finished_at is null",
        )
        .bind(message_id)
        .bind(content)
        .bind(metadata)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(true)
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
    // The same builder the live path assembles with, so a reload rebuilds the
    // message the browser already drew rather than a differently-shaped one.
    let mut parts = super::parts::Builder::new();
    for event in events.as_array().into_iter().flatten() {
        let payload = &event["payload"];
        match event["kind"].as_str() {
            Some("chat.delta") => {
                parts.text(payload["text"].as_str().unwrap_or_default());
            }
            Some("chat.tool") => {
                let call = &payload["call"];
                if calls.iter().any(|c| c["id"] == call["id"]) {
                    continue;
                }
                parts.call(call["id"].as_str().unwrap_or_default());
                calls.push(call.clone());
            }
            Some("chat.reasoning") => {
                // The event's own time, so a reload reports the duration the
                // live path measured rather than the time of the reload.
                let at = event["at"]
                    .as_str()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&chrono::Utc));
                parts.reasoning(payload["text"].as_str().unwrap_or_default(), at);
            }
            Some("chat.steer") => {
                parts.steer(payload["id"].as_str().unwrap_or_default());
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
    // A run of plain prose says nothing the flat content does not, so it is
    // left alone. Anything else -- a call, or a point the model stopped to
    // think -- is an arrangement that only `parts` records.
    if parts
        .parts()
        .iter()
        .all(|p| matches!(p, super::parts::Part::Text { .. }))
    {
        return metadata;
    }
    let mut metadata = match metadata {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    metadata.insert("tool_calls".into(), serde_json::Value::Array(calls));
    metadata.insert("parts".into(), parts.to_json());
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
        attempt: row.try_get("attempt").unwrap_or(1),
        finished_at: row.try_get("finished_at").ok().flatten(),
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

    async fn list_sessions(
        &self,
        workspace_id: Uuid,
        agent_ids: Option<&[Uuid]>,
        user_id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<AgentSession>, ChatError> {
        let rows = sqlx::query(
            "select id, workspace_id, agent_id, user_id, title, account from agent_sessions \
             where workspace_id = $1 \
               and ($2::uuid[] is null or agent_id = any($2) or user_id = $3) \
               and ($4::uuid is null or id < $4) \
             order by id desc \
             limit $5",
        )
        .bind(workspace_id)
        .bind(agent_ids)
        .bind(user_id)
        .bind(after)
        .bind(limit)
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
                 reasoning_tokens = coalesce($10, reasoning_tokens), \
                 finished_at = now() \
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
        attempt: i32,
    ) -> Result<Placeholder, ChatError> {
        // Idempotent per *attempt*. A retry of the same attempt collides and
        // takes the row it already made, which is what makes a worker dying
        // mid-way leave nothing half-done -- the property the single-reply index
        // used to provide, now keyed on `(replies_to, attempt)`.
        //
        // What changed is that a turn resuming after an approval asks for the
        // next attempt instead, so the refused reply stays where it is. Taking
        // it back would overwrite the refusal the reader approved against, and
        // leave a transcript showing a charge that succeeded with nothing in it
        // that ever needed approving.
        //
        // `xmax = 0` distinguishes the insert from the conflict, which is what
        // lets the caller announce the reply once rather than once per attempt.
        let row = sqlx::query(
            "insert into agent_messages \
                 (id, session_id, role, content, replies_to, attempt) \
             values ($1, $2, 'assistant', '', $3, $4) \
             on conflict (replies_to, attempt) where replies_to is not null \
             do update set replies_to = excluded.replies_to \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens, replies_to, absorbed_by, \
                       (xmax = 0) as created",
        )
        .bind(Uuid::now_v7())
        .bind(session_id)
        .bind(replies_to)
        .bind(attempt)
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

    async fn attempt_for(&self, replies_to: Uuid, resuming: bool) -> Result<i32, ChatError> {
        let latest: Option<(Uuid, i32, bool, bool)> = sqlx::query_as(
            "select id, attempt, finished_at is not null, \
                    said_something(content, metadata) \
             from agent_messages where replies_to = $1 \
             order by attempt desc limit 1",
        )
        .bind(replies_to)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;

        Ok(match latest {
            // Nothing yet: the first attempt.
            None => 1,
            // Resuming after somebody answered, and the attempt they answered
            // about is finished. The next one, so that reply stays as it was.
            Some((_, attempt, true, _)) if resuming => attempt + 1,
            // An attempt that put something on the reader's screen. The next
            // one, whatever brought this turn back.
            //
            // The rule used to be that only a *finished* attempt was worth
            // keeping, on the reasoning that a crash had produced nothing
            // anybody saw. That premise is false the moment a turn streams:
            // one ran for eight minutes, made six tool calls, and had its lease
            // reaped -- and the retry took the row back and overwrote all of it
            // with a one-sentence answer. The calls were on screen while they
            // happened and then were not, which is the message changing under
            // the reader by a path nobody had thought of.
            Some((_, attempt, _, true)) => attempt + 1,
            // Unfinished, and its row empty -- which is what *every* attempt
            // still streaming looks like, because a reply's row is written once,
            // when it finishes. What it said is in its events. Asking the row
            // alone kept nothing a crash interrupted, which was the whole case.
            Some((id, attempt, false, false)) => {
                if self.seal_interrupted(id).await? {
                    attempt + 1
                } else {
                    // Truly nothing: taken back, because an empty row stranded
                    // past a new one is what the abandoned-placeholder guard
                    // trips over.
                    attempt
                }
            }
            Some((_, attempt, true, false)) => attempt,
        })
    }

    async fn current_attempt(&self, replies_to: Uuid) -> Result<i32, ChatError> {
        let latest: Option<i32> =
            sqlx::query_scalar("select max(attempt) from agent_messages where replies_to = $1")
                .bind(replies_to)
                .fetch_one(&self.pool)
                .await
                .map_err(internal)?;
        Ok(latest.unwrap_or(1))
    }

    async fn discard_placeholder(&self, replies_to: Uuid, attempt: i32) -> Result<(), ChatError> {
        // Only while still empty: a turn that failed after writing its reply
        // must not have that reply deleted.
        // Scoped to the attempt being abandoned, and only over a reply with
        // nothing in it. A turn refused at a gate very often has empty
        // `content` and nothing but the refused call in its metadata -- the
        // guest made the call, was refused, and returned at the round boundary
        // without saying anything. Unscoped, abandoning a later attempt deleted
        // that one too, which is exactly the evidence the approver approved
        // against and the reason migration 0019 exists.
        //
        // What counts as "nothing in it" is `said_something`, shared with the
        // abandoned-placeholder guard below. Written out twice before that, and
        // the two had already drifted once: an exclusion added here was missed
        // there, under a comment claiming a parity that did not exist.
        //
        // Asked of the row, "nothing in it" is also true of a reply that
        // streamed for minutes and then died, because the row is written only
        // when a turn finishes. That deleted six tool calls a reader had
        // watched, on the turn's last failed attempt. So an unfinished reply is
        // first sealed from its events, which makes it say something if it did.
        let unfinished: Option<Uuid> = sqlx::query_scalar(
            "select id from agent_messages \
             where replies_to = $1 and attempt = $2 and finished_at is null",
        )
        .bind(replies_to)
        .bind(attempt)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        if let Some(id) = unfinished {
            self.seal_interrupted(id).await?;
        }
        sqlx::query(
            "delete from agent_messages \
             where replies_to = $1 and attempt = $2 \
               and not said_something(content, metadata)",
        )
        .bind(replies_to)
        .bind(attempt)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn record_approval(
        &self,
        session_id: Uuid,
        answer: super::ApprovalAnswer<'_>,
    ) -> Result<Message, ChatError> {
        // Stored as `assistant`, the way a summary is, and for the same
        // practical reason: those are the messages the transcript serves and the
        // client draws. A `system` role reaches neither -- the browser maps only
        // user and assistant to a component, so it would render as nothing at
        // all.
        //
        // What stops the agent reading it as its own speech is the mark plus the
        // framing on the way to the model, exactly as `summarise::framed` does
        // for a summary. The content below is written to be true either way.
        //
        // Deliberately not going through `append_message`, which refuses to
        // write past an abandoned reply. A turn waiting on an approval has an
        // empty reply by construction, and this has to land while that is true.
        let mark = serde_json::json!({
            super::APPROVAL_MARK: {
                "requires": answer.requires,
                "approved": answer.approved,
                "answered_by": answer.answered_by,
                "answered_by_name": answer.answered_by_name,
                "note": answer.note,
            }
        });

        // The content says it in words too. A client that does not know this
        // mark still shows something true, and the model reading the transcript
        // later is told what happened rather than inferring it from a refusal
        // that stopped repeating.
        let who = answer.answered_by_name.unwrap_or("somebody");
        let content = match (answer.approved, answer.note) {
            (true, Some(note)) if !note.trim().is_empty() => {
                format!("{who} approved this {}: {note}", answer.requires)
            }
            (true, _) => format!("{who} approved this {}.", answer.requires),
            // A decline carries an instruction where an approval needs none.
            // Lifting the hold is what lets the conversation carry on -- holding
            // it up suspended every later turn in the session, with the item
            // cancelled and nobody able to answer it again -- so something else
            // has to stop the agent simply calling again and raising the same
            // question. That something is this sentence.
            //
            // It does not forbid the call outright. A person who declines a
            // charge and then changes their mind should be able to say so and
            // have it go through, which means the agent must be free to try
            // again *after discussing it*. The gate is what actually holds the
            // line: a decline mints no grant, so a retry raises a fresh approval
            // rather than slipping past.
            (false, Some(note)) if !note.trim().is_empty() => {
                format!(
                    "{who} declined this {}: {note}\n\n{DECLINED_GUIDANCE}",
                    answer.requires
                )
            }
            (false, _) => format!(
                "{who} declined this {}.\n\n{DECLINED_GUIDANCE}",
                answer.requires
            ),
        };

        let row = sqlx::query(
            "insert into agent_messages (id, session_id, role, content, metadata, user_id) \
             values ($1, $2, 'assistant', $3, $4, $5) \
             returning id, session_id, role, content, metadata, model, \
                       prompt_tokens, completion_tokens, replies_to, absorbed_by",
        )
        .bind(Uuid::now_v7())
        .bind(session_id)
        .bind(&content)
        .bind(&mark)
        .bind(answer.answered_by)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        Ok(read_message(&row))
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
        //
        // `parked` is in it for that reason: a turn waiting on a person has an
        // empty reply and is not running, and it is the one state here that is
        // *expected* to sit that way for hours. Left out, the first message
        // somebody sends while an approval is pending is refused, and the
        // conversation they were trying to unblock is the one that wedges.
        //
        // `j.kind` is named although only turns carry a `message_id`: it is
        // what lets the planner use `jobs_chat_turn_message_idx`, whose
        // predicate it is. Without it this joined every job ever run.
        let abandoned: Option<Uuid> = sqlx::query_scalar(
            "select m.id from agent_messages m \
             left join jobs j \
                    on (j.payload->>'message_id')::uuid = m.replies_to \
                   and j.kind = 'chat.turn' \
                   and j.state in ('pending', 'running', 'succeeded', 'cancelled', 'parked') \
             where m.session_id = $1 and m.role = 'assistant' \
               and not said_something(m.content, m.metadata) \
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
    fn a_message_taken_mid_turn_keeps_its_place() {
        let events = json!([
            { "kind": "chat.tool", "payload": { "call": { "id": "c1", "name": "load_tools" } } },
            { "kind": "chat.steer", "payload": { "id": "u2" } },
            { "kind": "chat.delta", "payload": { "text": "Here is more." } },
        ]);

        assert_eq!(
            replay(json!({}), &events)["parts"],
            json!([
                { "type": "call", "id": "c1" },
                { "type": "steer", "id": "u2" },
                { "type": "text", "text": "Here is more." },
            ])
        );
    }

    #[test]
    fn text_alone_leaves_the_metadata_as_it_was() {
        let events = json!([{ "kind": "chat.delta", "payload": { "text": "hi" } }]);
        assert_eq!(replay(json!({}), &events), json!({}));
    }
}

#[cfg(test)]
mod reasoning_replay {
    use super::replay;
    use serde_json::json;

    fn thinking(at: &str, text: &str) -> serde_json::Value {
        json!({ "kind": "chat.reasoning", "payload": { "text": text }, "at": at })
    }

    /// A reload must report the duration the live path measured, not the time
    /// of the reload. The events carry their own clock, so the arithmetic is
    /// the same either way.
    #[test]
    fn a_reload_reports_what_the_thought_actually_took() {
        let events = json!([
            thinking("2026-09-28T07:00:00Z", "Let me "),
            thinking("2026-09-28T07:00:04Z", "work it out."),
            { "kind": "chat.tool", "payload": { "call": { "id": "c1", "name": "fetch_url" } } },
        ]);

        let parts = replay(json!({}), &events)["parts"].clone();
        assert_eq!(parts[0]["type"], "reasoning");
        assert_eq!(parts[0]["text"], "Let me work it out.");
        assert_eq!(parts[0]["ms"], 4000);
    }

    /// The clock restarts on each thought. A pause spent running a tool is not
    /// time the model spent thinking, and counting it would report a thought
    /// that took four seconds as having taken forty.
    #[test]
    fn time_spent_in_a_tool_is_not_counted_as_thinking() {
        let events = json!([
            thinking("2026-09-28T07:00:00Z", "first"),
            { "kind": "chat.tool", "payload": { "call": { "id": "c1", "name": "fetch_url" } } },
            { "kind": "chat.tool_result", "payload": { "id": "c1", "details": "ok", "is_error": false } },
            thinking("2026-09-28T07:00:30Z", "second "),
            thinking("2026-09-28T07:00:32Z", "thought"),
        ]);

        let parts = replay(json!({}), &events)["parts"].clone();
        let thoughts: Vec<&serde_json::Value> = parts
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["type"] == "reasoning")
            .collect();

        assert_eq!(
            thoughts.len(),
            2,
            "two thoughts, not one run across the call"
        );
        assert!(
            thoughts[0].get("ms").is_none(),
            "one fragment has no span, so nothing timed it -- and zero would \
             render as a thought that took no time"
        );
        assert_eq!(
            thoughts[1]["ms"], 2000,
            "two seconds, not the thirty since the first"
        );
    }

    /// An event written before the time was carried still replays its words.
    /// The contract the UI's untimed branch depends on: no `ms` rather than a
    /// zero. Asserted, because a test that only checked the text passed with the
    /// whole duration feature deleted.
    #[test]
    fn a_thought_with_no_clock_still_comes_back() {
        let events = json!([
            { "kind": "chat.reasoning", "payload": { "text": "no clock here" } },
            { "kind": "chat.tool", "payload": { "call": { "id": "c1", "name": "fetch_url" } } },
        ]);
        let parts = replay(json!({}), &events)["parts"].clone();
        assert_eq!(parts[0]["text"], "no clock here");
        assert!(
            parts[0].get("ms").is_none(),
            "nothing timed it, so it says nothing"
        );
    }
}
