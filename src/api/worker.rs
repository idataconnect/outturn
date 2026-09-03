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
    /// IANA zone the sender was in, e.g. "Europe/London". Carried on the turn
    /// rather than the account: it is where the user is now, and a laptop that
    /// crosses a border should not keep answering in the zone it left.
    #[serde(default)]
    pub timezone: Option<String>,
}

/// What a completed turn produced.
struct TurnOutcome {
    content: String,
    /// Tool calls the agent made, in order, each with the model's reason.
    tools: Vec<serde_json::Value>,
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
                    Ok(ExecuteEvent::Tool { id, name, reason }) => {
                        let call = serde_json::json!({
                            "id": id,
                            "name": name,
                            "reason": reason,
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
                    Ok(ExecuteEvent::Done { content }) => {
                        return Ok(TurnOutcome { content, tools });
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
        let placeholder = self
            .chat
            .append_message(payload.session_id, "assistant", "", None, Usage::default())
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
                placeholder.id,
            )
            .await;

        let reply = match reply {
            Ok(reply) => reply,
            Err(e) => {
                // The placeholder would otherwise sit empty forever.
                let _ = self.chat.delete_message(placeholder.id).await;
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
fn model_for(policy: &serde_json::Value) -> String {
    policy
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            std::env::var("OUTTURN_DEFAULT_MODEL").unwrap_or_else(|_| "llama3.1".into())
        })
}



