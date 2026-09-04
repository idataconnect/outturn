use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use uuid::Uuid;

use std::sync::atomic::{AtomicI64, Ordering};

use crate::auth::{Role, TokenMinter};
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
}

/// What a completed turn produced.
struct TurnOutcome {
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
    pub minter: Arc<TokenMinter>,
    /// Where agents execute. A separate service so a runaway guest competes
    /// for its own CPU rather than the API's, and so the two scale apart.
    pub runtime_url: String,
    pub http: reqwest::Client,
}

impl Worker {
    /// Runs until the process shuts down.
    ///
    /// Polls rather than listening: turns are enqueued through the job table,
    /// and a missed wake-up simply waits for the next tick.
    pub fn spawn(self: Arc<Self>, shutdown: Arc<tokio::sync::Notify>) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = shutdown.notified() => {
                        tracing::info!("chat worker stopping");
                        return;
                    }
                    _ = ticker.tick() => {
                        if let Err(e) = Arc::clone(&self).tick().await {
                            tracing::error!(error = %e, "chat worker tick failed");
                        }
                    }
                }
            }
        });
    }

    async fn tick(self: Arc<Self>) -> anyhow::Result<()> {
        // Returning abandoned leases first means a crashed worker's turn is
        // retried rather than left hanging.
        jobs::reap_abandoned(&self.pool).await?;

        let claimed = jobs::claim(&self.pool, &[CHAT_TURN], 4, jobs::DEFAULT_LEASE).await?;

        // Each turn runs on its own task: generation can take minutes, and
        // awaiting it here would stall every other session behind it.
        for handle in claimed {
            let worker = Arc::clone(&self);
            tokio::spawn(async move { worker.execute(handle).await });
        }
        Ok(())
    }

    async fn execute(self: Arc<Self>, handle: jobs::JobHandle) {
        let id = handle.job.id;

        // Keep the lease alive while this runs, so a slow generation is not
        // reaped into the queue and executed a second time.
        let heartbeat = {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(jobs::LEASE_HEARTBEAT);
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    match jobs::extend_lease(&pool, id, jobs::DEFAULT_LEASE).await {
                        Ok(true) => {}
                        // The job is no longer ours; stop renewing.
                        Ok(false) => return,
                        Err(e) => tracing::warn!(job_id = %id, error = %e, "lease renewal failed"),
                    }
                }
            })
        };

        let result = self.run_turn(&handle.job.payload).await;
        heartbeat.abort();

        match result {
            Ok(()) => {
                if let Err(e) = jobs::complete(&self.pool, id).await {
                    tracing::error!(job_id = %id, error = %e, "failed to complete job");
                }
            }
            Err(e) => {
                tracing::error!(job_id = %id, error = %e, "chat turn failed");
                // The last attempt is giving up, so the empty reply it left
                // has to go: nothing will fill it, and an abandoned one wedges
                // the session against further messages.
                let final_attempt = handle.job.attempts >= handle.job.max_attempts;
                if final_attempt
                    && let Ok(payload) =
                        serde_json::from_value::<ChatTurnPayload>(handle.job.payload.clone())
                    && let Err(e) = self.chat.discard_placeholder(payload.message_id).await
                {
                    tracing::error!(job_id = %id, error = %e, "failed to discard placeholder");
                }

                // Tell the browser rather than leaving it polling forever.
                if let Ok(payload) =
                    serde_json::from_value::<ChatTurnPayload>(handle.job.payload.clone())
                {
                    let _ = events::append(
                        &self.pool,
                        payload.tenant_id,
                        Some(payload.session_id),
                        "chat.error",
                        serde_json::json!({ "message": e.to_string() }),
                    )
                    .await;
                }
                if let Err(e) = jobs::fail(&self.pool, id, &e.to_string(), Duration::from_secs(5)).await
                {
                    tracing::error!(job_id = %id, error = %e, "failed to record job failure");
                }
            }
        }
    }

    /// Runs a turn on the runtime service, writing each delta to the event
    /// feed as it arrives.
    ///
    /// The runtime holds no database, so the transcript stays owned by this
    /// tier: deltas travel back over the response and are recorded here.
    async fn execute_on_runtime(
        &self,
        token: &str,
        payload: &ChatTurnPayload,
        conversation: Vec<serde_json::Value>,
        system_prompt: &str,
        model: &str,
        reasoning_effort: Option<&str>,
        traffic_type: &str,
        max_tool_rounds: Option<i64>,
        message_id: Uuid,
    ) -> anyhow::Result<TurnOutcome> {
        use futures::StreamExt;

        let response = self
            .http
            .post(format!("{}/v1/execute", self.runtime_url))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "session_id": payload.session_id,
                "tenant_id": payload.tenant_id,
                "conversation": conversation,
                "system_prompt": system_prompt,
                "model": model,
                "timezone": payload.timezone,
                "reasoning_effort": reasoning_effort,
                "traffic_type": traffic_type,
                "max_tool_rounds": max_tool_rounds,
                "reply_id": message_id,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            anyhow::bail!("runtime returned {status}: {detail}");
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut tools: Vec<serde_json::Value> = Vec::new();

        while let Some(bytes) = stream.next().await {
            buffer.push_str(std::str::from_utf8(&bytes?)?);

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
                    Ok(ExecuteEvent::Tool { id, name, action }) => {
                        let call = serde_json::json!({
                            "id": id,
                            "name": name,
                            "action": action,
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

    async fn run_turn(&self, payload: &serde_json::Value) -> anyhow::Result<()> {
        let payload: ChatTurnPayload = serde_json::from_value(payload.clone())?;

        let agent = self
            .agents
            .get(payload.tenant_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("agent: {e}"))?;

        if !agent.enabled {
            anyhow::bail!("agent is disabled");
        }

        let history = self
            .chat
            .messages(payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("history: {e}"))?;

        let conversation: Vec<serde_json::Value> = history
            .messages
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();

        // The assistant's message is created empty before generation starts, so
        // deltas attach to a row that already exists. Without this the browser
        // would render a streaming buffer and then swap it for a loaded
        // message, and any difference between the two would flash.
        // A steered message was answered inside the turn it interrupted, so
        // this queued turn has nothing left to do. Skipped rather than run,
        // which would produce a second reply to a question already addressed.
        if self
            .chat
            .was_absorbed(payload.message_id)
            .await
            .map_err(|e| anyhow::anyhow!("absorbed: {e}"))?
        {
            tracing::info!(
                session_id = %payload.session_id,
                message_id = %payload.message_id,
                "prompt was answered mid-turn; nothing to do"
            );
            return Ok(());
        }

        // Idempotent: a retry after a worker died mid-turn takes back the
        // reply it already created rather than starting a second one.
        let placeholder = self
            .chat
            .claim_placeholder(payload.message_id, payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("placeholder: {e}"))?;

        events::append(
            &self.pool,
            payload.tenant_id,
            Some(payload.session_id),
            "chat.message",
            serde_json::to_value(&placeholder)?,
        )
        .await?;

        // The runtime executes the agent and streams its progress back. Each
        // delta is announced as it arrives, carrying an index so a reader can
        // tell a dropped one from a slow one.
        let token = self.minter.mint(
            payload.session_id,
            payload.tenant_id,
            &[Role::Operator],
        )?;

        let reply = self
            .execute_on_runtime(
                &token,
                &payload,
                conversation,
                &agent.system_prompt,
                &model_for(&agent.policy),
                reasoning_effort_for(&agent.policy).as_deref(),
                &traffic_type_for(&agent.policy),
                max_tool_rounds_for(&agent.policy),
                placeholder.id,
            )
            .await;

        let reply = match reply {
            Ok(reply) => reply,
            Err(e) => {
                // Left in place when the turn will be retried -- the retry
                // fills it. Only a turn that is giving up discards it, which
                // happens where the job is marked permanently failed.
                return Err(e);
            }
        };

        let metadata = if reply.tools.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({ "tool_calls": reply.tools })
        };

        let finished = self
            .chat
            .set_message_content(
                placeholder.id,
                &reply.content,
                Some(&model_for(&agent.policy)),
                reply.provider.as_deref(),
                reply.usage,
                metadata,
            )
            .await
            .map_err(|e| anyhow::anyhow!("finalise: {e}"))?;

        // Carries no content: the browser has already rendered the deltas, and
        // sending the text again would invite a client to replace what it has
        // and flash if the two ever differed.
        events::append(
            &self.pool,
            payload.tenant_id,
            Some(payload.session_id),
            "chat.done",
            serde_json::json!({
                "message_id": finished.id,
            }),
        )
        .await?;

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



