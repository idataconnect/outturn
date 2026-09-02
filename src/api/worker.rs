use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use uuid::Uuid;

use std::sync::atomic::{AtomicI64, Ordering};

use crate::auth::{Role, TokenMinter};
use crate::events;
use crate::jobs;
use crate::runtime::component::{
    AgentRunner, Message as GuestMessage, ProgressSink, RunOptions,
};

use super::agent::AgentStore;
use super::chat::{ChatStore, Usage};

/// Job kind for "the user said something; produce a reply".
pub const CHAT_TURN: &str = "chat.turn";

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatTurnPayload {
    pub tenant_id: Uuid,
    pub session_id: Uuid,
    pub agent_id: Uuid,
}

/// Bounds a runaway guest. Generous enough for a long conversation, finite so
/// a loop cannot occupy a worker indefinitely.
const FUEL_PER_TURN: u64 = 50_000_000_000;

pub struct Worker {
    pub pool: PgPool,
    pub agents: Arc<dyn AgentStore>,
    pub chat: Arc<dyn ChatStore>,
    pub minter: Arc<TokenMinter>,
    pub gateway_url: String,
    /// Compiled once and reused: instantiation is cheap, compilation is not.
    pub runner: Arc<AgentRunner>,
    /// The component every agent currently runs.
    pub agent_module: Arc<Vec<u8>>,
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

        let conversation: Vec<GuestMessage> = history
            .iter()
            .map(|m| GuestMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
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

        // Deltas are announced as they arrive, each carrying its index so a
        // reader can tell a dropped one from a slow one.
        let sink: ProgressSink = {
            let pool = self.pool.clone();
            let tenant_id = payload.tenant_id;
            let session_id = payload.session_id;
            let message_id = placeholder.id;
            let index = Arc::new(AtomicI64::new(0));

            Arc::new(move |text: &str| {
                let pool = pool.clone();
                let text = text.to_string();
                let idx = index.fetch_add(1, Ordering::SeqCst);
                // Spawned because the sink is synchronous: it is called from
                // the host while the guest is blocked, and must not await.
                tokio::spawn(async move {
                    let _ = events::append(
                        &pool,
                        tenant_id,
                        Some(session_id),
                        "chat.delta",
                        serde_json::json!({
                            "message_id": message_id,
                            "idx": idx,
                            "text": text,
                        }),
                    )
                    .await;
                });
            })
        };

        // A token minted for this turn, carrying only what the guest needs.
        let token = self.minter.mint(
            payload.session_id,
            payload.tenant_id,
            &[Role::Operator],
        )?;

        let reply = self
            .runner
            .run(
                &self.agent_module,
                conversation,
                agent.system_prompt.clone(),
                RunOptions {
                    session_id: payload.session_id,
                    gateway_url: self.gateway_url.clone(),
                    gateway_token: token,
                    default_model: model_for(&agent.policy),
                    progress: Some(sink),
                    fuel: FUEL_PER_TURN,
                },
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

        let finished = self
            .chat
            .set_message_content(placeholder.id, &reply, Some(&model_for(&agent.policy)))
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
                "seq": finished.seq,
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



