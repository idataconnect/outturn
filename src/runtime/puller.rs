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
    /// Absent from an API that predates refreshing, which then never happens.
    #[serde(default)]
    gateway_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Quoted back on every report, so the API can tell this pod's account
    /// of the turn from a later holder's.
    lease_token: Uuid,
    #[serde(flatten)]
    request: ExecuteRequest,
}

pub struct Puller {
    pub api_url: String,
    /// What this pod presents to the API. A shared key that means only "the
    /// runtime tier": this pod executes workspace components and so holds
    /// nothing that could mint a credential for anyone. See `RuntimeKey`.
    pub runtime_key: String,
    pub http: reqwest::Client,
    /// For reporting a turn's events, which is a long request body answered
    /// once at the end rather than a response streamed back. `http`'s read
    /// timeout would measure the length of the turn instead of the health of
    /// the connection -- see `http_client::reporting_client`.
    pub reporting: reqwest::Client,
    pub runner: Arc<AgentRunner>,
    pub agent_module: Arc<Vec<u8>>,
    pub storage: Option<Arc<dyn super::storage::StorageBackend>>,
    pub gateway_url: String,
    pub admission: Arc<Admission>,
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
    async fn take_one(self: Arc<Self>, permit: super::admission::Permit) -> anyhow::Result<bool> {
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
                parts: m
                    .parts
                    .into_iter()
                    .map(|p| match p {
                        super::router::ConversationPart::Text { text } => {
                            super::component::ContentPart::Text(text)
                        }
                        super::router::ConversationPart::Call { call } => {
                            super::component::ContentPart::Call(super::component::ToolCall {
                                id: call.id,
                                name: call.name,
                                arguments: call.arguments,
                            })
                        }
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
            gateway_token_expires_at: assignment.gateway_token_expires_at,
            refresh_token: Some({
                let api_url = self.api_url.clone();
                let runtime_key = self.runtime_key.clone();
                let http = self.http.clone();
                Arc::new(move |current: String| {
                    let api_url = api_url.clone();
                    let runtime_key = runtime_key.clone();
                    let http = http.clone();
                    Box::pin(async move {
                        refresh_token(&http, &api_url, &runtime_key, lease, job_id, current).await
                    })
                        as std::pin::Pin<
                            Box<
                                dyn std::future::Future<
                                        Output = Option<(String, chrono::DateTime<chrono::Utc>)>,
                                    > + Send,
                            >,
                        >
                })
            }),
            // Relayed, not read. The gateway checks it against the commitment in
            // the turn token, which this tier cannot write.
            gates: request.gates.clone(),
            // Resolved by the API, which knows the agent and the operator's
            // setting. None here is a turn nobody chose a model for, refused
            // below rather than served by one this pod made up.
            default_model: request.model.clone().unwrap_or_default(),
            progress: Some(sinks.progress),
            reasoning: Some(sinks.reasoning),
            writing: Some(sinks.writing),
            on_tool: Some(sinks.on_tool),
            on_tool_result: Some(sinks.on_tool_result),
            on_usage: Some(sinks.on_usage),
            on_write: Some(sinks.on_write),
            // Tells the API a request was refused for want of an approval, so
            // it can ask somebody and park the turn. The shape only: what act
            // that was, and whether it really was gated, are the API's to decide.
            on_gated: Some({
                let api_url = self.api_url.clone();
                let runtime_key = self.runtime_key.clone();
                let http = self.http.clone();
                let session_id = request.session_id;
                Arc::new(move |refused: crate::runtime::component::GatedRequest| {
                    let api_url = api_url.clone();
                    let runtime_key = runtime_key.clone();
                    let http = http.clone();
                    Box::pin(async move {
                        report_gated(
                            &http,
                            &api_url,
                            &runtime_key,
                            lease,
                            session_id,
                            job_id,
                            refused,
                        )
                        .await
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
                })
            }),
            on_absorbed: Some(sinks.on_absorbed),
            // Sleeping and timers are the API's to arrange -- a wakeup is a job,
            // a sleep a hold and a queue item -- so this only carries the ask
            // there, under the lease, as an approval request is carried.
            on_wait: Some({
                let api_url = self.api_url.clone();
                let runtime_key = self.runtime_key.clone();
                let http = self.http.clone();
                let session_id = request.session_id;
                Arc::new(move |wait: crate::runtime::component::WaitRequest| {
                    let api_url = api_url.clone();
                    let runtime_key = runtime_key.clone();
                    let http = http.clone();
                    Box::pin(async move {
                        report_wait(
                            &http,
                            &api_url,
                            &runtime_key,
                            lease,
                            session_id,
                            job_id,
                            wait,
                        )
                        .await
                    })
                        as std::pin::Pin<
                            Box<dyn std::future::Future<Output = Result<String, String>> + Send>,
                        >
                })
            }),
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
            // Nothing eager yet, so every tool is reached through the guest's
            // loader. The path is here for a policy to travel down -- the
            // agent's settings, alongside `model` -- once there is one worth
            // sending; what promotes a tool is a decision nobody has made.
            eager_tools: Vec::new(),
            reply_id: request.reply_id,
            storage: self.storage.clone(),
            workspace_id: request.workspace_id,
            agent_id: request.agent_id,
            write_scopes: if request.write_scopes.is_empty() {
                vec!["session".to_string()]
            } else {
                request.write_scopes
            },
            // A job queued before reads were gated has no read scopes at all,
            // and reading that as "nothing" would stop a turn that was
            // accepted on the understanding it could read. Empty means what it
            // has always meant in practice: everything this agent can write,
            // plus the reads that were never restricted.
            read_scopes: if request.read_scopes.is_empty() {
                vec![
                    "session".to_string(),
                    "agent".to_string(),
                    "workspace".to_string(),
                ]
            } else {
                request.read_scopes
            },
            skill_files: request.skill_files,
            idle_timeout: self.idle_timeout,
            admission: Some(Arc::clone(&self.admission)),
            egress: request.egress,
        };

        let runner = Arc::clone(&self.runner);
        let module = Arc::clone(&self.agent_module);
        let prompt = request.system_prompt;
        let unnamed = request.model.is_none();
        tokio::spawn(async move {
            if unnamed {
                let _ = tx.send(ExecuteEvent::Failed {
                    message: "no model: the turn names none".into(),
                    held: None,
                    terminal: false,
                });
                return;
            }
            let outcome = runner.run(&module, conversation, prompt, options).await;
            let _ = match outcome {
                Ok(done) => tx.send(ExecuteEvent::Done {
                    content: done.reply,
                    prompt_tokens: done.cost.prompt_tokens,
                    completion_tokens: done.cost.completion_tokens,
                    cache_read_tokens: done.cost.cache_read_tokens,
                    cache_write_tokens: done.cost.cache_write_tokens,
                    reasoning_tokens: done.cost.reasoning_tokens,
                    provider: done.cost.provider,
                    held: done.held,
                    awaiting_approval: done.awaiting_approval,
                }),
                Err(e) if e.out_of_fuel() => tx.send(ExecuteEvent::Failed {
                    message: super::component::OUT_OF_FUEL.into(),
                    held: e.held,
                    terminal: true,
                }),
                Err(e) => tx.send(ExecuteEvent::Failed {
                    message: e.to_string(),
                    held: e.held,
                    terminal: false,
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
            .reporting
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

/// Tells the API a request was refused for want of an approval.
///
/// Returns whether the turn should expect to park. False on any failure: the
/// request has already been refused, so the worst case is an agent told less
/// than it could have been rather than one that proceeds when it should not.
/// Trades a running turn's gateway token for a fresh one.
///
/// Presents the lease as every other report does, and the current token so the
/// API can copy its commitments into the new one rather than recompute them.
/// `None` on any failure: the caller keeps the token it has.
async fn refresh_token(
    http: &reqwest::Client,
    api_url: &str,
    runtime_key: &str,
    lease: Uuid,
    job_id: Uuid,
    current: String,
) -> Option<(String, chrono::DateTime<chrono::Utc>)> {
    #[derive(serde::Deserialize)]
    struct Refreshed {
        gateway_token: String,
        gateway_token_expires_at: chrono::DateTime<chrono::Utc>,
    }

    let response = http
        .post(format!(
            "{}/v1/work/{job_id}/token",
            api_url.trim_end_matches('/')
        ))
        .bearer_auth(runtime_key)
        .header(crate::api::work::LEASE_HEADER, lease.to_string())
        .json(&serde_json::json!({ "gateway_token": current }))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), %job_id, "the API would not refresh a turn token");
        return None;
    }
    let refreshed: Refreshed = response.json().await.ok()?;
    Some((refreshed.gateway_token, refreshed.gateway_token_expires_at))
}

/// Carries a sleep or a timer to the API, and brings back what to tell the model.
///
/// A refusal comes back as the API's own words, which are written for the
/// model -- "at most thirty days", "that time has passed" -- so it can correct
/// the request rather than guess why it failed.
async fn report_wait(
    http: &reqwest::Client,
    api_url: &str,
    runtime_key: &str,
    lease: Uuid,
    session_id: Uuid,
    job_id: Uuid,
    wait: crate::runtime::component::WaitRequest,
) -> Result<String, String> {
    use crate::runtime::component::WaitRequest;
    let body = match wait {
        WaitRequest::Sleep { seconds, reason } => serde_json::json!({
            "job_id": job_id, "session_id": session_id,
            "kind": "sleep", "seconds": seconds, "reason": reason,
        }),
        WaitRequest::Timer {
            at,
            seconds,
            reason,
        } => serde_json::json!({
            "job_id": job_id, "session_id": session_id,
            "kind": "timer", "at": at, "seconds": seconds, "reason": reason,
        }),
        WaitRequest::ListTimers => serde_json::json!({
            "job_id": job_id, "session_id": session_id, "kind": "list",
        }),
        WaitRequest::CancelTimer { id } => serde_json::json!({
            "job_id": job_id, "session_id": session_id, "kind": "cancel", "id": id,
        }),
    };
    let response = http
        .post(format!("{}/v1/work/wait", api_url.trim_end_matches('/')))
        .bearer_auth(runtime_key)
        .header(crate::api::work::LEASE_HEADER, lease.to_string())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("could not reach the platform to arrange that: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        let why = response.text().await.unwrap_or_default();
        return Err(if why.is_empty() {
            status.to_string()
        } else {
            why
        });
    }
    #[derive(serde::Deserialize)]
    struct Arranged {
        message: String,
    }
    response
        .json::<Arranged>()
        .await
        .map(|a| a.message)
        .map_err(|e| format!("the platform's answer could not be read: {e}"))
}

async fn report_gated(
    http: &reqwest::Client,
    api_url: &str,
    runtime_key: &str,
    lease: Uuid,
    session_id: Uuid,
    job_id: Uuid,
    refused: crate::runtime::component::GatedRequest,
) -> bool {
    let response = http
        .post(format!("{}/v1/work/gated", api_url.trim_end_matches('/')))
        .bearer_auth(runtime_key)
        // As every other report carries it. The shared key says only "the
        // runtime tier"; the lease is what says this pod is the one running this
        // turn, without which the API would take a runtime's word for which
        // conversation it was speaking about.
        .header(crate::api::work::LEASE_HEADER, lease.to_string())
        .json(&serde_json::json!({
            "session_id": session_id,
            "job_id": job_id,
            "method": refused.method,
            "host": refused.host,
            "path": refused.path,
            "body": refused.body,
        }))
        .send()
        .await;

    match response {
        Ok(response) if response.status().is_success() => response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("park").and_then(|p| p.as_bool()))
            .unwrap_or(false),
        Ok(response) => {
            // Refused by the API, which is what a relay for an ungated request
            // gets. Worth seeing: the honest causes are a race with a skill
            // being unbound, and a runtime that should not be trusted.
            tracing::warn!(
                job_id = %job_id,
                status = %response.status(),
                "the API would not raise an approval for a refused request"
            );
            false
        }
        Err(e) => {
            tracing::warn!(job_id = %job_id, error = %e, "could not report a gated refusal");
            false
        }
    }
}
