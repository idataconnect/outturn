use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::auth::{Role, TokenMinter};
use crate::events;
use crate::gateway::llm::types::{
    ChatCompletionRequest, ChatCompletionResponse, Message as LlmMessage, MessageContent,
    Role as LlmRole,
};
use crate::jobs;

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

pub struct Worker {
    pub pool: PgPool,
    pub agents: Arc<dyn AgentStore>,
    pub chat: Arc<dyn ChatStore>,
    pub minter: Arc<TokenMinter>,
    pub gateway_url: String,
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
                        if let Err(e) = self.tick().await {
                            tracing::error!(error = %e, "chat worker tick failed");
                        }
                    }
                }
            }
        });
    }

    async fn tick(&self) -> anyhow::Result<()> {
        // Returning abandoned leases first means a crashed worker's turn is
        // retried rather than left hanging.
        jobs::reap_abandoned(&self.pool).await?;

        let claimed = jobs::claim(&self.pool, &[CHAT_TURN], 4, jobs::DEFAULT_LEASE).await?;
        for handle in claimed {
            let id = handle.job.id;
            match self.run_turn(&handle.job.payload).await {
                Ok(()) => jobs::complete(&self.pool, id).await?,
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
                    jobs::fail(&self.pool, id, &e.to_string(), Duration::from_secs(5)).await?;
                }
            }
        }
        Ok(())
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

        // The agent's system prompt leads every turn, ahead of the stored
        // conversation, so editing it takes effect on the next message.
        let mut messages = Vec::with_capacity(history.len() + 1);
        if !agent.system_prompt.is_empty() {
            messages.push(plain(LlmRole::System, &agent.system_prompt));
        }
        for m in &history {
            messages.push(plain(role_from_str(&m.role), &m.content));
        }

        let model = model_for(&agent.policy);
        let request = ChatCompletionRequest {
            model: model.clone(),
            messages,
            tools: None,
            temperature: None,
            max_tokens: None,
            stream: false,
        };

        // The worker acts on behalf of the session, so it mints a short-lived
        // service token rather than reusing the browser's.
        let token = self
            .minter
            .mint(payload.session_id, payload.tenant_id, &[Role::Operator])?;

        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.gateway_url))
            .bearer_auth(token)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("gateway returned {status}: {body}");
        }

        let completion: ChatCompletionResponse = response.json().await?;
        let reply = completion
            .choices
            .first()
            .map(|c| text_of(&c.message.content))
            .unwrap_or_default();

        let usage = completion
            .usage
            .as_ref()
            .map(|u| Usage {
                prompt_tokens: Some(u.prompt_tokens as i32),
                completion_tokens: Some(u.completion_tokens as i32),
            })
            .unwrap_or_default();

        let message = self
            .chat
            .append_message(payload.session_id, "assistant", &reply, Some(&model), usage)
            .await
            .map_err(|e| anyhow::anyhow!("append: {e}"))?;

        events::append(
            &self.pool,
            payload.tenant_id,
            Some(payload.session_id),
            "chat.message",
            serde_json::to_value(&message)?,
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

fn plain(role: LlmRole, content: &str) -> LlmMessage {
    LlmMessage {
        role,
        content: MessageContent::Text(content.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Stored roles are constrained by the table's check constraint, so an
/// unrecognised value cannot occur; treat one as `user` rather than panicking.
fn role_from_str(role: &str) -> LlmRole {
    match role {
        "system" => LlmRole::System,
        "assistant" => LlmRole::Assistant,
        "tool" => LlmRole::Tool,
        _ => LlmRole::User,
    }
}

/// Flattens multi-part content into text. Parts only appear for image input,
/// which this path does not yet produce.
fn text_of(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                crate::gateway::llm::types::ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}
