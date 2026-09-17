use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use uuid::Uuid;


use crate::events;
use crate::jobs;
use crate::runtime::router::ExecuteEvent;

use super::agent::AgentStore;
use super::chat::{ChatStore, Usage};





/// Job kind for "the user said something; produce a reply".
pub const CHAT_TURN: &str = "chat.turn";

/// Where a call's token counts came from, judged by what the provider sent.
///
/// A provider that returned a usage object is the authority on what its own
/// call cost. One that returned nothing leaves zeros behind, and those zeros
/// must not be recorded as though somebody had measured them: a turn that cost
/// nothing and a turn nobody counted look identical afterwards, and only this
/// tells them apart. Nothing can recover it later, so it is decided here, once,
/// on the only evidence there is.
fn source_of(provider_usage: &Option<serde_json::Value>) -> super::usage::UsageSource {
    match provider_usage {
        Some(usage) if !usage.is_null() => super::usage::UsageSource::Reported,
        _ => super::usage::UsageSource::Unknown,
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatTurnPayload {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
    /// The user message this turn answers. The reply hangs off it, so a
    /// retried turn fills the reply it already created instead of orphaning
    /// it, and two turns running at once in one session cannot collide.
    pub message_id: Uuid,
    /// IANA zone the sender was in, e.g. "Europe/London". Carried on the turn
    /// rather than the account: it is where the user is now, and a laptop that
    /// crosses a border should not keep answering in the zone it left.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Who sent the prompt, for the usage ledger. Absent on turns nobody
    /// sent -- a schedule, a webhook.
    #[serde(default)]
    pub user_id: Option<Uuid>,
}

/// Rebuilds the conversation a model should be shown from what was stored.
///
/// A turn's tool round trips are part of the conversation. Shown only the
/// prose it wrote afterwards, an agent cannot tell what it looked up from what
/// it decided -- so it looks things up again, and answers questions about its
/// own earlier answers by guessing.
///
/// What is replayed is what the model was given at the time, truncation and
/// all. Cutting a result down happens once, when the tool runs; a model that
/// wants more asks for more through the ranged read, rather than being handed
/// a larger version of an answer it already has. `details` -- the whole
/// result, for the reader -- is never sent.
///
/// One stored assistant message becomes up to three, because that is the order
/// things happened in: the calls, then their results, then the prose. Rounds
/// are not recovered; a turn that called tools three times replays as one
/// batch. That is a faithful account of what was asked and answered, and a
/// lossy one of when.
fn project(messages: &[super::chat::Message]) -> Vec<serde_json::Value> {
    // A stored summary stands in for everything it covers. The last one wins:
    // a later summary's range includes any earlier one, because each is
    // written from the projection the one before it produced.
    //
    // Dropped from the projection rather than from the session -- the messages
    // are still there to read, and a summary that turns out to have lost
    // something is a bad turn rather than a bad archive.
    let covered = messages
        .iter()
        .filter_map(|m| {
            m.metadata
                .get(super::chat::summarise::SUMMARY_MARK)
                .and_then(|v| v.as_str())
                .and_then(|id| id.parse::<Uuid>().ok())
                .map(|through| (m.id, through))
        })
        .next_back();

    let messages: Vec<&super::chat::Message> = match covered {
        Some((summary_id, through)) => messages
            .iter()
            .filter(|m| m.id == summary_id || !(m.id <= through))
            .collect(),
        None => messages.iter().collect(),
    };

    let mut projected = Vec::with_capacity(messages.len());

    for message in messages {
        let calls: Vec<&serde_json::Value> = message
            .metadata
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .map(|c| c.iter().collect())
            .unwrap_or_default();

        // The order the reply was produced in, where it was recorded. A
        // message written before this was kept flattens to its text and then
        // its calls, which is what it used to be replayed as.
        let recorded = message.metadata.get("parts").and_then(|p| p.as_array());
        let parts: Vec<serde_json::Value> = match recorded {
            Some(parts) if !parts.is_empty() => parts.clone(),
            _ => {
                // Written before the order was kept. Those turns were
                // replayed as the calls and then the text, which is what they
                // were: the model called, was answered, and then spoke.
                let mut fallback = Vec::new();
                for call in &calls {
                    fallback.push(serde_json::json!({"type": "call", "id": call["id"]}));
                }
                if !message.content.is_empty() {
                    fallback.push(serde_json::json!({"type": "text", "text": message.content}));
                }
                fallback
            }
        };

        if calls.is_empty() {
            projected.push(serde_json::json!({
                "role": message.role,
                "parts": [{"type": "text", "text": message.content}],
            }));
            continue;
        }

        // A turn goes back as it happened, and a result has to reach the model
        // between the call that asked and the words written after it. So text
        // that follows a call closes the message: what came before is what the
        // model had said when it called, and what comes after is what it said
        // once answered. A reply that called and then spoke -- the ordinary
        // shape -- splits exactly where it always did.
        let mut open: Vec<serde_json::Value> = Vec::new();
        let mut awaiting: Vec<&serde_json::Value> = Vec::new();

        let flush = |open: &mut Vec<serde_json::Value>,
                         awaiting: &mut Vec<&serde_json::Value>,
                         out: &mut Vec<serde_json::Value>| {
            if !open.is_empty() {
                out.push(serde_json::json!({
                    "role": "assistant",
                    "parts": std::mem::take(open),
                }));
            }
            for call in awaiting.drain(..) {
                out.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call["id"],
                    // Every call must be answered. A turn that died between
                    // asking and recording leaves one without a result, and a
                    // request carrying an unanswered call is rejected outright
                    // -- so the gap is filled rather than left to break the
                    // next turn as well.
                    "parts": [{
                        "type": "text",
                        "text": call["result"]
                            .as_str()
                            .unwrap_or("{\"error\":\"no result was recorded\"}"),
                    }],
                }));
            }
        };

        for part in &parts {
            match part["type"].as_str() {
                Some("text") => {
                    let text = part["text"].as_str().unwrap_or("");
                    if text.is_empty() {
                        continue;
                    }
                    if !awaiting.is_empty() {
                        flush(&mut open, &mut awaiting, &mut projected);
                    }
                    open.push(serde_json::json!({"type": "text", "text": text}));
                }
                Some("call") => {
                    let Some(call) = calls.iter().find(|c| c["id"] == part["id"]) else {
                        continue;
                    };
                    open.push(serde_json::json!({
                        "type": "call",
                        "call": {
                            "id": call["id"],
                            "name": call["name"],
                            "arguments": call["arguments"].as_str().unwrap_or("{}"),
                        },
                    }));
                    awaiting.push(call);
                }
                _ => {}
            }
        }
        flush(&mut open, &mut awaiting, &mut projected);
    }

    projected
}

/// The transcript as it stood when `prompt` was sent: nothing after it.
///
/// A message the user sent after this prompt is already stored by the time
/// the turn is prepared, and left in it would reach the model twice -- once
/// here as history, and again when the gateway hands it over as a steer. The
/// model then answers the later message in the earlier one's reply and is
/// told about it a second time. Later messages reach this turn only as
/// steers, once, or wait for their own turn.
fn up_to(messages: Vec<super::chat::Message>, prompt: Uuid) -> Vec<super::chat::Message> {
    // Ids are UUIDv7, so id order is send order.
    messages.into_iter().filter(|m| m.id <= prompt).collect()
}

/// Says why the conversation stops where it does, for a turn picking it up.
///
/// A session that was stopped before its turn ran has no partial reply -- the
/// transcript is a prompt and then silence. So the marker says a message went
/// unanswered and why, rather than that a reply was cut off: telling a model its
/// reply was stopped when it never wrote one invites it to apologise for a
/// fragment that does not exist.
///
/// Stopping mid-flight is the other case and wants different words. It does not
/// happen yet -- nothing stops a turn once it is running -- and when it does,
/// this is where that marker goes.
///
/// Placed before the prompts rather than after, because it is context for what
/// follows: everything below it is what was asked while nothing could answer.
fn marked(projected: Vec<serde_json::Value>, restarting_from: Option<&str>) -> Vec<serde_json::Value> {
    let Some(reason) = restarting_from else {
        return projected;
    };

    // Ahead of the last message, which is the prompt that restarted the
    // session. What sits between the marker and the end is the question that
    // went unanswered and the one asking again -- and the guest's own framing
    // asks the model to answer each in order.
    let split = projected.len().saturating_sub(1);
    let mut out = Vec::with_capacity(projected.len() + 1);
    out.extend(projected.iter().take(split).cloned());
    out.push(serde_json::json!({
        "role": "user",
        "parts": [{
            "type": "text",
            "text": format!(
                "[the messages below went unanswered: this conversation was \
                 stopped -- {reason}. It has been restarted. Answer what was \
                 asked, and do not apologise for the pause.]"
            ),
        }],
    }));
    out.extend(projected.into_iter().skip(split));
    out
}

/// What a completed turn produced.
pub(super) struct TurnOutcome {
    content: String,
    /// Tool calls the agent made, in order, each with the model's own label.
    tools: Vec<serde_json::Value>,
    /// The reply in the order it arrived: prose and the calls that sat between
    /// it.
    ///
    /// Text is held here; a call is named by id and its detail read from
    /// `tools`, so nothing about a call is written down twice. Assembled from
    /// the event stream, which is already in order -- the arrangement was
    /// never unknown, only discarded.
    parts: Vec<serde_json::Value>,
    /// Summed across every round of the turn, counted by the runtime host.
    usage: Usage,
    /// The endpoint that served it, for attributing spend.
    provider: Option<String>,
}

pub struct Worker {
    pub pool: PgPool,
    pub agents: Arc<dyn AgentStore>,
    /// The prose an agent is given beside its own prompt.
    pub skills: Arc<dyn super::skill::SkillStore>,
    pub chat: Arc<dyn ChatStore>,
    /// Where every model call is written down, as it is reported.
    pub usage: Arc<dyn super::usage::UsageStore>,
    /// The cascade a turn's knobs come from.
    pub settings: Arc<dyn super::settings::SettingsStore>,
    /// Holds on work: kill switches, and later the approvals a turn waits on.
    pub inhibitors: Arc<dyn super::inhibitor::InhibitorStore>,
    /// Signs the token a summary's model call carries. Optional because the
    /// worker runs without one: a deployment with no gateway configured still
    /// prepares turns, it just cannot summarise.
    pub minter: Option<Arc<crate::auth::TokenMinter>>,
    /// Where to ask for a summary. Absent means no summarising, and the trim
    /// underneath carries on alone.
    pub gateway_url: Option<String>,
}


impl Worker {



    /// Gives up on a turn: clears the reply nothing will fill, and says so.
    ///
    /// An empty reply left behind wedges the session against further messages,
    /// and a reader with no error event waits on an indicator that resolves on
    /// no timescale at all. Both halves matter, which is why they are one
    /// function rather than two blocks that drifted apart.
    async fn abandon_payload(&self, payload: &ChatTurnPayload, reason: &str) {
        if let Err(e) = self.chat.discard_placeholder(payload.message_id).await {
            tracing::error!(
                session_id = %payload.session_id,
                error = %e,
                "failed to discard placeholder"
            );
        }

        let _ = events::append(
            &self.pool,
            payload.workspace_id,
            Some(payload.session_id),
            "chat.error",
            serde_json::json!({ "message": reason, "message_id": payload.message_id }),
        )
        .await;
    }


    /// Reads a turn's progress and records it as it arrives.
    ///
    /// Takes a stream of bytes rather than a response, because the same events
    /// arrive by two routes: as the body of a reply from a runtime this tier
    /// called, and as the body of a request from a runtime that came asking.
    /// What has to happen to them is identical either way -- the transcript is
    /// written here, where the database is, and a runtime never touches it.
    pub(super) async fn consume_turn(
        &self,
        stream: impl futures::Stream<Item = Result<axum::body::Bytes, impl std::fmt::Display>> + Unpin,
        payload: &ChatTurnPayload,
        message_id: Uuid,
        job_id: Uuid,
    ) -> anyhow::Result<TurnOutcome> {
        use futures::StreamExt;

        let mut stream = stream;
        let mut buffer = String::new();
        let mut tools: Vec<serde_json::Value> = Vec::new();
        // Built as the events arrive, which is the order they happened in.
        let mut parts: Vec<serde_json::Value> = Vec::new();
        // The session's account label, for the ledger. Read once, on the
        // first call that needs it, so a turn that makes no model call reads
        // nothing.
        let mut account: Option<Option<String>> = None;

        while let Some(bytes) = stream.next().await {
            let bytes = bytes.map_err(|e| anyhow::anyhow!("{e}"))?;
            buffer.push_str(std::str::from_utf8(&bytes)?);

            // An event may span reads, so only whole lines are parsed.
            while let Some(index) = buffer.find('\n') {
                let line = buffer[..index].trim().to_string();
                buffer.drain(..=index);
                if line.is_empty() {
                    continue;
                }

                match serde_json::from_str::<ExecuteEvent>(&line) {
                    Ok(ExecuteEvent::Delta { idx, text }) => {
                        match parts.last_mut() {
                            Some(p) if p["type"] == "text" => {
                                let joined =
                                    format!("{}{}", p["text"].as_str().unwrap_or(""), text);
                                p["text"] = serde_json::json!(joined);
                            }
                            _ => parts.push(serde_json::json!({"type": "text", "text": text})),
                        }
                        events::append(
                            &self.pool,
                            payload.workspace_id,
                            Some(payload.session_id),
                            "chat.delta",
                            serde_json::json!({
                                "message_id": message_id,
                                "idx": idx,
                                "text": text,
                            }),
                        )
                        .await?;
                    }
                    Ok(ExecuteEvent::Tool {
                        id,
                        name,
                        action,
                        arguments,
                    }) => {
                        let call = serde_json::json!({
                            "id": id,
                            "name": name,
                            "action": action,
                            // Stored, never announced: the browser shows the
                            // action, and a later turn needs the call.
                            "arguments": arguments,
                        });
                        // Announced live so the browser can show the work as
                        // it happens, and kept so the finished message can
                        // record it -- the event feed is prunable, the
                        // transcript is not.
                        tools.push(call.clone());
                        parts.push(serde_json::json!({"type": "call", "id": id}));
                        events::append(
                            &self.pool,
                            payload.workspace_id,
                            Some(payload.session_id),
                            "chat.tool",
                            serde_json::json!({
                                "message_id": message_id,
                                "call": call,
                            }),
                        )
                        .await?;
                    }
                    Ok(ExecuteEvent::ToolResult {
                        id,
                        details,
                        content,
                        is_error,
                    }) => {
                        // Attached to the call it answers rather than sent as
                        // its own thing, so the browser has one object per
                        // tool: what it was doing, and what came back.
                        if let Some(call) = tools
                            .iter_mut()
                            .find(|c| c["id"].as_str() == Some(id.as_str()))
                        {
                            call["details"] = serde_json::json!(details);
                            // What the model was handed, kept exactly as it
                            // was handed over. Replaying anything else would
                            // rewrite the conversation the model remembers.
                            call["result"] = serde_json::json!(content);
                            call["is_error"] = serde_json::json!(is_error);
                        }
                        events::append(
                            &self.pool,
                            payload.workspace_id,
                            Some(payload.session_id),
                            "chat.tool_result",
                            serde_json::json!({
                                "message_id": message_id,
                                "id": id,
                                "details": details,
                                "is_error": is_error,
                            }),
                        )
                        .await?;
                    }
                    Ok(ExecuteEvent::Wrote { path, key }) => {
                        // Same call the upload handler makes. A document is a
                        // document whether a person dragged it in or an agent
                        // unpacked it from an archive, and only this tier can
                        // queue the job that reads it. The key is the
                        // runtime's word for where it wrote; a runtime that
                        // lied would be asking this tier to read outside the
                        // workspace, so the word is checked.
                        if !super::extract::belongs_to(&key, payload.workspace_id) {
                            tracing::warn!(key = %key, workspace_id = %payload.workspace_id,
                                "runtime reported a write outside the workspace; ignoring");
                        } else {
                            super::extract::enqueue(&self.pool, payload.workspace_id, &key, &path)
                                .await;
                        }
                    }
                    Ok(ExecuteEvent::Usage {
                        round,
                        endpoint,
                        model,
                        paid_by,
                        prompt_tokens,
                        completion_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                        reasoning_tokens,
                        provider_usage,
                        service_tier,
                    }) => {
                        // Written the moment the call is known to have cost
                        // something, not when the turn ends: a turn that fails
                        // after three calls still bills for three.
                        let account = match &account {
                            Some(a) => a.clone(),
                            None => {
                                let found = self
                                    .chat
                                    .get_session(payload.workspace_id, payload.session_id)
                                    .await
                                    .ok()
                                    .and_then(|s| s.account);
                                account = Some(found.clone());
                                found
                            }
                        };
                        if let Err(e) = self
                            .usage
                            .record(super::usage::RecordUsage {
                                workspace_id: payload.workspace_id,
                                agent_id: Some(payload.agent_id),
                                session_id: Some(payload.session_id),
                                user_id: payload.user_id,
                                account,
                                reply_id: Some(message_id),
                                job_id: Some(job_id),
                                round: round as i32,
                                traffic_type: traffic_type_for(
                                    &self
                                        .agents
                                        .get(payload.workspace_id, payload.agent_id)
                                        .await
                                        .map(|a| a.policy)
                                        .unwrap_or(serde_json::Value::Null),
                                ),
                                endpoint,
                                model,
                                credential_owner: paid_by,
                                fallback: "none".to_string(),
                                prompt_tokens: prompt_tokens as i32,
                                completion_tokens: completion_tokens as i32,
                                cache_read_tokens: cache_read_tokens as i32,
                                cache_write_tokens: cache_write_tokens as i32,
                                reasoning_tokens: reasoning_tokens as i32,
                                usage_source: source_of(&provider_usage),
                                provider_usage,
                                service_tier,
                            })
                            .await
                        {
                            // A ledger write that fails is an operator's
                            // problem, and loud: the bill is wrong.
                            tracing::error!(
                                job_id = %job_id,
                                workspace_id = %payload.workspace_id,
                                error = %e,
                                "could not record usage"
                            );
                        }
                    }
                    Ok(ExecuteEvent::Done {
                        content,
                        prompt_tokens,
                        completion_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                        reasoning_tokens,
                        provider,
                    }) => {
                        return Ok(TurnOutcome {
                            content,
                            tools,
                            parts,
                            usage: Usage {
                                // Recorded as signed, since a provider that
                                // reports nothing should read as absent rather
                                // than as zero spend.
                                prompt_tokens: Some(prompt_tokens as i32),
                                completion_tokens: Some(completion_tokens as i32),
                                cache_read_tokens: Some(cache_read_tokens as i32),
                                cache_write_tokens: Some(cache_write_tokens as i32),
                                reasoning_tokens: Some(reasoning_tokens as i32),
                            },
                            provider,
                        });
                    }
                    Ok(ExecuteEvent::Failed { message }) => {
                        anyhow::bail!("guest failed: {message}")
                    }
                    Err(e) => tracing::warn!(error = %e, "malformed event from runtime"),
                }
            }
        }

        anyhow::bail!("runtime stream ended without a result")
    }

    /// Returns work whose runtime stopped reporting.
    ///
    /// A turn is claimed by the tier handing it out and reported by the
    /// runtime running it, and nothing connects those two but a lease. When a
    /// runtime is killed mid-turn nobody fails the job -- the pod that would
    /// have is gone -- so without this the row stays `running` for ever, and
    /// because a serial key admits no second job while one is running, every
    /// later turn in that session is blocked behind it.
    pub fn spawn_reaper(self: Arc<Self>, shutdown: Arc<tokio::sync::Notify>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(jobs::LEASE_HEARTBEAT);
            loop {
                tokio::select! {
                    _ = shutdown.notified() => return,
                    _ = ticker.tick() => {
                        match jobs::reap_abandoned(&self.pool).await {
                            Ok((0, _)) => {}
                            Ok((n, gave_up)) => {
                                tracing::info!(jobs = n, "returned abandoned work to the queue");
                                // A job the reaper parks as failed has no
                                // worker left to clean up after it. Left
                                // alone, its empty reply blocks the session
                                // and the reader waits for ever.
                                for job in gave_up {
                                    if job.kind != CHAT_TURN {
                                        continue;
                                    }
                                    match serde_json::from_value::<ChatTurnPayload>(job.payload) {
                                        Ok(payload) => {
                                            self.abandon_payload(
                                                &payload,
                                                "the turn was lost too many times and was given up on",
                                            )
                                            .await
                                        }
                                        Err(e) => tracing::error!(job_id = %job.id, error = %e, "payload"),
                                    }
                                }
                            }
                            Err(e) => tracing::error!(error = %e, "could not reap abandoned work"),
                        }

                        // Keeps the live-session table the size of the
                        // conversations happening now. Nothing depends on it
                        // being prompt -- the count filters on expiry anyway --
                        // so a missed sweep costs a slightly larger scan.
                        let _ = sqlx::query("delete from live_sessions where expires_at < now()")
                            .execute(&self.pool)
                            .await;

                        // Deltas exist to assemble a reply that is still
                        // streaming and to let a browser catch up on one. Once
                        // the reply is stored they are copies of text held
                        // elsewhere, and the events table is the one that
                        // grows with every token ever generated. Kept a day
                        // so a poll cursor from a long-idle tab still finds
                        // them, then gone.
                        let _ = sqlx::query(
                            "delete from events e \
                             where e.kind = 'chat.delta' \
                               and e.created_at < now() - interval '1 day' \
                               and exists ( \
                                   select 1 from agent_messages m \
                                   where m.id = (e.payload->>'message_id')::uuid \
                                     and m.content <> '')",
                        )
                        .execute(&self.pool)
                        .await;
                    },
                }
            }
        });
    }

    /// Whether anything is holding this turn, and what to do about it.
    ///
    /// `Ok(None)` means nothing is: carry on. `Ok(Some(_))` is the whole answer
    /// for this turn, already recorded, and the caller returns it.
    ///
    /// The latch is read first and cleared here rather than at the API edge,
    /// because what clears it is a prompt carrying a real `user_id` -- and this
    /// is the tier that has the prompt in hand.
    /// `Ok(Err(_))` is the whole answer for this turn, already recorded.
    /// `Ok(Ok(reason))` means carry on, and `reason` is what this turn is
    /// restarting from -- `None` when nothing was holding it.
    #[allow(clippy::type_complexity)]
    async fn inhibited(
        &self,
        payload: &ChatTurnPayload,
    ) -> anyhow::Result<
        Result<Option<String>, anyhow::Result<Option<crate::runtime::router::ExecuteRequest>>>,
    > {
        use super::inhibitor::Verdict;

        // What this turn is picking up from, where it is picking up at all.
        let mut restarting_from: Option<String> = None;

        // A stopped session stays stopped until a person says something. Not
        // until the hold is released: releasing a kill switch must not resume
        // fifty conversations that were killed while it was on.
        if self
            .chat
            .stopped_reason(payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("stopped: {e}"))?
            .is_some()
        {
            // `user_id` is null for anything the platform produced, so the
            // agent cannot clear its own latch and neither can a steer it
            // provoked.
            let by_a_person = payload.user_id.is_some();
            if !by_a_person {
                tracing::info!(
                    session_id = %payload.session_id,
                    "a stopped session declined work that no person asked for"
                );
                return Ok(Err(Ok(None)));
            }
            restarting_from = self
                .chat
                .clear_stop(payload.session_id)
                .await
                .map_err(|e| anyhow::anyhow!("clear stop: {e}"))?;
            tracing::info!(session_id = %payload.session_id, "a person restarted a stopped session");
        }

        let holds = self
            .inhibitors
            .covering(payload.workspace_id, payload.agent_id, payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("inhibitors: {e}"))?;
        let decision = super::inhibitor::decide(holds);

        match decision.verdict {
            Verdict::Proceed => Ok(Ok(restarting_from)),
            Verdict::Stopped => {
                // Said in the transcript as well as latched: the next turn
                // reads this history, and a reply that simply stops is one the
                // model apologises for or tries to finish.
                let why = decision
                    .deciding()
                    .map(|i| i.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                self.chat
                    .stop_session(payload.session_id, &why)
                    .await
                    .map_err(|e| anyhow::anyhow!("stop session: {e}"))?;
                tracing::info!(
                    session_id = %payload.session_id,
                    workspace_id = %payload.workspace_id,
                    reason = %why,
                    "a turn was stopped before it ran"
                );
                Ok(Err(Ok(None)))
            }
            // Nothing takes a suspended hold yet -- that arrives with
            // human-in-the-loop. Until then it is treated as a stop without the
            // latch: the turn does not run, and the next one re-evaluates.
            Verdict::Suspended => {
                tracing::info!(
                    session_id = %payload.session_id,
                    "a turn was suspended before it ran"
                );
                Ok(Err(Ok(None)))
            }
        }
    }

    /// Everything a turn needs before it can run.
    ///
    /// All of it touches the database -- the agent, the transcript, the egress
    /// rules, the reply the turn will stream into -- so it happens on this
    /// tier whichever way the turn is going to reach a runtime. Returns None
    /// when the turn has nothing left to do, which is not a failure: a steered
    /// message was answered inside the turn it interrupted, and answering it
    /// again would produce a second reply to a question already addressed.
    pub(super) async fn prepare_turn(
        &self,
        payload: &ChatTurnPayload,
    ) -> anyhow::Result<Option<crate::runtime::router::ExecuteRequest>> {
        let agent = self
            .agents
            .get(payload.workspace_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("agent: {e}"))?;

        if !agent.enabled {
            anyhow::bail!("agent is disabled");
        }

        if self
            .chat
            .was_absorbed(payload.message_id)
            .await
            .map_err(|e| anyhow::anyhow!("absorbed: {e}"))?
        {
            return Ok(None);
        }

        // Before anything is read or written for this turn. A hold that
        // arrives while a turn is being prepared is one the next turn catches;
        // a hold checked after the work is done has already paid for it.
        let restarting_from = match self.inhibited(payload).await? {
            Ok(reason) => reason,
            Err(refusal) => return refusal,
        };

        let history = self
            .chat
            .messages(payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("history: {e}"))?;
        let history = up_to(history.messages, payload.message_id);

        let egress = super::egress::rules_for(&self.pool, payload.workspace_id)
            .await
            .map_err(|e| anyhow::anyhow!("egress rules: {e}"))?;
        // Committed here, next to the rules it is over, so the token minted
        // for this turn can carry a hash rather than the runtime's word for
        // what it was given. The runtime executes workspace code and cannot be
        // the tier that reports its own rules.
        let egress_commitment = crate::egress::commit::root(payload.workspace_id, &egress);

        let placeholder = self
            .chat
            .claim_placeholder(payload.message_id, payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("placeholder: {e}"))?;

        if placeholder.created {
            events::append(
                &self.pool,
                payload.workspace_id,
                Some(payload.session_id),
                "chat.message",
                serde_json::to_value(&placeholder.message)?,
            )
            .await?;
        } else {
            // The reply already exists, so this is a retry: the pod running
            // it was lost and the turn is starting over.
            //
            // Anything the lost attempt had absorbed goes back to pending.
            // The gateway marked those messages as taken when it handed them
            // over, and the attempt that took them died with them unanswered;
            // left marked, they would never be handed over again and their
            // own turns would complete as "already answered" -- the message
            // simply vanishes. Unmarked, the retry is offered them as steers
            // exactly as the first attempt was.
            sqlx::query(
                "update agent_messages set absorbed_by = null \
                 where absorbed_by = $1 and session_id = $2",
            )
            .bind(placeholder.message.id)
            .bind(payload.session_id)
            .execute(&self.pool)
            .await?;

            // Said, so a reader watching an empty reply is told why it went
            // quiet rather than left with an indicator that means nothing.
            events::append(
                &self.pool,
                payload.workspace_id,
                Some(payload.session_id),
                "chat.retry",
                serde_json::json!({
                    "message_id": placeholder.message.id,
                    "replies_to": payload.message_id,
                }),
            )
            .await?;
        }

        // Resolved here, above the runtime, so the guest is handed values
        // and never learns whether the operator, the workspace or the agent
        // chose them.
        let settings = self
            .settings
            .resolve(payload.workspace_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("settings: {e}"))?;

        // Composed here rather than in the runtime, for the same reason the
        // settings are: the guest is handed prose and never learns which of it
        // the operator wrote, which the workspace added, or that either could
        // have been otherwise.
        let skills = self
            .skills
            .resolve_for_agent(payload.workspace_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("skills: {e}"))?;

        // Written down before the turn runs, against the reply it will fill in.
        // What was composed is a fact about this turn whether or not it goes on
        // to succeed, and a failed turn is exactly the one an eval wants to be
        // able to look at afterwards.
        self.skills
            .record_turn(placeholder.message.id, &skills)
            .await
            .map_err(|e| anyhow::anyhow!("recording skills: {e}"))?;

        // Resolved once: the prompt has to name the same model the request asks
        // for, or the agent is told one thing and served by another.
        let model = model_for(&agent.policy);

        // Composed before the conversation is built, because a summary is
        // written against it: what the agent was told to do is what decides
        // which parts of a conversation mattered.
        let system_prompt =
            super::skill::compose_for_turn(&agent.system_prompt, &skills, &model);

        Ok(Some(crate::runtime::router::ExecuteRequest {
            session_id: payload.session_id,
            workspace_id: payload.workspace_id,
            agent_id: payload.agent_id,
            write_scopes: settings.write_scopes,
            read_scopes: settings.read_scopes,
            conversation: {
                let projected = marked(project(&history), restarting_from.as_deref());

                // Over budget is where compaction begins. A summary is tried
                // first because it loses less: the early turns become a
                // paragraph rather than disappearing. It is a model call the
                // user did not ask for, so it happens only when the
                // alternative is losing the messages outright.
                let projected = self
                    .summarised(
                        &projected,
                        &history,
                        &system_prompt,
                        settings.context_budget,
                        payload.session_id,
                        payload.workspace_id,
                        &model,
                    )
                    .await
                    .unwrap_or(projected);

                // Then the floor underneath it. Whatever a summary did not
                // save, this drops -- and when there is no model to ask, or
                // the summary itself would not fit, this is the whole of what
                // happens.
                let (projected, trimmed) = super::chat::trim::to_fit(
                    projected,
                    settings.context_budget,
                );
                if !trimmed.is_empty() {
                    // Said out loud: a turn that quietly lost half its history
                    // is one nobody can explain afterwards, and these numbers
                    // are what say whether the budget is set anywhere near
                    // right.
                    tracing::info!(
                        session_id = %payload.session_id,
                        workspace_id = %payload.workspace_id,
                        results_dropped = trimmed.results_dropped,
                        messages_dropped = trimmed.messages_dropped,
                        was = trimmed.was,
                        now = trimmed.now,
                        budget = settings.context_budget,
                        "conversation trimmed to fit"
                    );
                }
                projected
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<_, _>>()?
            },
            // Composed with the model that will serve this turn, so an agent
            // asked what it is has something true to read rather than a gap to
            // fill.
            system_prompt,
            model: Some(model.clone()),
            timezone: payload.timezone.clone(),
            reasoning_effort: settings.reasoning_effort,
            temperature: settings.temperature,
            traffic_type: Some(traffic_type_for(&agent.policy)),
            max_tool_rounds: Some(i64::from(settings.max_tool_rounds)),
            reply_id: placeholder.message.id,
            egress,
            egress_commitment,
        }))
    }

    /// Replaces the early part of a conversation with a summary of it, when it
    /// is over budget and there is a model to ask.
    ///
    /// Returns `None` whenever it did not happen, for any reason -- no
    /// gateway, nothing worth summarising, the model refused, the summary came
    /// back empty. Every one of those means the trim underneath does the work
    /// alone, which is exactly what it is for. A failure here must never cost
    /// a turn: the user asked for an answer, not for a summary.
    async fn summarised(
        &self,
        conversation: &[serde_json::Value],
        // What the conversation was projected from, so the summary can be
        // stored against the last message it covers. The projection has no
        // ids in it -- it is what goes to the model -- and a summary that
        // cannot say what it stands in for cannot be carried.
        history: &[super::chat::Message],
        system_prompt: &str,
        budget: usize,
        session_id: uuid::Uuid,
        workspace_id: uuid::Uuid,
        // The model this turn is routed to. The summary goes to the same one:
        // asking a different model would bill a route the turn is not using,
        // and a deployment whose default is unset or unroutable would fail
        // every compaction while the turn itself is fine.
        model: &str,
    ) -> Option<Vec<serde_json::Value>> {
        use super::chat::summarise;
        use crate::gateway::llm::types::{
            ChatCompletionRequest, ChatCompletionResponse, ContentPart, MessageContent,
        };

        if super::chat::trim::total_cost(conversation) <= budget {
            return None;
        }
        let (Some(minter), Some(gateway_url)) = (&self.minter, &self.gateway_url) else {
            return None;
        };
        let through = summarise::boundary(conversation)?;

        let request = summarise::request(system_prompt, &conversation[..through]);
        // Bounded by construction: the system prompt, what is being replaced,
        // and the instruction. Never the whole transcript, which is the thing
        // that does not fit.
        let body = ChatCompletionRequest {
            model: model.to_string(),
            messages: request,
            temperature: None,
            // Long enough to carry the constraints forward, short enough that
            // a summary cannot itself become the thing that does not fit.
            max_tokens: Some(1024),
            tools: None,
            // Summarising is reading, not deciding.
            reasoning_effort: Some("none".into()),
            stream: false,
            stream_options: None,
        };

        // Signed for the session it is summarising, so the spend lands on the
        // workspace that caused it: every model call is a row in the usage
        // ledger, and a summary nobody is billed for is a summary nobody can
        // account for. No egress commitment, because summarising reaches
        // nothing but the model.
        let token = minter
            .mint_turn(session_id, workspace_id, crate::egress::commit::empty_root())
            .ok()?;

        let response = reqwest::Client::new()
            .post(format!("{gateway_url}/v1/chat/completions"))
            .bearer_auth(token)
            .header(crate::gateway::TRAFFIC_HEADER, summarise::TRAFFIC_TYPE)
            .json(&body)
            .send()
            .await
            .ok()?;

        if !response.status().is_success() {
            tracing::warn!(
                status = %response.status(),
                "could not summarise; the trim will carry the conversation"
            );
            return None;
        }

        // Read before the body is consumed. These are what let the ledger say
        // where the call went and whose credential paid, which it cannot get
        // from the completion itself.
        let endpoint = response
            .headers()
            .get("x-outturn-provider")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let paid_by = response
            .headers()
            .get("x-outturn-paid-by")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("operator")
            .to_string();

        let completion: ChatCompletionResponse = match response.json().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "could not read the summary; the trim will carry the conversation");
                return None;
            }
        };
        // Both shapes, because a provider may answer with either and a summary
        // silently skipped is one that was paid for and thrown away.
        let summary = completion.choices.first().map(|c| match &c.message.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        });
        let Some(summary) = summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            tracing::warn!("the summary came back empty; the trim will carry the conversation");
            return None;
        };
        // A model asked for 1024 tokens can still answer with more. Past the
        // bound the summary is refused rather than truncated: half a summary
        // ending mid-sentence would be carried forward for the rest of the
        // session, and the trim underneath loses less than that.
        if summary.len() > summarise::SUMMARY_MAX_BYTES {
            tracing::warn!(
                summary_bytes = summary.len(),
                limit = summarise::SUMMARY_MAX_BYTES,
                "the summary came back longer than a summary may be; the trim will carry the conversation"
            );
            return None;
        }

        let replaced = summarise::apply(conversation.to_vec(), summary, through);
        // A summary larger than what it replaced is one that helped nobody,
        // and sending it would be worse than the trim alone.
        if super::chat::trim::total_cost(&replaced) >= super::chat::trim::total_cost(conversation) {
            tracing::warn!("the summary was no smaller than the conversation; keeping the original");
            return None;
        }

        // A carried summary that has grown past the bound is folded rather
        // than carried again: it was at the front of what was just summarised,
        // so the new summary already stands for it and everything after it.
        // Under the bound the old one is left where it is and the new one
        // supersedes it by covering more -- the projection takes the last mark
        // it finds.
        //
        // Stored, so the next turn reads the summary instead of paying to
        // write it again. Best effort: a summary that could not be saved has
        // still done its job for this turn, and a failed write must not cost
        // the turn the user actually asked for.
        //
        // Covered is everything before the tail that survived. Taken from the
        // stored messages rather than from `through`, which indexes the
        // projection -- and the projection may already have a summary standing
        // where several messages were.
        if let Some(through_id) = history
            .len()
            .checked_sub(summarise::TAIL_MESSAGES)
            .and_then(|end| history.get(end.saturating_sub(1)))
            .map(|m| m.id)
        {
            self.store_summary(session_id, summary, through_id, model).await;
        }

        // A summary is a model call the user did not ask for and is billed
        // for regardless, so it belongs in the ledger like any other. Written
        // after the summary is known to be usable: a call whose result was
        // thrown away still cost money, but the rows above return early and
        // are recorded the same way for the same reason.
        let provider_usage = completion
            .usage
            .as_ref()
            .and_then(|u| serde_json::to_value(u).ok());
        let counted = completion.usage.as_ref();
        if let Err(e) = self
            .usage
            .record(super::usage::RecordUsage {
                workspace_id,
                // No agent and no job: a summary is the platform's own work on
                // behalf of a session, not a turn the agent ran.
                agent_id: None,
                session_id: Some(session_id),
                user_id: None,
                account: None,
                reply_id: None,
                job_id: None,
                round: 0,
                traffic_type: summarise::TRAFFIC_TYPE.to_string(),
                endpoint,
                // What answered, falling back to what was asked for -- the
                // same order the turn path uses, so a route that rewrote the
                // model is visible in the ledger and a provider that names
                // nothing does not leave the column blank.
                model: if completion.model.is_empty() {
                    model.to_string()
                } else {
                    completion.model.clone()
                },
                credential_owner: paid_by,
                fallback: "none".to_string(),
                prompt_tokens: counted.map(|u| u.prompt_tokens as i32).unwrap_or(0),
                completion_tokens: counted.map(|u| u.completion_tokens as i32).unwrap_or(0),
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                usage_source: source_of(&provider_usage),
                provider_usage,
                service_tier: None,
            })
            .await
        {
            // Loud, like the turn path: a ledger write that fails means the
            // bill is wrong.
            tracing::error!(
                workspace_id = %workspace_id,
                session_id = %session_id,
                error = %e,
                "could not record the summary's usage"
            );
        }

        tracing::info!(
            messages_replaced = through,
            summary_bytes = summary.len(),
            "conversation summarised"
        );
        Some(replaced)
    }

    /// Writes a summary into the session, marked with what it stands in for.
    ///
    /// Best effort throughout: every failure path leaves the turn exactly as it
    /// would have been without storing, which is a summary that works for this
    /// turn and is written again next time. Nothing here is worth failing a
    /// turn over.
    async fn store_summary(
        &self,
        session_id: uuid::Uuid,
        summary: &str,
        through: uuid::Uuid,
        model: &str,
    ) {
        use super::chat::{Delivery, Usage};

        // Appended and then marked, because appending takes no metadata. A
        // summary that lost its mark between the two would be read as an
        // ordinary assistant message: wrong, but wrong in the direction of
        // saying too much rather than dropping the conversation.
        let stored = match self
            .chat
            .append_message(
                session_id,
                "assistant",
                summary,
                Some(model),
                Usage::default(),
                // Only meaningful for a user message arriving mid-turn; a
                // summary is neither, and the column takes the default.
                Delivery::default(),
                None,
            )
            .await
        {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "could not store the summary; it will be written again next turn");
                return;
            }
        };

        if let Err(e) = self
            .chat
            .set_message_content(
                stored.id,
                summary,
                Some(model),
                None,
                Usage::default(),
                serde_json::json!({ super::chat::summarise::SUMMARY_MARK: through.to_string() }),
            )
            .await
        {
            tracing::warn!(error = %e, "could not mark the summary; it will be written again next turn");
        }
    }

    /// Records what a turn produced, and closes the job out.
    ///
    /// Called with the stream a runtime is posting back, so everything that
    /// touches the transcript stays on this tier. The job is completed or
    /// failed here rather than by the runtime, which holds no database and
    /// should not be trusted to say whether its own work succeeded.
    ///
    /// `lease_token` is the claim the reporting runtime holds, already checked
    /// by the caller. Everything this writes is conditional on still holding
    /// it: a lease that lapses mid-report means the turn is out with another
    /// pod, whose result must stand.
    pub(super) async fn finish_turn(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        stream: impl futures::Stream<Item = Result<axum::body::Bytes, impl std::fmt::Display>> + Unpin,
    ) -> anyhow::Result<()> {
        let job = jobs::get(&self.pool, job_id).await?;
        let payload: ChatTurnPayload = serde_json::from_value(job.payload.clone())?;
        let lease_token = Some(lease_token);

        // Renewed for as long as results keep arriving. A turn runs for as
        // long as a model takes and the lease is deliberately short, so
        // without this the reaper hands the same turn to a second runtime
        // while the first is still streaming it -- and both would then write
        // the same reply.
        //
        // Held here rather than in the runtime because a lease is a claim on a
        // row, and the runtime holds no database. Arriving bytes are the
        // evidence the turn is alive, which is better evidence than a pod
        // saying so.
        // Quoted back on every renewal, so a heartbeat outliving its claim
        // cannot extend whichever claim replaced it.
        let heartbeat = {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                // Renewed immediately rather than after a first interval. The
                // lease started when the turn was handed out, and a runtime
                // that spent time generating before its first byte has already
                // burned some of it -- so the clock this heartbeat is racing
                // began before the heartbeat did.
                let mut ticker = tokio::time::interval(jobs::LEASE_HEARTBEAT);
                let Some(token) = lease_token else {
                    // Nothing to renew against: this job was not claimed the
                    // way a running turn is, so leave the lease alone rather
                    // than extending someone else's.
                    return;
                };
                loop {
                    ticker.tick().await;
                    match jobs::extend_lease(&pool, job_id, jobs::DEFAULT_LEASE, token).await {
                        // No longer ours: something reaped it, and renewing
                        // would take it back from whoever has it now.
                        Ok(false) => return,
                        Ok(true) => {}
                        Err(e) => tracing::warn!(job_id = %job_id, error = %e, "lease renewal failed"),
                    }
                }
            })
        };

        // The reply, not the prompt. `payload.message_id` is the message this
        // turn answers; deltas and the finished content belong to the reply
        // that hangs off it. Attaching them to the prompt streams the answer
        // into the user's own message and leaves the reply empty for ever --
        // which is what a reader sees as their words replaced and a thinking
        // indicator that never resolves.
        //
        // Taken back rather than passed in, because this tier is reached by a
        // runtime reporting a job id and nothing else. The unique index on
        // `replies_to` is what makes that unambiguous, and is the same thing
        // that lets a retry take back the reply it already made.
        let placeholder = self
            .chat
            .claim_placeholder(payload.message_id, payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("placeholder: {e}"))?;
        let reply_id = placeholder.message.id;

        let outcome = self.consume_turn(stream, &payload, reply_id, job_id).await;
        heartbeat.abort();

        let reply = match outcome {
            Ok(reply) => reply,
            Err(e) => {
                tracing::error!(job_id = %job_id, error = %e, "chat turn failed");
                if job.attempts >= job.max_attempts {
                    self.abandon_payload(&payload, &e.to_string()).await;
                } else {
                    let _ = events::append(
                        &self.pool,
                        payload.workspace_id,
                        Some(payload.session_id),
                        "chat.error",
                        serde_json::json!({ "message": e.to_string(), "message_id": payload.message_id }),
                    )
                    .await;
                }
                jobs::fail(&self.pool, job_id, &e.to_string(), Duration::from_secs(5), lease_token).await?;
                return Ok(());
            }
        };

        // Checked again before anything final is written. The heartbeat has
        // been renewing against this token; if that stopped succeeding, the
        // lease lapsed under a stall and this turn has been handed to another
        // pod, whose reply this must not overwrite.
        let still_ours = match lease_token {
            Some(token) => jobs::extend_lease(&self.pool, job_id, jobs::DEFAULT_LEASE, token).await?,
            None => false,
        };
        if !still_ours {
            anyhow::bail!("the lease on this turn lapsed while it was being reported");
        }

        let agent = self
            .agents
            .get(payload.workspace_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("agent: {e}"))?;

        let metadata = if reply.tools.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({ "tool_calls": reply.tools, "parts": reply.parts })
        };

        let finished = self
            .chat
            .set_message_content(
                reply_id,
                &reply.content,
                Some(&model_for(&agent.policy)),
                reply.provider.as_deref(),
                reply.usage,
                metadata,
            )
            .await
            .map_err(|e| anyhow::anyhow!("finalise: {e}"))?;

        events::append(
            &self.pool,
            payload.workspace_id,
            Some(payload.session_id),
            "chat.done",
            serde_json::json!({ "message_id": finished.id }),
        )
        .await?;

        // With the token this tier read when the report began. If the lease
        // lapsed meanwhile and the turn is running elsewhere, this returns
        // NotFound and the other pod's result stands.
        //
        // A turn somebody stopped ends as `cancelled` rather than `succeeded`:
        // it did produce a reply, and that reply was kept, but recording it as
        // an ordinary success loses the only evidence that the answer is short
        // because it was interrupted rather than because that was the answer.
        match jobs::cancel_requested(&self.pool, job_id).await {
            Ok(true) => jobs::mark_cancelled(&self.pool, job_id, lease_token).await?,
            // A failure to ask is not a reason to leave the job running: the
            // work is done either way, and the worse of the two records is the
            // one that says nothing finished.
            _ => jobs::complete(&self.pool, job_id, lease_token).await?,
        }

        // A conversation that has had a turn and still has no name gets one
        // asked for. After the job is closed, so a namer that cannot be
        // queued costs nothing but its absence.
        if let Ok(session) = self.chat.get_session(payload.workspace_id, payload.session_id).await {
            super::naming::enqueue_if_unnamed(&self.pool, &session).await;
        }
        Ok(())
    }

}

/// Model name from the agent's policy, falling back to the deployment default.
/// What class of traffic this agent's turns are, from its policy.
///
/// Names the work rather than the destination: the gateway decides where
/// "assistant" traffic goes, and can move it without the agent changing.
fn traffic_type_for(policy: &serde_json::Value) -> String {
    policy
        .get("traffic_type")
        .and_then(|t| t.as_str())
        .unwrap_or(crate::gateway::routing::DEFAULT_TRAFFIC_TYPE)
        .to_string()
}

fn model_for(policy: &serde_json::Value) -> String {
    policy
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            std::env::var("OUTTURN_DEFAULT_MODEL").unwrap_or_else(|_| "llama3.1".into())
        })
}




#[cfg(test)]
mod projection_tests {
    use super::*;
    use crate::api::chat::Message;

    fn message(role: &str, content: &str, metadata: serde_json::Value) -> Message {
        Message {
            id: Uuid::now_v7(),
            session_id: Uuid::now_v7(),
            role: role.to_string(),
            content: content.to_string(),
            metadata,
            delta_next: 0,
            model: None,
            prompt_tokens: None,
            completion_tokens: None,
            replies_to: None,
            absorbed_by: None,
            job_state: None,
        }
    }

    fn call(id: &str, result: Option<&str>) -> serde_json::Value {
        let mut call = serde_json::json!({
            "id": id,
            "name": "get_current_time",
            "action": "Checking the time",
            "arguments": "{}",
            "details": "the whole thing, for the reader",
        });
        if let Some(result) = result {
            call["result"] = serde_json::json!(result);
        }
        call
    }

    /// Every call answered, every answer to a call that was made.
    ///
    /// Both protocols reject a request that breaks this, so a projection that
    /// gets it wrong does not degrade -- it fails the turn.
    fn assert_well_formed(projected: &[serde_json::Value]) {
        let mut awaiting: Vec<String> = Vec::new();
        for message in projected {
            match message["role"].as_str() {
                Some("assistant") => {
                    for part in message["parts"].as_array().unwrap_or(&Vec::new()) {
                        if part["type"] == "call" {
                            let id = part["call"]["id"].as_str().expect("call id");
                            awaiting.push(id.to_string());
                        }
                    }
                }
                Some("tool") => {
                    let id = message["tool_call_id"].as_str().expect("tool_call_id");
                    let found = awaiting.iter().position(|a| a == id);
                    assert!(found.is_some(), "a result answered no call: {id}");
                    awaiting.remove(found.expect("checked"));
                }
                _ => {}
            }
        }
        assert!(awaiting.is_empty(), "calls left unanswered: {awaiting:?}");
    }

    #[test]
    fn a_turn_sees_nothing_sent_after_its_own_prompt() {
        // A later message is delivered as a steer by the gateway; delivering
        // it here as well is how the model came to answer "Bleargh 3" in the
        // reply to "Bleargh 2" and then be told about it again.
        let first = message("user", "Bleargh 2", serde_json::json!({}));
        let later = message("user", "Bleargh 3", serde_json::json!({}));
        let prompt = first.id;
        let kept = up_to(vec![first, later], prompt);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].content, "Bleargh 2");
    }

    /// A stored summary stands in for what it covers.
    ///
    /// The whole point of storing one: the next turn reads it instead of
    /// paying a model to write the same paragraph again.
    #[test]
    fn a_stored_summary_replaces_the_messages_behind_it() {
        let first = message("user", "the long beginning", serde_json::json!({}));
        let second = message("assistant", "a long answer", serde_json::json!({}));
        let summary = message(
            "assistant",
            "they discussed beginnings",
            serde_json::json!({ super::super::chat::summarise::SUMMARY_MARK: second.id.to_string() }),
        );
        let after = message("user", "and then?", serde_json::json!({}));

        let projected = project(&[first, second, summary, after]);

        assert_eq!(projected.len(), 2, "{projected:?}");
        assert_eq!(projected[0]["parts"][0]["text"], "they discussed beginnings");
        assert_eq!(projected[1]["parts"][0]["text"], "and then?");
    }

    /// The newest summary wins, and takes the older one with it.
    ///
    /// Each summary is written from the projection the one before it produced,
    /// so a later mark always covers an earlier summary as well as the
    /// messages after it. Carrying both would replay the same history twice.
    #[test]
    fn a_later_summary_supersedes_the_one_before_it() {
        let first = message("user", "the long beginning", serde_json::json!({}));
        let older = message(
            "assistant",
            "an older summary",
            serde_json::json!({ super::super::chat::summarise::SUMMARY_MARK: first.id.to_string() }),
        );
        let middle = message("user", "more talk", serde_json::json!({}));
        let newer = message(
            "assistant",
            "a newer summary",
            serde_json::json!({ super::super::chat::summarise::SUMMARY_MARK: middle.id.to_string() }),
        );
        let after = message("user", "and now?", serde_json::json!({}));

        let projected = project(&[first, older, middle, newer, after]);

        assert_eq!(projected.len(), 2, "{projected:?}");
        assert_eq!(projected[0]["parts"][0]["text"], "a newer summary");
        assert_eq!(projected[1]["parts"][0]["text"], "and now?");
    }

    /// A restarted conversation says why it stopped, and says it truthfully.
    ///
    /// The stop happened before the turn ran, so there is no partial reply to
    /// explain -- what needs explaining is the message nobody answered.
    #[test]
    fn a_restart_says_what_went_unanswered() {
        let projected = marked(
            vec![
                serde_json::json!({"role": "user", "parts": [{"type": "text", "text": "what is our Q3 revenue?"}]}),
                serde_json::json!({"role": "user", "parts": [{"type": "text", "text": "hello?"}]}),
            ],
            Some("monthly spend cap reached"),
        );

        assert_eq!(projected.len(), 3, "{projected:?}");
        let marker = projected[1]["parts"][0]["text"].as_str().expect("marker");
        assert!(marker.contains("monthly spend cap reached"), "{marker}");
        assert!(marker.contains("went unanswered"), "{marker}");
        // Both the question nobody answered and the one that restarted it are
        // still there, in order, after the marker.
        assert_eq!(projected[0]["parts"][0]["text"], "what is our Q3 revenue?");
        assert_eq!(projected[2]["parts"][0]["text"], "hello?");
    }

    /// An ordinary turn carries no marker at all.
    #[test]
    fn a_conversation_that_was_never_stopped_is_left_alone() {
        let original = vec![
            serde_json::json!({"role": "user", "parts": [{"type": "text", "text": "hello"}]}),
        ];
        assert_eq!(marked(original.clone(), None), original);
    }

    #[test]
    fn a_restart_with_nothing_before_it_still_explains_itself() {
        // The stop landed on the session's first prompt, so there is nothing
        // ahead of the marker. It still has to be said.
        let projected = marked(
            vec![serde_json::json!({"role": "user", "parts": [{"type": "text", "text": "hi"}]})],
            Some("stopped by an operator"),
        );
        assert_eq!(projected.len(), 2, "{projected:?}");
        assert!(
            projected[0]["parts"][0]["text"]
                .as_str()
                .expect("marker")
                .contains("stopped by an operator")
        );
        assert_eq!(projected[1]["parts"][0]["text"], "hi");
    }

    #[test]
    fn a_conversation_without_tools_is_unchanged() {
        let projected = project(&[
            message("user", "hello", serde_json::json!({})),
            message("assistant", "hi", serde_json::json!({})),
        ]);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0]["parts"][0]["text"], "hello");
        assert_eq!(projected[1]["parts"][0]["text"], "hi");
    }

    #[test]
    fn a_tool_call_is_replayed_before_the_answer_it_fed() {
        let projected = project(&[
            message("user", "what time is it?", serde_json::json!({})),
            message(
                "assistant",
                "It is Friday.",
                serde_json::json!({ "tool_calls": [call("call_1", Some("{\"weekday\":\"Friday\"}"))] }),
            ),
        ]);

        assert_eq!(projected.len(), 4, "{projected:#?}");
        assert_eq!(projected[1]["parts"][0]["call"]["id"], "call_1");
        assert_eq!(projected[2]["role"], "tool");
        assert_eq!(projected[2]["parts"][0]["text"], "{\"weekday\":\"Friday\"}");
        assert_eq!(projected[3]["parts"][0]["text"], "It is Friday.");
        assert_well_formed(&projected);
    }

    #[test]
    fn the_reader_only_view_is_never_sent() {
        let projected = project(&[message(
            "assistant",
            "done",
            serde_json::json!({ "tool_calls": [call("call_1", Some("short"))] }),
        )]);
        let sent = serde_json::to_string(&projected).expect("serialise");
        assert!(
            !sent.contains("for the reader"),
            "the untruncated result reached the model: {sent}"
        );
    }

    #[test]
    fn a_call_that_recorded_no_result_is_still_answered() {
        // A turn that died between asking and recording. Leaving the call
        // unanswered would make every later turn in this session malformed,
        // not just the one that failed.
        let projected = project(&[message(
            "assistant",
            "",
            serde_json::json!({ "tool_calls": [call("call_1", None)] }),
        )]);
        assert_well_formed(&projected);
        assert!(
            projected[1]["parts"][0]["text"]
                .as_str()
                .expect("text")
                .contains("error")
        );
    }

    #[test]
    fn a_turn_that_only_called_tools_adds_no_empty_message() {
        let projected = project(&[message(
            "assistant",
            "",
            serde_json::json!({ "tool_calls": [call("call_1", Some("ok"))] }),
        )]);
        assert_eq!(projected.len(), 2, "an empty reply was sent: {projected:#?}");
    }

    /// A turn that spoke, looked something up, then spoke again.
    ///
    /// The old projection could not say this: it replayed every call before
    /// every word, so the model was shown itself acting before it had spoken.
    /// The reply splits where the result has to land, and nowhere else.
    #[test]
    fn text_before_a_call_is_replayed_before_it() {
        let projected = project(&[message(
            "assistant",
            "Let me check. It is Friday.",
            serde_json::json!({
                "tool_calls": [call("call_1", Some("{\"weekday\":\"Friday\"}"))],
                "parts": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "call", "id": "call_1"},
                    {"type": "text", "text": "It is Friday."}
                ]
            }),
        )]);

        assert_eq!(projected.len(), 3, "{projected:#?}");
        // What it had said when it called, and the call, in one message.
        assert_eq!(projected[0]["parts"][0]["text"], "Let me check.");
        assert_eq!(projected[0]["parts"][1]["call"]["id"], "call_1");
        // Then the answer, then what it said once answered.
        assert_eq!(projected[1]["role"], "tool");
        assert_eq!(projected[2]["parts"][0]["text"], "It is Friday.");
        assert_well_formed(&projected);
    }

    #[test]
    fn several_calls_in_one_turn_all_get_answers() {
        let projected = project(&[message(
            "assistant",
            "both done",
            serde_json::json!({
                "tool_calls": [call("call_1", Some("a")), call("call_2", Some("b"))]
            }),
        )]);
        assert_well_formed(&projected);
        let calls = projected[0]["parts"]
            .as_array()
            .expect("parts")
            .iter()
            .filter(|p| p["type"] == "call")
            .count();
        assert_eq!(calls, 2);
    }
}

#[cfg(test)]
mod usage_source_tests {
    use super::*;
    use crate::api::usage::UsageSource;

    /// A provider that said what its call cost is the authority on it.
    #[test]
    fn a_reported_usage_object_is_recorded_as_reported() {
        let usage = Some(serde_json::json!({ "prompt_tokens": 100, "completion_tokens": 20 }));
        assert_eq!(source_of(&usage), UsageSource::Reported);
    }

    /// A provider that said nothing leaves zeros, and zeros that claim to have
    /// been measured are the bug this column exists to prevent: a turn nobody
    /// counted then reads exactly like a turn that was free, and no later
    /// inspection can tell them apart.
    #[test]
    fn a_round_the_provider_never_costed_is_not_recorded_as_measured() {
        for absent in [None, Some(serde_json::Value::Null)] {
            assert_eq!(
                source_of(&absent),
                UsageSource::Unknown,
                "zeros with no provider figure behind them must say so, or a \
                 bill cannot be argued from this ledger"
            );
            assert_ne!(
                source_of(&absent),
                UsageSource::Reported,
                "recording an unmeasured round as reported is the silent error"
            );
        }
    }
}
