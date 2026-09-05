use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use uuid::Uuid;


use crate::auth::{Role, TokenMinter};
use crate::events;
use crate::jobs;
use crate::runtime::router::ExecuteEvent;

use super::agent::AgentStore;
use super::chat::{ChatStore, Usage};

/// Turns this pod will have in flight before it stops claiming.
///
/// The API's share of a turn is a task and an open stream, which is cheap --
/// but claiming is what takes work off the queue, and work taken off the queue
/// is work no other pod can serve. An unbounded claimer turns a shared backlog
/// into a private one.
const DEFAULT_MAX_IN_FLIGHT_TURNS: usize = 16;

/// Claimed per tick at most, so a burst is spread over ticks rather than
/// landing on one pod because it happened to ask first.
const CLAIM_BATCH: i64 = 4;

/// How long a turn waits after a runtime refused it for want of room.
///
/// Short, because the refusal says nothing is wrong -- only that every pod
/// asked so far was busy. Long enough that retrying is not itself the load.
const NO_ROOM_BACKOFF: Duration = Duration::from_secs(2);

/// The runtime had no room for this turn.
///
/// Distinguished from every other error because it is not a failure: nothing
/// was attempted, nothing is wrong with the job, and counting it as an attempt
/// would let a busy cluster exhaust a turn's retries without ever running it.
#[derive(Debug)]
struct NoRoom(String);

impl std::fmt::Display for NoRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "runtime had no room: {}", self.0)
    }
}

impl std::error::Error for NoRoom {}

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
    /// Slots for turns this pod is carrying. Bounds what it will claim, so a
    /// backlog stays in the queue where other pods -- and the autoscaler --
    /// can see it.
    pub in_flight: Arc<tokio::sync::Semaphore>,
}

/// The in-flight bound this pod will use.
pub fn max_in_flight_turns() -> usize {
    std::env::var("OUTTURN_MAX_IN_FLIGHT_TURNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_IN_FLIGHT_TURNS)
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

        // Claim no more than can be carried. Leaving work in the queue is the
        // point: it stays visible to every other pod, and to whatever is
        // deciding how many pods there should be.
        let room = self.in_flight.available_permits();
        if room == 0 {
            return Ok(());
        }
        let batch = CLAIM_BATCH.min(room as i64);

        let claimed = jobs::claim(&self.pool, &[CHAT_TURN], batch, jobs::DEFAULT_LEASE).await?;

        // Each turn runs on its own task: generation can take minutes, and
        // awaiting it here would stall every other session behind it.
        for handle in claimed {
            let worker = Arc::clone(&self);
            // Taken here rather than inside the task, so a claim and the slot
            // it occupies cannot drift apart. try_acquire because the room was
            // checked above and anything else is a bug rather than a wait.
            let Ok(slot) = Arc::clone(&self.in_flight).try_acquire_owned() else {
                tracing::warn!(job_id = %handle.job.id, "claimed past capacity; releasing");
                let _ =
                    jobs::release(&self.pool, handle.job.id, NO_ROOM_BACKOFF, jobs::MAX_RELEASES)
                        .await;
                continue;
            };
            tokio::spawn(async move {
                let _slot = slot;
                worker.execute(handle).await
            });
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
            Err(e) if e.downcast_ref::<NoRoom>().is_some() => {
                // Back on the queue untouched. Nothing ran, so nothing failed,
                // and the attempt this claim took is given back.
                match jobs::release(&self.pool, id, NO_ROOM_BACKOFF, jobs::MAX_RELEASES).await {
                    Ok(jobs::Released::Queued) => {
                        tracing::debug!(job_id = %id, reason = %e, "turn returned to the queue");
                    }
                    // Long past the point where this is a busy cluster. The
                    // reader has been watching an indicator the whole time and
                    // is owed an answer, even a disappointing one.
                    Ok(jobs::Released::GaveUp) => {
                        tracing::error!(
                            job_id = %id,
                            releases = jobs::MAX_RELEASES,
                            "no runtime had room; giving up on the turn"
                        );
                        self.abandon(&handle, "no runtime had room for this turn").await;
                    }
                    Err(e) => {
                        tracing::error!(job_id = %id, error = %e, "failed to release job");
                    }
                }
            }
            Err(e) => {
                tracing::error!(job_id = %id, error = %e, "chat turn failed");
                // The last attempt is giving up, so the empty reply it left
                // has to go: nothing will fill it, and an abandoned one wedges
                // the session against further messages.
                if handle.job.attempts >= handle.job.max_attempts {
                    self.abandon(&handle, &e.to_string()).await;
                } else if let Ok(payload) =
                    serde_json::from_value::<ChatTurnPayload>(handle.job.payload.clone())
                {
                    // Still has attempts left, so the reply stays for the
                    // retry to take back -- but the reader is told the round
                    // failed rather than left watching.
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

    /// Gives up on a turn: clears the reply nothing will fill, and says so.
    ///
    /// An empty reply left behind wedges the session against further messages,
    /// and a reader with no error event waits on an indicator that resolves on
    /// no timescale at all. Both halves matter, which is why they are one
    /// function rather than two blocks that drifted apart.
    async fn abandon(&self, handle: &jobs::JobHandle, reason: &str) {
        let Ok(payload) = serde_json::from_value::<ChatTurnPayload>(handle.job.payload.clone())
        else {
            return;
        };

        if let Err(e) = self.chat.discard_placeholder(payload.message_id).await {
            tracing::error!(job_id = %handle.job.id, error = %e, "failed to discard placeholder");
        }

        let _ = events::append(
            &self.pool,
            payload.tenant_id,
            Some(payload.session_id),
            "chat.error",
            serde_json::json!({ "message": reason }),
        )
        .await;
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
            // A runtime at capacity says so with 503. That is a statement
            // about the pod, not the turn, so the turn goes back on the queue
            // intact rather than being marked as having failed once.
            if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                return Err(anyhow::Error::new(NoRoom(detail)));
            }
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

        let conversation = project(&history.messages);

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

        // Announced only when this attempt made it. A turn handed back for
        // want of room comes round again every couple of seconds, and saying
        // "here is a message" each time writes an event per cycle for a
        // message the browser already has.
        if placeholder.created {
            events::append(
                &self.pool,
                payload.tenant_id,
                Some(payload.session_id),
                "chat.message",
                serde_json::to_value(&placeholder.message)?,
            )
            .await?;
        }

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
                placeholder.message.id,
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
                placeholder.message.id,
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
