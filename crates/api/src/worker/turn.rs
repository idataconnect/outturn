use std::time::Duration;

use sqlx::postgres::PgPool;
use uuid::Uuid;

use outturn_core::events;
use outturn_core::jobs;
use outturn_core::protocol::work::ExecuteEvent;
use outturn_core::storage::stored;

use crate::actions::ActionStore as _;
use crate::chat::Usage;

use super::projection::{
    answered, keep_signatures, marked, projected_with_sources, repeated, signed_replies, up_to,
    without_gate_mark,
};
use super::{ChatTurnPayload, Prepared, Worker};

/// Where a call's token counts came from, judged by what the provider sent
/// and what the runtime said about the call.
///
/// A provider that returned a usage object is the authority on what its own
/// call cost -- for the whole answer, or for the part of it a cut stream
/// delivered. Numbers the runtime worked out itself, because the stream was
/// cut before the provider said anything, are an estimate and are recorded
/// as one. A call with neither leaves zeros behind, and those zeros must not
/// be recorded as though somebody had measured them: a turn that cost
/// nothing and a turn nobody counted look identical afterwards, and only this
/// tells them apart. Nothing can recover it later, so it is decided here,
/// once, on the only evidence there is.
pub(super) fn source_of(
    provider_usage: &Option<serde_json::Value>,
    partial: bool,
    estimated: bool,
) -> crate::usage::UsageSource {
    use crate::usage::UsageSource;
    match provider_usage {
        _ if estimated => UsageSource::Estimated,
        Some(usage) if !usage.is_null() && partial => UsageSource::ReportedPartial,
        Some(usage) if !usage.is_null() => UsageSource::Reported,
        _ => UsageSource::Unknown,
    }
}

/// What a completed turn produced.
pub(crate) struct TurnOutcome {
    /// Whether this turn stopped because somebody is being asked to approve
    /// something, and so should be kept rather than finished.
    ///
    /// The reply it wrote stays either way: what it managed to say before it
    /// stopped is part of the conversation, and `replies_to` means the resumed
    /// turn takes that reply back rather than orphaning it.
    pub(crate) awaiting_approval: bool,
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
    parts: Vec<crate::chat::parts::Part>,
    /// Summed across every round of the turn, counted by the runtime host.
    usage: Usage,
    /// The endpoint that served it, for attributing spend.
    provider: Option<String>,
    /// The model that served it, as the gateway reported it. Not the one the
    /// agent asked for: a route may answer with another, and a provider may
    /// quietly serve a newer version under the old name (docs/models.md).
    model: Option<String>,
    /// What the turn's replies that called a tool were signed with, kept on
    /// the reply for the turns after it.
    signed: Vec<outturn_core::protocol::work::SignedReply>,
}

/// A runtime reported on a turn it no longer holds.
///
/// Its own type so the report endpoint can answer it as a refusal rather than a
/// fault: the lease lapsed, the turn was handed on, and this pod's account of it
/// is simply no longer wanted. Folded into every other error it came back as a
/// 500, which reads as the API being broken.
#[derive(Debug, thiserror::Error)]
#[error("the lease on this turn lapsed while it was being reported")]
pub struct LeaseLost;

/// Renews a claim's lease for as long as it is held.
///
/// A turn runs for as long as a model takes and the lease is deliberately
/// short, so without this the reaper hands the same turn to a second pod
/// while the first is still on it -- and both would then write the same
/// reply. Held here rather than in the runtime because a lease is a claim on
/// a row, and the runtime holds no database.
///
/// A guard rather than a task handle, because the renewal must stop on every
/// way out of the work it covers. Aborted by hand at the one place the happy
/// path ended, it kept running past the `?` returns before it -- renewing a
/// lease on a turn this pod had already given up on.
///
/// The token is quoted back on every renewal, so a heartbeat outliving its
/// claim cannot extend whichever claim replaced it.
pub(crate) struct LeaseHeartbeat(tokio::task::JoinHandle<()>);

impl LeaseHeartbeat {
    pub(crate) fn start(pool: PgPool, job_id: Uuid, token: Uuid) -> Self {
        Self(tokio::spawn(async move {
            // Renewed immediately rather than after a first interval. The
            // lease started at the claim, and whatever ran between then and
            // here has already burned some of it -- so the clock this
            // heartbeat is racing began before the heartbeat did.
            let mut ticker = tokio::time::interval(jobs::LEASE_HEARTBEAT);
            loop {
                ticker.tick().await;
                match jobs::extend_lease(&pool, job_id, jobs::DEFAULT_LEASE, token).await {
                    // No longer ours: something reaped it, and renewing
                    // would take it back from whoever has it now.
                    Ok(false) => return,
                    Ok(true) => {}
                    Err(e) => {
                        tracing::warn!(job_id = %job_id, error = %e, "lease renewal failed")
                    }
                }
            }
        }))
    }
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A turn that failed in a way another attempt would repeat.
///
/// Its own type so the failure path can tell it from the ordinary kind, which
/// is retried. The message is the runtime's, meant for the transcript.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct FailedForGood(String);

/// A turn the model's provider declined to answer. Final for the same reason
/// as `FailedForGood`, and its own type because it is not a failure: the
/// reader is shown that a safety check tripped, not that something broke.
#[derive(Debug, thiserror::Error)]
#[error("{}", .0.reason)]
struct Declined(outturn_core::protocol::work::Refusal);

impl Worker {
    /// The policy a turn of this agent runs under: its template version's for
    /// one made from a template, else its own. Null, said in the log, where it
    /// cannot be read -- the ledger row is still worth writing.
    async fn policy_of(&self, workspace_id: Uuid, agent_id: Uuid) -> serde_json::Value {
        match self.templates.for_agent(workspace_id, agent_id).await {
            Ok(Some(made)) => return made.running.policy,
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(%agent_id, error = %e, "could not read a template agent's policy");
                return serde_json::Value::Null;
            }
        }
        match self.agents.get(workspace_id, agent_id).await {
            Ok(agent) => agent.policy,
            Err(e) => {
                tracing::warn!(%agent_id, error = %e, "could not read an agent's policy");
                serde_json::Value::Null
            }
        }
    }

    /// The agent as a turn sees it, and the tools offered from its first round.
    ///
    /// An agent made from a template runs the template's newest version, or the
    /// one its workspace pinned where the template allows it: its
    /// prompt, composed with the workspace's own section where the template
    /// allows one, and its policy. Read each turn rather than copied into the
    /// agent, so a publish reaches every workspace at its next turn. An agent
    /// made by hand is as it was written, and defers every tool.
    pub(super) async fn agent_for_turn(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> anyhow::Result<(crate::agent::Agent, Vec<String>)> {
        let mut agent = self
            .agents
            .get(workspace_id, agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if agent.template_id.is_none() {
            return Ok((agent, Vec::new()));
        }
        let Some(made) = self
            .templates
            .for_agent(workspace_id, agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("template: {e}"))?
        else {
            return Ok((agent, Vec::new()));
        };
        let addition = made
            .template
            .allow_additions
            .then_some(agent.workspace_addition.as_str());
        agent.system_prompt = crate::agent_template::compose_prompt(&made.running, addition);
        agent.policy = made.running.policy.clone();
        Ok((agent, made.running.eager_tools))
    }

    /// Gives up on a turn: clears the reply nothing will fill, and says so.
    ///
    /// An empty reply left behind wedges the session against further messages,
    /// and a reader with no error event waits on an indicator that resolves on
    /// no timescale at all. Both halves matter, which is why they are one
    /// function rather than two blocks that drifted apart.
    pub(crate) async fn abandon_payload(&self, payload: &ChatTurnPayload, reason: &str) {
        self.abandon_payload_with(payload, reason, None).await
    }

    /// Kept on the prompt the refusal answered, so a reader who opens the
    /// conversation later is shown it the same way as one who watched it
    /// happen. Not fatal: the live event still says it.
    async fn record_refusal(
        &self,
        payload: &ChatTurnPayload,
        refusal: &outturn_core::protocol::work::Refusal,
    ) {
        if let Err(e) = sqlx::query(
            "update agent_messages set metadata = metadata || jsonb_build_object('refused', $2::jsonb) \
             where id = $1",
        )
        .bind(payload.message_id)
        .bind(serde_json::json!(refusal))
        .execute(&self.pool)
        .await
        {
            tracing::warn!(error = %e, "could not record the refusal on its prompt");
        }
    }

    async fn abandon_payload_with(
        &self,
        payload: &ChatTurnPayload,
        reason: &str,
        refusal: Option<&outturn_core::protocol::work::Refusal>,
    ) {
        // The attempt being abandoned is the latest one: earlier attempts are
        // finished, and one of them may hold the refusal somebody approved
        // against. `current_attempt` is the one this turn was writing.
        let attempt = self
            .chat
            .current_attempt(payload.message_id)
            .await
            .unwrap_or(1);
        if let Err(e) = self
            .chat
            .discard_placeholder(payload.message_id, attempt)
            .await
        {
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
            match refusal {
                Some(refusal) => serde_json::json!({
                    "message": reason,
                    "message_id": payload.message_id,
                    "refusal": refusal,
                }),
                None => serde_json::json!({ "message": reason, "message_id": payload.message_id }),
            },
        )
        .await;
    }

    /// Says that a turn was held, to whoever has the conversation open.
    ///
    /// `chat.held` rather than `chat.error`, because a failure and a stop are
    /// not the same thing and this codebase does not let them look alike (see
    /// docs/inhibitors.md). A failure is retried and a stop is not; a reader
    /// shown an error for a deliberate hold is told the system broke when
    /// somebody decided it should wait. The browser's error path also discards
    /// the placeholder bubble and files the turn under failures, which is the
    /// wrong account of a conversation that is merely paused.
    ///
    /// Separate from `abandon_payload`, which also discards a placeholder: a
    /// turn refused by a hold is refused before one is claimed, so there is
    /// nothing to discard and the only thing owed is the telling. Best effort,
    /// because a refusal that could not be announced is still a refusal and
    /// failing the job over it would retry work that is meant not to run.
    async fn announce_hold(&self, payload: &ChatTurnPayload, why: &str, resumable: bool) {
        // The approval waiting on somebody, where one is. Carried on the event so
        // a reader watching this conversation can answer it without going to find
        // a queue -- the person who triggered a hold is usually the person who can
        // lift it, and sending them elsewhere to do it loses the thread.
        //
        // Absent when the hold is anything else: a spend cap and an operator's
        // stop are held the same way and neither is answerable here.
        let pending = crate::actions::PostgresActionStore::new(self.pool.clone())
            .approval_on_session(payload.workspace_id, payload.session_id)
            .await
            .ok()
            .flatten()
            .map(|item| crate::gated::answerable(&item));
        // Or the sleep, which a person can end from the same place.
        let asleep = crate::wake::asleep(&self.pool, payload.workspace_id, payload.session_id)
            .await
            .ok()
            .flatten();

        self.announce(
            payload,
            "chat.held",
            serde_json::json!({
                "message": why,
                "message_id": payload.message_id,
                // Whether anything the reader does will start it again. A stop
                // latches and waits for a person; a suspension lifts when the
                // hold does.
                "resumable": resumable,
                "approval": pending,
                "asleep": asleep,
            }),
        )
        .await;
    }

    /// One way for this tier to tell a conversation's readers something.
    ///
    /// Best effort by design: an event that could not be written has not
    /// broken the turn it was about, and failing the job to retry the telling
    /// would re-run work that already happened.
    async fn announce(&self, payload: &ChatTurnPayload, kind: &str, body: serde_json::Value) {
        let _ = events::append(
            &self.pool,
            payload.workspace_id,
            Some(payload.session_id),
            kind,
            body,
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
    pub(crate) async fn consume_turn(
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
        // The same builder `replay` assembles with, so what streams and what a
        // reload rebuilds are the same message.
        let mut parts = crate::chat::parts::Builder::new();
        // The session's account label, for the ledger. Read once, on the
        // first call that needs it, so a turn that makes no model call reads
        // nothing.
        let mut account: Option<Option<String>> = None;
        // The traffic type the ledger records each call under, read once for
        // the same reason. From the agent's policy only -- its template's,
        // for one made from a template -- rather than the whole turn's view.
        let mut traffic: Option<String> = None;

        // Chunks are cut wherever the network cut them, which can be inside
        // a character; see `outturn_core::utf8`.
        let mut decoder = outturn_core::utf8::Decoder::new();
        while let Some(bytes) = stream.next().await {
            let bytes = bytes.map_err(|e| anyhow::anyhow!("{e}"))?;
            decoder.push(&bytes, &mut buffer)?;

            // An event may span reads, so only whole lines are parsed.
            while let Some(index) = buffer.find('\n') {
                let line = buffer[..index].trim().to_string();
                buffer.drain(..=index);
                if line.is_empty() {
                    continue;
                }

                match serde_json::from_str::<ExecuteEvent>(&line) {
                    Ok(ExecuteEvent::Absorbed { ids }) => {
                        // Where the reply changed course. The stored text is
                        // untouched -- it is still what streamed -- but the
                        // reader is shown the message here, splitting the
                        // reply around it, rather than below a reply that
                        // went on to answer it.
                        for id in ids {
                            parts.steer(&id.to_string());
                            events::append(
                                &self.pool,
                                payload.workspace_id,
                                Some(payload.session_id),
                                "chat.steer",
                                serde_json::json!({
                                    "message_id": message_id,
                                    "id": id,
                                }),
                            )
                            .await?;
                        }
                    }
                    Ok(ExecuteEvent::Reasoning { text }) => {
                        // A part, where it happened. A model that thinks, calls
                        // a tool, reads the answer and thinks again produced two
                        // separate thoughts about two different things, and one
                        // block accumulating both says it deliberated once --
                        // about a result it had not yet seen when it started.
                        //
                        // Coalesced with the part before it only when that is
                        // also thinking, exactly as a text delta coalesces:
                        // within one stretch of thinking the fragments are one
                        // thought arriving a token at a time.
                        // Placed where it happened, with the clock restarting on
                        // each thought: time spent waiting on a tool is not time
                        // the model spent thinking.
                        parts.reasoning(&text, Some(chrono::Utc::now()));
                        events::append(
                            &self.pool,
                            payload.workspace_id,
                            Some(payload.session_id),
                            "chat.reasoning",
                            serde_json::json!({
                                "message_id": message_id,
                                "text": text,
                            }),
                        )
                        .await?;
                    }
                    Ok(ExecuteEvent::Writing { index, name, bytes }) => {
                        // Told to the reader and nothing else. Not a part: the
                        // call it announces is recorded when the guest starts
                        // it, and a transcript holding both would show every
                        // call twice. Not stored either, for the same reason --
                        // a reload mid-round simply shows the call when it
                        // starts, as it always did.
                        events::append(
                            &self.pool,
                            payload.workspace_id,
                            Some(payload.session_id),
                            "chat.writing",
                            serde_json::json!({
                                "message_id": message_id,
                                "index": index,
                                "name": name,
                                "bytes": bytes,
                            }),
                        )
                        .await?;
                    }
                    Ok(ExecuteEvent::Delta { idx, text }) => {
                        parts.text(&text);
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
                        parts.call(&id);
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
                        // A call held for approval is not a failure, and the
                        // browser draws it as waiting rather than as an error.
                        // Said as a field rather than left for the browser to
                        // find in the text: the mark is bookkeeping between
                        // tiers, and the sentence telling the model not to
                        // retry is for the model. Both come out of what a
                        // person reads; `content`, which the model is handed
                        // and the retraction later matches on, keeps them.
                        //
                        // Only an error ending in the gate's own suffix: a
                        // fetched page can carry the mark or the sentence, and
                        // matching either anywhere let it pose as a held call
                        // and had a quoted copy of the sentence cut out.
                        let held = is_error.then(|| without_gate_mark(&details)).flatten();
                        let gated = held.is_some();
                        let details = held.unwrap_or(details);
                        // Attached to the call it answers rather than sent as
                        // its own thing, so the browser has one object per
                        // tool: what it was doing, and what came back.
                        if let Some(call) = tools
                            .iter_mut()
                            .find(|c| c["id"].as_str() == Some(id.as_str()))
                        {
                            call["details"] = serde_json::json!(details);
                            if gated {
                                call["gated"] = serde_json::json!(true);
                            }
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
                                "gated": gated,
                            }),
                        )
                        .await?;
                    }
                    Ok(ExecuteEvent::Wrote { path, key, bytes }) => {
                        // Same call the upload handler makes. A document is a
                        // document whether a person dragged it in or an agent
                        // unpacked it from an archive, and only this tier can
                        // queue the job that reads it. The key is the
                        // runtime's word for where it wrote; a runtime that
                        // lied would be asking this tier to read outside the
                        // workspace, so the word is checked.
                        if !crate::extract::belongs_to(&key, payload.workspace_id) {
                            tracing::warn!(key = %key, workspace_id = %payload.workspace_id,
                                "runtime reported a write outside the workspace; ignoring");
                        } else {
                            crate::extract::enqueue(&self.pool, payload.workspace_id, &key, &path)
                                .await;
                            // Counted against the workspace's storage, with
                            // the text the runtime dropped beside it uncounted.
                            if let Some(bytes) = bytes {
                                let counted = async {
                                    stored::forget(&self.pool, &key).await?;
                                    stored::record(&self.pool, &key, bytes).await
                                };
                                if let Err(e) = counted.await {
                                    tracing::warn!(key = %key, error = %e, "could not count a write");
                                }
                            }
                        }
                    }
                    Ok(ExecuteEvent::Deleted { key }) => {
                        // Checked as a write is: uncounting another
                        // workspace's file would hand it storage it is using.
                        if crate::extract::belongs_to(&key, payload.workspace_id)
                            && let Err(e) = stored::forget(&self.pool, &key).await
                        {
                            tracing::warn!(key = %key, error = %e, "could not uncount a delete");
                        }
                    }
                    Ok(ExecuteEvent::Usage {
                        round,
                        traffic_type: call_traffic,
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
                        partial,
                        estimated,
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
                            .record(crate::usage::RecordUsage {
                                workspace_id: payload.workspace_id,
                                agent_id: Some(payload.agent_id),
                                session_id: Some(payload.session_id),
                                user_id: payload.user_id,
                                account,
                                reply_id: Some(message_id),
                                job_id: Some(job_id),
                                round: round as i32,
                                // As the runtime said where it did: a look
                                // at an image is `vision` whatever the turn
                                // is, and the bill should say so. A runtime
                                // that did not say made a round, which is
                                // the turn's own class.
                                traffic_type: match (call_traffic, &traffic) {
                                    (Some(t), _) => t,
                                    (None, Some(t)) => t.clone(),
                                    (None, None) => {
                                        let t = traffic_type_for(
                                            &self
                                                .policy_of(payload.workspace_id, payload.agent_id)
                                                .await,
                                            payload.user_id.is_none(),
                                        );
                                        traffic = Some(t.clone());
                                        t
                                    }
                                },
                                endpoint,
                                model,
                                credential_owner: paid_by,
                                fallback: "none".to_string(),
                                prompt_tokens: prompt_tokens as i32,
                                completion_tokens: completion_tokens as i32,
                                cache_read_tokens: cache_read_tokens as i32,
                                cache_write_tokens: cache_write_tokens as i32,
                                reasoning_tokens: reasoning_tokens as i32,
                                usage_source: source_of(&provider_usage, partial, estimated),
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
                        model,
                        held,
                        awaiting_approval,
                        signed,
                    }) => {
                        // Latched here, where generation actually stopped,
                        // rather than a turn later when the next one is
                        // refused. Without this a hold taken and released
                        // while a turn streamed would leave the session
                        // unlatched -- and the next turn, finding nothing
                        // holding it, would simply run. A stop is supposed to
                        // need a person to lift it.
                        // A turn waiting on an approval is not latched. The
                        // latch exists so a stop needs a person to lift it; an
                        // approval *is* lifted by a person, and latching would
                        // leave the conversation stopped after the yes -- with
                        // the hold released, the turn resumed, and the session
                        // refusing to run it.
                        if let Some(reason) = &held
                            && !awaiting_approval
                        {
                            // Fails the turn rather than logging and carrying
                            // on, which is what the latch at preparation does
                            // and for the same reason: a stop that did not
                            // record itself is a session that answers the next
                            // message as though nothing happened. The reply is
                            // already written and the transcript keeps it -- so
                            // what a retry costs is a repeated latch, which is
                            // a no-op, against the alternative of a kill switch
                            // that silently did not take.
                            self.chat
                                .stop_session(payload.session_id, reason)
                                .await
                                .map_err(|e| anyhow::anyhow!("latching a cut turn: {e}"))?;
                            tracing::info!(
                                session_id = %payload.session_id,
                                reason = %reason,
                                "a hold cut this turn; the session is stopped until somebody restarts it"
                            );
                        }

                        return Ok(TurnOutcome {
                            awaiting_approval,
                            content,
                            tools,
                            parts: parts.into_parts(),
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
                            model,
                            signed,
                        });
                    }
                    Ok(ExecuteEvent::Failed {
                        message,
                        held,
                        terminal,
                        refusal,
                    }) => {
                        // A turn a hold cut and that then failed is a stop
                        // first and a failure second. Latched before the bail,
                        // because the bail is what retries it -- and a retry
                        // that finds the hold released would run the work the
                        // hold existed to prevent.
                        if let Some(reason) = &held {
                            self.chat
                                .stop_session(payload.session_id, reason)
                                .await
                                .map_err(|e| anyhow::anyhow!("latching a cut turn: {e}"))?;
                            tracing::info!(
                                session_id = %payload.session_id,
                                reason = %reason,
                                "a hold cut this turn before it failed; the session is stopped"
                            );
                        }
                        if let Some(refusal) = refusal {
                            return Err(Declined(refusal).into());
                        }
                        if terminal {
                            return Err(FailedForGood(message).into());
                        }
                        anyhow::bail!("{message}")
                    }
                    Err(e) => tracing::warn!(error = %e, "malformed event from runtime"),
                }
            }
        }

        anyhow::bail!("runtime stream ended without a result")
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
    ) -> anyhow::Result<Result<Option<crate::chat::Stopped>, anyhow::Result<Prepared>>> {
        use crate::inhibitor::Verdict;

        // A stopped session stays stopped until a person says something. Not
        // until the hold is released: releasing a kill switch must not resume
        // fifty conversations that were killed while it was on.
        //
        // Read rather than cleared. Whether the latch lifts depends on the
        // verdict below, and clearing it first loses what it was holding:
        // `stop_session` keeps the first stop's reason and time by refusing to
        // write where `stopped_at` is already set, so a latch cleared here and
        // re-taken below comes back stamped `now()` with the current reason.
        // A person nudging a conversation held for three weeks would restart
        // the clock every time, and `Stopped.at` exists precisely to say how
        // long it has really been.
        // Kept, not just counted: if this turn is refused too, what the reader
        // is told has to be what the latch actually says. The current hold may
        // be a different one that arrived later, and announcing its reason
        // while the transcript, the latch and the next turn's marker all
        // narrate the first is three accounts of one pause.
        let latched = self
            .chat
            .stopped_reason(payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("stopped: {e}"))?;
        let was_stopped = latched.is_some();

        if was_stopped {
            // `user_id` is null for anything the platform produced, so the
            // agent cannot clear its own latch and neither can a steer it
            // provoked.
            let by_a_person = payload.user_id.is_some();
            if !by_a_person {
                tracing::info!(
                    session_id = %payload.session_id,
                    "a stopped session declined work that no person asked for"
                );
                return Ok(Err(Ok(Prepared::Nothing)));
            }
        }

        let holds = self
            .inhibitors
            .covering(payload.workspace_id, payload.agent_id, payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("inhibitors: {e}"))?;
        let decision = crate::inhibitor::decide(holds);

        match decision.verdict {
            Verdict::Proceed => {
                // The hold is off and a person asked, so the latch lifts here
                // and nowhere else. What it was holding becomes what this turn
                // is picking up from.
                let restarting_from = if was_stopped {
                    let cleared = self
                        .chat
                        .clear_stop(payload.session_id)
                        .await
                        .map_err(|e| anyhow::anyhow!("clear stop: {e}"))?;
                    tracing::info!(
                        session_id = %payload.session_id,
                        "a person restarted a stopped session"
                    );
                    cleared
                } else {
                    None
                };
                Ok(Ok(restarting_from))
            }
            Verdict::Stopped => {
                // Said in the transcript as well as latched: the next turn
                // reads this history, and a reply that simply stops is one the
                // model apologizes for or tries to finish.
                //
                // A no-op when the session was already stopped, which is what
                // keeps the original reason and time.
                let why = decision.why();

                // Unless this conversation is *also* waiting on somebody. A
                // stop that lands over a pending approval is the workspace's
                // state, not this conversation's: it was not stopped, it was
                // already waiting, and the stop refuses the turn on its own
                // without needing a latch left behind.
                //
                // Latching anyway is a trap, because a latch outlives the hold
                // that set it. The approver answers, the approval's hold goes,
                // the turn is given back -- and refuses again on a latch nobody
                // can see, with the approval already gone from the queue. They
                // did the thing they were asked to do and the conversation
                // stayed dead.
                //
                // Only a suspended hold counts. Two stops are still a stop.
                //
                // Through `should_latch` rather than inline: the branch here is
                // unreachable from a test without a live worker, and a copy of
                // the rule inverted right here left every test in the crate
                // passing.
                let latching = crate::inhibitor::should_latch(&decision);
                let also_waiting = !latching;

                if latching {
                    self.chat
                        .stop_session(payload.session_id, &why)
                        .await
                        .map_err(|e| anyhow::anyhow!("stop session: {e}"))?;
                }
                // Told to whoever is watching. This refusal happens before a
                // placeholder exists, so without an event the message sits in
                // the transcript with no reply and no indication -- and the
                // thread view cannot read `/v1/inhibitors` to work out why on
                // its own.
                //
                // Only when a person is waiting. A platform-produced turn --
                // no `user_id` -- is nobody's pending question, and an error
                // banner for a turn the reader never asked for explains
                // nothing they can act on.
                if payload.user_id.is_some() {
                    // `resumable` follows the latch, not the verdict. A turn
                    // that was not latched carries on by itself once the stop
                    // lifts, and telling its reader to send a message would have
                    // them typing into a conversation that is already coming
                    // back -- and clearing a latch that is not there.
                    self.announce_hold(payload, latched.as_deref().unwrap_or(&why), also_waiting)
                        .await;
                }
                tracing::info!(
                    session_id = %payload.session_id,
                    workspace_id = %payload.workspace_id,
                    reason = %why,
                    also_waiting,
                    "a turn was stopped before it ran"
                );
                // Kept rather than finished when it is also waiting on
                // somebody. `Nothing` completes the job, which for a stop is
                // right -- the turn was declined and nothing will bring it
                // back -- and for this is the wedge the latch used to cause,
                // by a worse route: the approver answers, `resume_for_scope`
                // finds no parked row, and the grant is minted against a job
                // that will never run again.
                //
                // Parking is what `resumable: true` above already promises.
                Ok(Err(Ok(if also_waiting {
                    Prepared::Park
                } else {
                    Prepared::Nothing
                })))
            }
            // A suspension pauses rather than declines: the turn is kept and
            // given back to the queue when the hold lifts. It takes no latch,
            // because there is no fragment to explain and nothing for a person
            // to restart -- releasing the hold is what restarts it.
            Verdict::Suspended => {
                let why = decision.why();
                tracing::info!(
                    session_id = %payload.session_id,
                    reason = %why,
                    "a turn was parked waiting on a hold"
                );
                // Resumable: releasing the hold gives this turn back to the
                // queue, rather than waiting for somebody to say something.
                if payload.user_id.is_some() {
                    self.announce_hold(payload, &why, true).await;
                }
                Ok(Err(Ok(Prepared::Park)))
            }
        }
    }

    /// Everything a turn needs before it can run.
    ///
    /// All of it touches the database -- the agent, the transcript, the egress
    /// rules, the reply the turn will stream into -- so it happens on this
    /// tier whichever way the turn is going to reach a runtime.
    ///
    /// Three outcomes rather than two. `Nothing` is a turn with nothing left to
    /// do, which is not a failure: a steered message was answered inside the
    /// turn it interrupted, and answering it again would produce a second reply
    /// to a question already addressed. `Park` is a turn a suspended hold
    /// refused, which has to be kept rather than completed -- completing it is
    /// what made `resumable` a promise nothing could keep.
    pub(crate) async fn prepare_turn(
        &self,
        job_id: Uuid,
        payload: &ChatTurnPayload,
    ) -> anyhow::Result<Prepared> {
        let (agent, eager_tools) = self
            .agent_for_turn(payload.workspace_id, payload.agent_id)
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
            return Ok(Prepared::Nothing);
        }

        // Before anything is read or written for this turn. A hold that
        // arrives while a turn is being prepared is one the next turn catches;
        // a hold checked after the work is done has already paid for it.
        let restarting_from = match self.inhibited(payload).await? {
            Ok(reason) => reason,
            Err(refusal) => return refusal,
        };

        // From the newest summary on, which is all the projection keeps. Should
        // the prompt somehow sort below what that summary covers it would be
        // missing from the tail, and the whole history is read instead: a
        // slower turn rather than one answering nothing.
        let mut history = self
            .chat
            .turn_history(payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("history: {e}"))?;
        if !history.messages.iter().any(|m| m.id == payload.message_id) {
            history = self
                .chat
                .messages(payload.session_id)
                .await
                .map_err(|e| anyhow::anyhow!("history: {e}"))?;
        }
        let history = up_to(history.messages, payload.message_id);

        let egress = crate::egress::rules_for(&self.pool, payload.workspace_id)
            .await
            .map_err(|e| anyhow::anyhow!("egress rules: {e}"))?;
        // Committed here, next to the rules it is over, so the token minted
        // for this turn can carry a hash rather than the runtime's word for
        // what it was given. The runtime executes workspace code and cannot be
        // the tier that reports its own rules.
        let egress_commitment = outturn_core::egress::commit::root(payload.workspace_id, &egress);

        // A turn somebody answered an approval for writes a new attempt rather
        // than taking back the reply that was refused, and so does one retried
        // after its last attempt showed the reader something. A crashed retry
        // that showed nothing takes its own attempt back, which is what makes
        // that idempotent. The one place this is decided: see `attempt_for`.
        // Read once, here, because the attempt has to be settled before a
        // placeholder is claimed and the same set is committed into the token
        // further down. Two reads was the first shape, with a comment claiming
        // it kept a query off the path of every turn -- which it did not, since
        // this one is on that path regardless.
        let granted = crate::grant::live_for(&self.pool, payload.workspace_id, job_id)
            .await
            .map_err(|e| anyhow::anyhow!("grants: {e}"))?;
        // Was this turn given back by a person answering, rather than by a
        // crash? That decides which attempt it writes, and it is not the same
        // question as whether it holds a yes: a decline answers and grants
        // nothing, so the reply it was refused on must be kept even though
        // there is no grant. A crashed retry was never answered, so it still
        // takes its own attempt back -- which is what stops an empty placeholder
        // stranding the session.
        //
        // Whether a *refusal* is retracted is a third question again, asked per
        // call against the grants themselves in `answered`.
        // The requests held in this conversation that somebody answered: those
        // still to be sent go out from this turn, and the rest are what the
        // model is told became of each.
        let held = crate::held::in_session(&self.pool, payload.workspace_id, payload.session_id)
            .await
            .map_err(|e| anyhow::anyhow!("held requests: {e}"))?;
        let answered_for = crate::grant::was_answered(&self.pool, payload.workspace_id, job_id)
            .await
            .map_err(|e| anyhow::anyhow!("answered: {e}"))?;
        let attempt = self
            .chat
            .attempt_for(payload.message_id, answered_for)
            .await
            .map_err(|e| anyhow::anyhow!("attempt: {e}"))?;

        let placeholder = self
            .chat
            .claim_placeholder(payload.message_id, payload.session_id, attempt)
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
        let mut settings = self
            .settings
            .resolve(payload.workspace_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("settings: {e}"))?;
        // Two of the settings that belong to a model are decided before any
        // model is asked, so they are decided for the chain: the conversation
        // has to fit whichever model ends up answering, and whether the
        // prompt is repeated is the first model's choice, since it is the one
        // expected to answer. Effort and temperature are the gateway's, per
        // model as it walks (docs/models.md).
        let traffic = traffic_type_for(&agent.policy, payload.user_id.is_none());
        let (budget, repeat) =
            sized_for_chain(&self.pool, payload.workspace_id, payload.agent_id, &traffic).await?;
        settings.context_budget = budget;
        settings.repeat_prompt = repeat;

        // Composed here rather than in the runtime, for the same reason the
        // settings are: the guest is handed prose and never learns which of it
        // the operator wrote, which the workspace added, or that either could
        // have been otherwise.
        //
        // These are the skills as they stand now. Gates are read from them on
        // every turn, so tightening one reaches a running conversation at once;
        // the prose a conversation was given is kept from its first turn and
        // composed again only at compaction -- see `kept_prompt`.
        let live_skills = self
            .skills
            .resolve_for_agent(payload.workspace_id, payload.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("skills: {e}"))?;

        // Resolved once: the prompt has to name the same model the request asks
        // for, or the agent is told one thing and served by another.
        let model = model_for(&self.pool, payload.workspace_id, &traffic).await?;

        // Composed before the conversation is built, because a summary is
        // written against it: what the agent was told to do is what decides
        // which parts of a conversation mattered.
        // What this turn's skills declare needs approving, and the commitment
        // over it. Computed here, beside the egress commitment, because both are
        // statements this tier makes about a turn and neither is the runtime's to
        // assert -- see `egress::gate`.
        let gates = assemble_gates(
            &self.pool,
            payload.workspace_id,
            &live_skills,
            &egress,
            settings.approve_new_hosts,
        )
        .await
        .map_err(|e| anyhow::anyhow!("gates: {e}"))?;

        // A turn resuming from an approval carries grants, and they travel with
        // it rather than changing what is gated.
        //
        // Dropping the satisfied gates was tried and is the wrong shape. A gate
        // dropped from the commitment is not enforced at all, so anything the
        // drop was too broad about becomes an ungated request -- and it was too
        // broad: keyed on the act alone, one approved GET uncommitted every
        // `reach` gate for every unreviewed host and method, and a unit grant for
        // one booking uncommitted the charging of every other. The failure
        // direction has to be the other one, so the gate stays and the grant is
        // what lets a particular request through.
        //
        // It also cannot work for a unit grant, whose key is a field of the
        // request body: nothing here has a body, and the only tier that does is
        // the one making the call.
        if !granted.is_empty() {
            tracing::info!(
                workspace_id = %payload.workspace_id,
                session_id = %payload.session_id,
                count = granted.len(),
                "a resumed turn carries approvals"
            );
        }

        // Committed together, so a grant is as unforgeable as the gate it is
        // about and a stripped one fails the same way.
        // Cloned because `with_grants` takes ownership and the same set is
        // needed again below, where it decides which refusal is retracted.
        //
        // The held requests are sent under gates of their own, with a grant for
        // each, committed into a second token. Not in this one: the guest's
        // calls travel under it, and the model making an approved call a
        // second time must be refused and asked about again, not let through.
        let waiting: Vec<&crate::held::Held> = held.iter().filter(|h| h.waiting()).collect();
        let held_gates = (!waiting.is_empty()).then(|| {
            outturn_core::egress::gate::Gates::of(gates.clone())
                .with_grants(waiting.iter().map(|h| h.grant()).collect())
        });
        // And what the workspace's policies say in advance, about this turn's
        // gates only: see `auto_approval`. Never with the held requests, which
        // carry a person's yes already.
        let policies = crate::auto_approval::for_turn(
            &self.pool,
            payload.workspace_id,
            payload.agent_id,
            job_id,
            &live_skills,
        )
        .await;
        let gates = outturn_core::egress::gate::Gates::of(gates)
            .with_grants(granted.clone())
            .with_policies(policies);
        let gate_commitment = gates.root(payload.workspace_id);

        let prompt_text = history
            .iter()
            .find(|m| m.id == payload.message_id)
            .map(|m| m.content.clone());

        // What the system prompt would be if it were composed now. Used when the
        // conversation has none kept, when its model changed, and when it
        // compacts; otherwise the kept one is sent, byte for byte, so the
        // provider's cache of it keeps hitting.
        let fresh_prompt =
            crate::skill::compose_for_turn(&agent.system_prompt, &live_skills, &model);
        let kept = self.kept_prompt(payload.session_id, &model).await?;

        // A skill somebody invoked is brought into context with the message
        // that invoked it, on every turn the message is projected in, so a
        // retry or a later turn sees what the first one saw. See
        // `skill::invoked`.
        let mut history = history;
        crate::skill::invoked::prepare(
            &mut history,
            &live_skills,
            kept.as_ref().map_or(&live_skills, |(skills, _)| skills),
            self.storage.as_ref(),
        )
        .await;

        // Sources come from the unmarked projection: `marked` inserts its one
        // entry immediately before the final message, which is past anything a
        // summary cuts at, so the indices a cut uses mean the same in both.
        let (projected, mut sources) = projected_with_sources(&history);
        let (projected, sending) = answered(projected, &gates, &granted, &held, Some(&mut sources));
        let held_calls: Vec<outturn_core::egress::held::HeldCall> = waiting
            .iter()
            .map(|h| h.call(sending.get(&h.id).cloned()))
            .collect();
        let projected = marked(projected, restarting_from.as_ref());

        // Over budget is where compaction begins. A summary is tried first
        // because it loses less: the early turns become a paragraph rather than
        // disappearing. It is a model call the user did not ask for, so it
        // happens only when the alternative is losing the messages outright.
        //
        // Written against the prompt as it would be composed now, because
        // compacting is when the conversation takes that prompt up: the summary
        // has to know what the agent is being told from here on.
        let summary = self
            .compacting(
                &projected,
                &sources,
                &fresh_prompt,
                crate::chat::trim::room_for_conversation(settings.context_budget, &fresh_prompt),
                payload.session_id,
                payload.workspace_id,
                payload.agent_id,
                &model,
            )
            .await;
        let compacted = summary.is_some();
        let projected = summary.unwrap_or(projected);

        let (skills, system_prompt) = match kept {
            Some(kept) if !compacted => kept,
            _ => {
                self.keep_prompt(payload.session_id, &model, &fresh_prompt, &live_skills)
                    .await?;
                (live_skills, fresh_prompt)
            }
        };

        // Written down before the turn runs, against the reply it will fill in:
        // the versions this turn was actually given, which in a long
        // conversation may be older than what is published. What was composed is
        // a fact about this turn whether or not it goes on to succeed, and a
        // failed turn is exactly the one an eval wants to be able to look at
        // afterwards.
        self.skills
            .record_turn(placeholder.message.id, &skills)
            .await
            .map_err(|e| anyhow::anyhow!("recording skills: {e}"))?;

        let conversation: Vec<outturn_core::protocol::work::ConversationMessage> = {
            // What the conversation may actually have. The system prompt is
            // sent on every round too and nothing can trim it, so it is spent
            // before any of this -- see `trim::room_for_conversation`.
            let room =
                crate::chat::trim::room_for_conversation(settings.context_budget, &system_prompt);

            // The floor under compaction. Whatever a summary did not
            // save, this drops -- and when there is no model to ask, or
            // the summary itself would not fit, this is the whole of what
            // happens.
            let (projected, trimmed) = crate::chat::trim::to_fit(projected, room);
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
                    // Both, because the difference is the whole point:
                    // a reader wondering why a generous budget trimmed
                    // anything is looking at the instructions.
                    room,
                    instructions = system_prompt.len(),
                    "conversation trimmed to fit"
                );
            }
            // Last, after compaction and the trim: repetition is how this
            // turn asks, not part of the conversation, so no summary is
            // written from it and no budget is spent cutting it.
            let projected = if settings.repeat_prompt {
                repeated(projected, prompt_text.as_deref())
            } else {
                projected
            };
            projected
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<_, _>>()?
        };

        // What the replies still in the conversation were signed with -- not
        // every reply's, since one trimmed or compacted away has nothing left
        // to attach to.
        let signed = {
            let calls: std::collections::HashSet<&str> = conversation
                .iter()
                .flat_map(|m| &m.parts)
                .filter_map(|p| match p {
                    outturn_core::protocol::work::ConversationPart::Call { call } => {
                        Some(call.id.as_str())
                    }
                    _ => None,
                })
                .collect();
            signed_replies(&self.pool, payload.session_id)
                .await?
                .into_iter()
                .filter(|s| calls.contains(s.first_call.as_str()))
                .collect()
        };

        Ok(Prepared::Run(Box::new(
            outturn_core::protocol::work::ExecuteRequest {
                session_id: payload.session_id,
                workspace_id: payload.workspace_id,
                agent_id: payload.agent_id,
                write_scopes: settings.write_scopes,
                read_scopes: settings.read_scopes,
                skill_files: crate::skill::objects_for_turn(&skills),
                skill_operations: crate::skill::operations_for_turn(&skills),
                conversation,
                // Composed with the model that will serve this turn, so an agent
                // asked what it is has something true to read rather than a gap to
                // fill.
                system_prompt,
                model: Some(model.clone()),
                timezone: payload.timezone.clone(),
                // Left to the gateway, which knows which model it is asking.
                reasoning_effort: None,
                temperature: None,
                signed,
                traffic_type: Some(traffic),
                max_tool_rounds: Some(i64::from(settings.max_tool_rounds)),
                max_written_bytes: Some(settings.max_written_bytes),
                remaining_stored_bytes: crate::quota::remaining(
                    self.settings.as_ref(),
                    &self.pool,
                    payload.workspace_id,
                )
                .await
                .unwrap_or_else(|e| {
                    // Unreadable is not "no quota": the turn may write nothing
                    // it could not later be held to.
                    tracing::warn!(workspace_id = %payload.workspace_id, error = %e,
                        "could not read the storage quota; the turn may write nothing");
                    Some(0)
                }),
                reply_id: placeholder.message.id,
                egress,
                egress_commitment,
                gate_commitment,
                gates: gates.into(),
                eager_tools,
                held: held_calls,
                held_gates: held_gates.map(Into::into),
            },
        )))
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
    pub(crate) async fn finish_turn(
        &self,
        job_id: Uuid,
        lease_token: Uuid,
        stream: impl futures::Stream<Item = Result<axum::body::Bytes, impl std::fmt::Display>> + Unpin,
    ) -> anyhow::Result<()> {
        let job = jobs::get(&self.pool, job_id).await?;
        let payload: ChatTurnPayload = serde_json::from_value(job.payload.clone())?;
        let lease_token = Some(lease_token);

        // Renewed for as long as this turn is being reported. Arriving bytes
        // are the evidence the turn is alive, which is better evidence than a
        // pod saying so. Nothing to renew against when the job was not
        // claimed the way a running turn is: then the lease is left alone
        // rather than extending someone else's.
        let heartbeat =
            lease_token.map(|token| LeaseHeartbeat::start(self.pool.clone(), job_id, token));

        // The reply, not the prompt. `payload.message_id` is the message this
        // turn answers; deltas and the finished content belong to the reply
        // that hangs off it. Attaching them to the prompt streams the answer
        // into the user's own message and leaves the reply empty for ever --
        // which is what a reader sees as their words replaced and a thinking
        // indicator that never resolves.
        //
        // Taken back rather than passed in, because this tier is reached by a
        // runtime reporting a job id and nothing else. `current_attempt` asks
        // for the attempt that already exists, which is the one `prepare_turn`
        // settled on when it started this turn. Deciding again here could pick
        // a different number and have one turn write two replies.
        let attempt = self
            .chat
            .current_attempt(payload.message_id)
            .await
            .map_err(|e| anyhow::anyhow!("attempt: {e}"))?;
        let placeholder = self
            .chat
            .claim_placeholder(payload.message_id, payload.session_id, attempt)
            .await
            .map_err(|e| anyhow::anyhow!("placeholder: {e}"))?;
        let reply_id = placeholder.message.id;

        let outcome = self.consume_turn(stream, &payload, reply_id, job_id).await;
        // The results have stopped arriving, so the evidence has: what
        // follows is this pod's own bookkeeping, which checks the lease
        // rather than extending it.
        drop(heartbeat);

        let reply = match outcome {
            Ok(reply) => reply,
            Err(e) => {
                tracing::error!(job_id = %job_id, error = %e, "chat turn failed");
                // Before anything is said about the failure. A pod whose lease
                // lapsed is reporting on a turn another pod may now be running:
                // announcing `chat.error` would tell that session's reader a
                // live turn had failed, and abandoning would discard the reply
                // the new holder is streaming into.
                let still_ours = match lease_token {
                    Some(token) => jobs::holds_lease(&self.pool, job_id, token).await?,
                    None => true,
                };
                if !still_ours {
                    return Err(LeaseLost.into());
                }
                // Given up on now rather than after the attempts run out: the
                // next attempt would fail the same way, after paying again for
                // every model call this one made on the way there.
                let declined = e.downcast_ref::<Declined>().map(|d| d.0.clone());
                let for_good = e.downcast_ref::<FailedForGood>().is_some() || declined.is_some();
                if let Some(refusal) = &declined {
                    self.record_refusal(&payload, refusal).await;
                }
                if for_good || job.attempts >= job.max_attempts {
                    self.abandon_payload_with(&payload, &e.to_string(), declined.as_ref())
                        .await;
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
                let failed = if for_good {
                    jobs::fail_for_good(&self.pool, job_id, &e.to_string(), lease_token).await
                } else {
                    jobs::fail(
                        &self.pool,
                        job_id,
                        &e.to_string(),
                        Duration::from_secs(5),
                        lease_token,
                    )
                    .await
                };
                match failed {
                    Ok(()) => {}
                    // Lost between the check above and here.
                    Err(jobs::JobError::NotFound) => return Err(LeaseLost.into()),
                    Err(other) => return Err(other.into()),
                }
                return Ok(());
            }
        };

        // Checked again before anything final is written. The heartbeat has
        // been renewing against this token; if that stopped succeeding, the
        // lease lapsed under a stall and this turn has been handed to another
        // pod, whose reply this must not overwrite.
        let still_ours = match lease_token {
            Some(token) => {
                jobs::extend_lease(&self.pool, job_id, jobs::DEFAULT_LEASE, token).await?
            }
            None => false,
        };
        if !still_ours {
            return Err(LeaseLost.into());
        }

        // `parts` is kept whenever it says something the flat reply does not:
        // which calls were made, and where the model stopped to think. A reply
        // that is one run of prose needs none of it, and storing it there would
        // be the same string twice.
        let thought = reply
            .parts
            .iter()
            .any(|p| matches!(p, crate::chat::parts::Part::Reasoning { .. }));
        let metadata = if reply.tools.is_empty() && !thought {
            serde_json::json!({})
        } else if reply.tools.is_empty() {
            serde_json::json!({ "parts": reply.parts })
        } else {
            serde_json::json!({ "tool_calls": reply.tools, "parts": reply.parts })
        };

        // Whether somebody asked for a stop, read before the final write and
        // acted on with it below.
        let stopped = matches!(jobs::cancel_requested(&self.pool, job_id).await, Ok(true));

        // The reply's final write, `chat.done` and the job's closing, in one
        // transaction. Apart, a crash after the write and before the closing
        // left a finished reply under a job still running; the reaper gave
        // the turn back, `attempt_for` saw a finished attempt that said
        // something, and the retry wrote a second reply beside the first --
        // two answers to one prompt, on screen. Together, the closing is in
        // the commit that makes the reply final, so a retry finds either a
        // closed job or an attempt still empty, and never the middle.
        let mut tx = self.pool.begin().await?;
        let finished = self
            .chat
            .set_message_content_on(
                &mut tx,
                reply_id,
                &reply.content,
                // What served, or nothing: a runtime that did not say leaves
                // the reply unattributed rather than credited to whatever
                // was configured.
                reply.model.as_deref(),
                reply.provider.as_deref(),
                reply.usage,
                metadata,
            )
            .await
            .map_err(|e| anyhow::anyhow!("finalize: {e}"))?;

        events::append_on(
            &mut tx,
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
        // A turn waiting on an approval is *kept* rather than finished. Parking
        // is what a suspended hold does everywhere else -- the job drops its
        // lease, gives back the attempt it spent, and `resume_parked` hands it
        // out again when somebody answers -- and this is the one place a hold
        // arrives after a turn was claimed, so `prepare_turn` never sees it.
        //
        // Completing instead is the bug this replaces: the hold was taken, the
        // reply was written, the job succeeded, and answering the approval had
        // nothing to give back. The agent said it had asked somebody, the person
        // said yes, and the conversation sat silent until the user typed again.
        //
        // After a cancel, because somebody pressing stop outranks a turn waiting
        // to carry on: the approval stays pending and answering it resumes
        // nothing, which is what stopping means.
        //
        // Each closing is matched against the lease. A closing that matches
        // nothing means the lease lapsed and the turn is running elsewhere:
        // the transaction is dropped with the reply in it, so the other pod's
        // result stands alone rather than beside this one.
        let mut parked = false;
        if stopped {
            match jobs::mark_cancelled_on(&mut *tx, job_id, lease_token).await {
                Ok(()) => {}
                Err(jobs::JobError::NotFound) => return Err(LeaseLost.into()),
                Err(other) => return Err(other.into()),
            }
        } else if reply.awaiting_approval {
            match jobs::park_on(&mut *tx, job_id, lease_token).await? {
                jobs::Parked::Parked => parked = true,
                // The lease lapsed and another pod holds this turn. Its
                // result stands; parking on top of it would take a turn
                // somebody else is running away from them.
                jobs::Parked::NotHeld => {
                    tracing::warn!(
                        job_id = %job_id,
                        "a turn waiting on an approval no longer holds its lease"
                    );
                    return Err(LeaseLost.into());
                }
            }
        } else {
            match jobs::complete_on(&mut *tx, job_id, lease_token).await {
                Ok(()) => {}
                Err(jobs::JobError::NotFound) => return Err(LeaseLost.into()),
                Err(other) => return Err(other.into()),
            }
        }
        tx.commit().await?;

        // Not fatal: without them the next turn starts without this reply's
        // reasoning, which is how every turn started before they were kept.
        if let Err(e) =
            keep_signatures(&self.pool, payload.session_id, finished.id, &reply.signed).await
        {
            tracing::warn!(error = %e, "could not keep what the reply was signed with");
        }

        if parked {
            tracing::info!(
                job_id = %job_id,
                session_id = %payload.session_id,
                "a turn is waiting on an approval and was kept"
            );
            // And the reader is told, which nothing else does from here:
            // `announce_hold` fires when a turn is *refused* at preparation,
            // and this one was already streaming when the hold arrived.
            // Without this the conversation shows a red tool error and a
            // working composer, which reads as finished rather than paused --
            // and the approval the event carries is what puts Approve in
            // front of the person who is already looking. Said as a reader
            // would say it: "waiting on an approval" and "the hold is lifted"
            // are words from inside this platform; whoever is watching is a
            // person who asked for something and is being told why it
            // stopped.
            self.announce_hold(&payload, "Paused: this needs somebody to approve it", true)
                .await;
        }

        // A conversation that has had a turn and still has no name gets one
        // asked for. After the job is closed, so a namer that cannot be
        // queued costs nothing but its absence.
        if let Ok(session) = self
            .chat
            .get_session(payload.workspace_id, payload.session_id)
            .await
        {
            let naming = self
                .settings
                .resolve(payload.workspace_id, payload.agent_id)
                .await
                .map(|s| s.session_naming)
                .unwrap_or_else(|_| "after_first_turn".to_string());
            crate::naming::enqueue_if_unnamed(&self.pool, &session, &naming).await;
        }
        Ok(())
    }
}

/// The conversation budget a turn on `traffic` can hold to, and whether its
/// prompt is repeated: the smallest budget any model on the chain resolves
/// to, so a fallback to a smaller window never finds the conversation too
/// long for it -- a refusal that would end the walk -- and the first model's
/// choice of repetition. A class with no chain is sized by the settings for
/// every model.
async fn sized_for_chain(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
    traffic: &str,
) -> anyhow::Result<(usize, bool)> {
    use outturn_core::model_settings::resolve;
    let links = outturn_core::chains::links_for(pool, workspace_id, traffic).await?;
    let mut each = Vec::with_capacity(links.len().max(1));
    for link in &links {
        each.push(
            resolve(
                pool,
                workspace_id,
                Some(agent_id),
                Some(link.model_id),
                &link.facts,
            )
            .await?,
        );
    }
    if each.is_empty() {
        each.push(
            resolve(
                pool,
                workspace_id,
                Some(agent_id),
                None,
                &Default::default(),
            )
            .await?,
        );
    }
    let budget = each
        .iter()
        .map(|s| s.context_budget)
        .min()
        .unwrap_or_default();
    Ok((
        usize::try_from(budget).unwrap_or(usize::MAX),
        each[0].repeat_prompt,
    ))
}

/// Model name from the agent's policy, falling back to the deployment default.
/// What class of traffic a turn is.
///
/// Names the work rather than the destination: the gateway decides where
/// "assistant" traffic goes, and can move it without the agent changing.
///
/// A class the agent's policy names wins, triggered or not: an agent confined
/// to one class is confined for a reason, and a schedule is not a way round
/// it. Otherwise a turn nobody sent -- a schedule's or a webhook's, which carry
/// no user -- is background, the same judgment that queues it behind people.
pub(super) fn traffic_type_for(policy: &serde_json::Value, nobody_waiting: bool) -> String {
    use outturn_core::protocol::{BACKGROUND_TRAFFIC_TYPE, DEFAULT_TRAFFIC_TYPE};
    match policy.get("traffic_type").and_then(|t| t.as_str()) {
        Some(named) => named.to_string(),
        None if nobody_waiting => BACKGROUND_TRAFFIC_TYPE.to_string(),
        None => DEFAULT_TRAFFIC_TYPE.to_string(),
    }
}

/// The model a turn on `traffic` will be asked of, so its prompt can name
/// it: the first of the chain that answers the class.
///
/// Nothing further. A model written into the code, or into an environment
/// variable, is one nobody chose: it answers every turn of a deployment that
/// forgot to configure one, silently. Refusing says what is missing.
pub(super) async fn model_for(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    traffic: &str,
) -> anyhow::Result<String> {
    outturn_core::chains::links_for(pool, workspace_id, traffic)
        .await?
        .into_iter()
        .next()
        .map(|first| first.model)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no model: no chain answers {traffic} traffic. The operator can give it one on \
                 Platform, Models"
            )
        })
}

#[cfg(test)]
mod traffic_type_tests {
    use super::traffic_type_for;
    use serde_json::json;

    #[test]
    fn a_turn_somebody_sent_is_assistant_traffic() {
        assert_eq!(traffic_type_for(&json!({}), false), "assistant");
    }

    #[test]
    fn a_turn_nobody_is_waiting_on_is_background_traffic() {
        assert_eq!(traffic_type_for(&json!({}), true), "background");
    }

    /// An agent confined to a class stays confined when a schedule runs it.
    #[test]
    fn a_class_the_agent_names_wins_either_way() {
        let policy = json!({ "traffic_type": "in-house" });
        assert_eq!(traffic_type_for(&policy, false), "in-house");
        assert_eq!(traffic_type_for(&policy, true), "in-house");
    }
}

#[cfg(test)]
mod usage_source_tests {
    use super::*;
    use crate::usage::UsageSource;

    /// A provider that said what its call cost is the authority on it.
    #[test]
    fn a_reported_usage_object_is_recorded_as_reported() {
        let usage = Some(serde_json::json!({ "prompt_tokens": 100, "completion_tokens": 20 }));
        assert_eq!(source_of(&usage, false, false), UsageSource::Reported);
    }

    /// A provider that reports as it goes leaves real numbers behind when the
    /// stream is cut, for the part that was delivered; one that reports only
    /// at the end leaves nothing, and the runtime's arithmetic stands in.
    /// Neither is a measurement of the whole call, and each says which it is.
    #[test]
    fn a_cut_call_is_recorded_as_partial_or_as_an_estimate() {
        let usage = Some(serde_json::json!({ "prompt_tokens": 100, "completion_tokens": 20 }));
        assert_eq!(source_of(&usage, true, false), UsageSource::ReportedPartial);
        assert_eq!(source_of(&None, true, true), UsageSource::Estimated);
    }

    /// A provider that said nothing leaves zeros, and zeros that claim to have
    /// been measured are the bug this column exists to prevent: a turn nobody
    /// counted then reads exactly like a turn that was free, and no later
    /// inspection can tell them apart.
    #[test]
    fn a_round_the_provider_never_costed_is_not_recorded_as_measured() {
        for absent in [None, Some(serde_json::Value::Null)] {
            assert_eq!(
                source_of(&absent, false, false),
                UsageSource::Unknown,
                "zeros with no provider figure behind them must say so, or a \
                 bill cannot be argued from this ledger"
            );
            assert_ne!(
                source_of(&absent, true, false),
                UsageSource::Reported,
                "recording an unmeasured round as reported is the silent error"
            );
        }
    }
}

/// What a turn's skills and its workspace's ceiling say needs approving.
///
/// One copy, called when a turn is prepared and again when a refusal comes back
/// claiming to have hit one. The second caller is why this is a free function:
/// `src/api/gated.rs` re-derives the gates rather than trusting the runtime's
/// account of what it was refused, and a second implementation there would be a
/// second answer to "what is gated", which is the one question this mechanism
/// cannot afford two answers to.
pub async fn assemble_gates(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    skills: &[crate::skill::ResolvedSkill],
    egress: &[outturn_core::egress::rule::EgressRule],
    approve_new_hosts: bool,
) -> anyhow::Result<Vec<outturn_core::egress::gate::Gate>> {
    let mut gates = crate::skill::gates_for_turn(pool, skills)
        .await
        .map_err(|e| anyhow::anyhow!("gates: {e}"))?
        .into_vec();

    // The workspace's own ceiling, if it set one. `approve_new_hosts` turns into
    // gates here rather than being a flag the gateway reads, so nothing
    // downstream has to know the setting exists: the commitment, the token
    // claim, the check, the refusal and the parked turn are all the
    // per-operation machinery, reused whole.
    //
    // Exempting the hosts a skill's own declaration opened is the point of the
    // setting rather than a softening of it. Those were consented to when the
    // skill was installed, in an act naming the skill and the host together; a
    // host somebody added by hand says agents *may* reach it, not that any use
    // of it was reviewed.
    if approve_new_hosts {
        let allowed: Vec<String> = egress.iter().map(|r| r.host.clone()).collect();
        let exempt = crate::egress::hosts_from_skills(pool, workspace_id)
            .await
            .map_err(|e| anyhow::anyhow!("skill hosts: {e}"))?;
        gates.extend(outturn_core::egress::gate::for_unreviewed_hosts(
            &allowed, &exempt,
        ));
    }

    Ok(gates)
}

/// The gates for a turn that is already running, re-derived from its payload.
///
/// Used by the refusal path, which has a job id and nothing else. Resolves the
/// same skills, egress rules and settings `prepare_turn` did -- so a skill
/// unbound mid-turn narrows what can be approved rather than widening it, which
/// is the safe direction.
pub async fn gates_for(
    pool: &sqlx::PgPool,
    payload: &ChatTurnPayload,
) -> anyhow::Result<outturn_core::egress::gate::Gates> {
    use crate::settings::SettingsStore as _;
    use crate::skill::SkillStore as _;

    let skills = crate::skill::PostgresSkillStore::new(pool.clone())
        .resolve_for_agent(payload.workspace_id, payload.agent_id)
        .await
        .map_err(|e| anyhow::anyhow!("skills: {e}"))?;

    let egress = crate::egress::rules_for(pool, payload.workspace_id)
        .await
        .map_err(|e| anyhow::anyhow!("egress: {e}"))?;

    let settings = crate::settings::PostgresSettingsStore::new(pool.clone())
        .resolve(payload.workspace_id, payload.agent_id)
        .await
        .map_err(|e| anyhow::anyhow!("settings: {e}"))?;

    Ok(outturn_core::egress::gate::Gates::of(
        assemble_gates(
            pool,
            payload.workspace_id,
            &skills,
            &egress,
            settings.approve_new_hosts,
        )
        .await?,
    ))
}
