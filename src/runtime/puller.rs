//! Asking for work, rather than waiting to be given it.
//!
//! A pod with a free slot asks the API for a turn and is given one. A pod with
//! no room does not ask, so it is never offered work it would have to refuse.
//!
//! Capacity is not a slot count. A turn's cost is unknown when it is taken and
//! a turn accepted a moment ago is still growing into memory, so the pod
//! charges itself an assumed cost the instant it takes work and only replaces
//! that with the real figure once the turn has grown into it. Asking is
//! therefore a statement about memory, not about a counter.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use uuid::Uuid;

use super::admission::Admission;
use super::component::{AgentRunner, Message, RunOptions};
use super::router::{ExecuteEvent, ExecuteRequest};

/// How long to wait before asking again after being told there is no work.
///
/// Short, because this is the idle path and the poll it follows already waited
/// on the server for its own timeout. This only covers the gap between one
/// poll returning empty and the next beginning.
const IDLE_PAUSE: Duration = Duration::from_millis(100);

/// How long to wait before asking again after a failure.
///
/// Longer, because a failure means the API is unwell and asking harder does
/// not help it.
const ERROR_PAUSE: Duration = Duration::from_secs(2);

#[derive(serde::Deserialize)]
struct Assignment {
    job_id: Uuid,
    gateway_token: String,
    /// Quoted back on every report, so the API can tell this pod's account
    /// of the turn from a later holder's.
    lease_token: Uuid,
    #[serde(flatten)]
    request: ExecuteRequest,
}

pub struct Puller {
    pub api_url: String,
    /// What this pod presents to the API. A shared key that means only "the
    /// runtime tier": this pod executes tenant components and so holds
    /// nothing that could mint a credential for anyone. See `RuntimeKey`.
    pub runtime_key: String,
    pub http: reqwest::Client,
    pub runner: Arc<AgentRunner>,
    pub agent_module: Arc<Vec<u8>>,
    pub storage: Option<Arc<dyn super::storage::StorageBackend>>,
    pub gateway_url: String,
    pub admission: Arc<Admission>,
    pub default_model: String,
    pub idle_timeout: Duration,
}

impl Puller {
    /// Runs until the process shuts down.
    pub fn spawn(self: Arc<Self>, shutdown: Arc<tokio::sync::Notify>) {
        // Whether this pod has ever reached the API. Until it has, a failure
        // to do so is a dependency starting up rather than a fault, and is
        // said once instead of every two seconds.
        let settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let announced = Arc::new(std::sync::atomic::AtomicBool::new(false));

        tokio::spawn(async move {
            loop {
                // Asking is gated on having somewhere to put the answer. A pod
                // that cannot take a turn does not ask for one, which is the
                // whole of admission control now -- there is nothing to refuse
                // because nothing is offered.
                let permit = match self.admission.try_admit() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tokio::select! {
                            _ = tokio::time::sleep(IDLE_PAUSE) => continue,
                            _ = shutdown.notified() => return,
                        }
                    }
                };

                let pause = match Arc::clone(&self).take_one(permit).await {
                    Ok(true) => {
                        // Reaching the API at all clears the startup grace: a
                        // failure after this is a failure, not a wait.
                        settled.store(true, std::sync::atomic::Ordering::Relaxed);
                        Duration::ZERO
                    }
                    Ok(false) => {
                        settled.store(true, std::sync::atomic::Ordering::Relaxed);
                        IDLE_PAUSE
                    }
                    Err(e) => {
                        // A runtime usually starts before the API is ready, so
                        // the first failures are a dependency arriving rather
                        // than anything wrong. Logging those at warn makes a
                        // healthy boot read as broken and teaches a reader to
                        // skip the line that would have mattered.
                        if settled.load(std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!(error = %e, "could not take work");
                        } else if !announced.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            tracing::info!(
                                api = %self.api_url,
                                error = %e,
                                "waiting for the API before taking work"
                            );
                        }
                        ERROR_PAUSE
                    }
                };

                if !pause.is_zero() {
                    tokio::select! {
                        _ = tokio::time::sleep(pause) => {}
                        _ = shutdown.notified() => return,
                    }
                }
            }
        });
    }

    fn token(&self) -> anyhow::Result<String> {
        Ok(self.runtime_key.clone())
    }

    /// Asks for one turn and runs it. Returns whether there was work.
    async fn take_one(
        self: Arc<Self>,
        permit: super::admission::Permit,
    ) -> anyhow::Result<bool> {
        let response = self
            .http
            .post(format!("{}/v1/work", self.api_url))
            .bearer_auth(self.token()?)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("asking for work returned {}", response.status());
        }

        let Some(assignment) = response.json::<Option<Assignment>>().await? else {
            return Ok(false);
        };

        // Spawned so the loop can go back to asking. The permit rides along,
        // so the slot is held for exactly as long as the turn runs.
        tokio::spawn(async move {
            let job_id = assignment.job_id;
            let lease = assignment.lease_token;
            if let Err(e) = self.run(assignment, permit).await {
                tracing::error!(job_id = %job_id, error = %e, "turn failed");
                // Say so, because the endpoint that would have reported this
                // turn is the one that failed. Silence here leaves the job
                // claimed and the session behind it blocked until a lease
                // lapses.
                self.hand_back(job_id, lease).await;
            }
        });

        Ok(true)
    }

    async fn run(
        &self,
        assignment: Assignment,
        permit: super::admission::Permit,
    ) -> anyhow::Result<()> {
        let job_id = assignment.job_id;
        let lease = assignment.lease_token;
        let request = assignment.request;

        let conversation: Vec<Message> = request
            .conversation
            .into_iter()
            .map(|m| Message {
                role: m.role,
                content: m.content,
                tool_calls: m
                    .tool_calls
                    .into_iter()
                    .map(|c| super::component::ToolCall {
                        id: c.id,
                        name: c.name,
                        arguments: c.arguments,
                    })
                    .collect(),
                tool_call_id: m.tool_call_id,
            })
            .collect();

        // Progress is reported by streaming it back to the tier that owns the
        // transcript. The channel is unbounded because the sinks are called
        // while the guest is blocked and cannot wait for a slow reader.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ExecuteEvent>();
        let sinks = super::router::sinks_for(&tx);

        let options = RunOptions {
            session_id: request.session_id,
            gateway_url: self.gateway_url.clone(),
            gateway_token: assignment.gateway_token,
            default_model: request.model.unwrap_or_else(|| self.default_model.clone()),
            progress: Some(sinks.0),
            on_tool: Some(sinks.1),
            on_tool_result: Some(sinks.2),
            on_usage: Some(sinks.3),
            fuel: super::router::FUEL_PER_TURN,
            timezone: request.timezone,
            reasoning_effort: request.reasoning_effort,
            temperature: request.temperature,
            traffic_type: request
                .traffic_type
                .unwrap_or_else(|| crate::gateway::routing::DEFAULT_TRAFFIC_TYPE.to_string()),
            max_tool_rounds: match request.max_tool_rounds {
                Some(n) if n <= 0 => 0,
                Some(n) => u32::try_from(n).unwrap_or(u32::MAX),
                None => super::router::DEFAULT_MAX_TOOL_ROUNDS,
            },
            reply_id: request.reply_id,
            storage: self.storage.clone(),
            tenant_id: request.tenant_id,
            idle_timeout: self.idle_timeout,
            egress: request.egress,
        };

        let runner = Arc::clone(&self.runner);
        let module = Arc::clone(&self.agent_module);
        let prompt = request.system_prompt;
        tokio::spawn(async move {
            let outcome = runner.run(&module, conversation, prompt, options).await;
            let _ = match outcome {
                Ok((content, cost)) => tx.send(ExecuteEvent::Done {
                    content,
                    prompt_tokens: cost.prompt_tokens,
                    completion_tokens: cost.completion_tokens,
                    cache_read_tokens: cost.cache_read_tokens,
                    cache_write_tokens: cost.cache_write_tokens,
                    reasoning_tokens: cost.reasoning_tokens,
                    provider: cost.provider,
                }),
                Err(e) => tx.send(ExecuteEvent::Failed {
                    message: e.to_string(),
                }),
            };
        });

        let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(|event| {
            serde_json::to_string(&event)
                .map(|mut line| {
                    line.push('\n');
                    axum::body::Bytes::from(line)
                })
                .map_err(std::io::Error::other)
        });

        let response = self
            .http
            .post(format!("{}/v1/work/{job_id}/events", self.api_url))
            .bearer_auth(self.token()?)
            .header(crate::api::work::LEASE_HEADER, lease.to_string())
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await?;

        // Held until the results are delivered, not merely until the guest
        // stops. A permit dropped when generation ends frees the slot while
        // the transcript is still being written, and the pod takes another
        // turn against memory the last one has not finished with -- which is
        // the whole thing this is meant to prevent.
        drop(permit);

        if !response.status().is_success() {
            anyhow::bail!("reporting a turn returned {}", response.status());
        }

        Ok(())
    }
}

impl Puller {
    /// Tells the API a turn could not be delivered.
    ///
    /// Best effort by nature: this runs because something already failed, and
    /// if the API cannot be reached to say so then the lease is what recovers
    /// the turn. Saying so when possible turns a forty-five second stall into
    /// an immediate retry.
    async fn hand_back(&self, job_id: Uuid, lease: Uuid) {
        let sent = self
            .http
            .post(format!("{}/v1/work/{job_id}/abandon", self.api_url))
            .header(crate::api::work::LEASE_HEADER, lease.to_string())
            .bearer_auth(match self.token() {
                Ok(token) => token,
                Err(e) => {
                    tracing::warn!(job_id = %job_id, error = %e, "could not mint a token to hand a turn back");
                    return;
                }
            })
            .send()
            .await;

        if let Err(e) = sent {
            tracing::warn!(
                job_id = %job_id,
                error = %e,
                "could not hand a turn back; its lease will recover it"
            );
        }
    }
}
