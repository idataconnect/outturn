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

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatTurnPayload {
    pub tenant_id: Uuid,
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
    let mut projected = Vec::with_capacity(messages.len());

    for message in messages {
        let calls = message
            .metadata
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .filter(|c| !c.is_empty());

        if let Some(calls) = calls {
            projected.push(serde_json::json!({
                "role": "assistant",
                "content": "",
                "tool_calls": calls
                    .iter()
                    .map(|c| serde_json::json!({
                        "id": c["id"],
                        "name": c["name"],
                        "arguments": c["arguments"].as_str().unwrap_or("{}"),
                    }))
                    .collect::<Vec<_>>(),
            }));

            for call in calls {
                projected.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call["id"],
                    // Every call must be answered. A turn that died between
                    // asking and recording leaves one without a result, and a
                    // request carrying an unanswered call is rejected outright
                    // -- so the gap is filled rather than left to break the
                    // next turn as well.
                    "content": call["result"]
                        .as_str()
                        .unwrap_or("{\"error\":\"no result was recorded\"}"),
                }));
            }
        }

        // An assistant message that only called tools has nothing else to say,
        // and an empty one costs tokens to communicate that.
        if !message.content.is_empty() || calls.is_none() {
            projected.push(serde_json::json!({
                "role": message.role,
                "content": message.content,
            }));
        }
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

/// What a completed turn produced.
pub(super) struct TurnOutcome {
    content: String,
    /// Tool calls the agent made, in order, each with the model's own label.
    tools: Vec<serde_json::Value>,
    /// Summed across every round of the turn, counted by the runtime host.
    usage: Usage,
    /// The endpoint that served it, for attributing spend.
    provider: Option<String>,
}

pub struct Worker {
    pub pool: PgPool,
    pub agents: Arc<dyn AgentStore>,
    pub chat: Arc<dyn ChatStore>,
    /// Where every model call is written down, as it is reported.
    pub usage: Arc<dyn super::usage::UsageStore>,
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
            payload.tenant_id,
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
                        events::append(
                            &self.pool,
                            payload.tenant_id,
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
                        events::append(
                            &self.pool,
                            payload.tenant_id,
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
                            payload.tenant_id,
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
                    }) => {
                        // Written the moment the call is known to have cost
                        // something, not when the turn ends: a turn that fails
                        // after three calls still bills for three.
                        let account = match &account {
                            Some(a) => a.clone(),
                            None => {
                                let found = self
                                    .chat
                                    .get_session(payload.tenant_id, payload.session_id)
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
                                tenant_id: payload.tenant_id,
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
                                        .get(payload.tenant_id, payload.agent_id)
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
                            })
                            .await
                        {
                            // A ledger write that fails is an operator's
                            // problem, and loud: the bill is wrong.
                            tracing::error!(
                                job_id = %job_id,
                                tenant_id = %payload.tenant_id,
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
            .get(payload.tenant_id, payload.agent_id)
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

        let history = self
            .chat
            .messages(payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("history: {e}"))?;
        let history = up_to(history.messages, payload.message_id);

        let egress = super::egress::rules_for(&self.pool, payload.tenant_id)
            .await
            .map_err(|e| anyhow::anyhow!("egress rules: {e}"))?;

        let placeholder = self
            .chat
            .claim_placeholder(payload.message_id, payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("placeholder: {e}"))?;

        if placeholder.created {
            events::append(
                &self.pool,
                payload.tenant_id,
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
                payload.tenant_id,
                Some(payload.session_id),
                "chat.retry",
                serde_json::json!({
                    "message_id": placeholder.message.id,
                    "replies_to": payload.message_id,
                }),
            )
            .await?;
        }

        Ok(Some(crate::runtime::router::ExecuteRequest {
            session_id: payload.session_id,
            tenant_id: payload.tenant_id,
            conversation: project(&history)
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<_, _>>()?,
            system_prompt: agent.system_prompt,
            model: Some(model_for(&agent.policy)),
            timezone: payload.timezone.clone(),
            reasoning_effort: reasoning_effort_for(&agent.policy),
            traffic_type: Some(traffic_type_for(&agent.policy)),
            max_tool_rounds: max_tool_rounds_for(&agent.policy),
            reply_id: placeholder.message.id,
            egress,
        }))
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
                        payload.tenant_id,
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
            .get(payload.tenant_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("agent: {e}"))?;

        let metadata = if reply.tools.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({ "tool_calls": reply.tools })
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
            payload.tenant_id,
            Some(payload.session_id),
            "chat.done",
            serde_json::json!({ "message_id": finished.id }),
        )
        .await?;

        // With the token this tier read when the report began. If the lease
        // lapsed meanwhile and the turn is running elsewhere, this returns
        // NotFound and the other pod's result stands.
        jobs::complete(&self.pool, job_id, lease_token).await?;
        Ok(())
    }

}

/// Model name from the agent's policy, falling back to the deployment default.
/// How much the model should deliberate, from the agent's policy.
///
/// Absent leaves the provider's default alone. "none" turns thinking off where
/// it is supported, which is worth doing for agents whose work does not need
/// it: it cuts a gemma4 tool turn from 113 completion tokens to 24.
fn reasoning_effort_for(policy: &serde_json::Value) -> Option<String> {
    policy
        .get("reasoning_effort")
        .and_then(|e| e.as_str())
        .map(str::to_string)
}

/// How many model calls a turn of this agent may make.
///
/// Absent leaves the runtime's default. Zero or negative disables the limit,
/// for agents whose work legitimately runs long.
fn max_tool_rounds_for(policy: &serde_json::Value) -> Option<i64> {
    policy.get("max_tool_rounds").and_then(|r| r.as_i64())
}

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
                    for call in message["tool_calls"].as_array().unwrap_or(&Vec::new()) {
                        awaiting.push(call["id"].as_str().expect("call id").to_string());
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

    #[test]
    fn a_conversation_without_tools_is_unchanged() {
        let projected = project(&[
            message("user", "hello", serde_json::json!({})),
            message("assistant", "hi", serde_json::json!({})),
        ]);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0]["content"], "hello");
        assert_eq!(projected[1]["content"], "hi");
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
        assert_eq!(projected[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(projected[2]["role"], "tool");
        assert_eq!(projected[2]["content"], "{\"weekday\":\"Friday\"}");
        assert_eq!(projected[3]["content"], "It is Friday.");
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
        assert!(projected[1]["content"].as_str().expect("content").contains("error"));
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
        assert_eq!(projected[0]["tool_calls"].as_array().expect("calls").len(), 2);
    }
}
