use uuid::Uuid;

use super::Worker;
use super::projection::projected_with_sources;
use super::turn::{model_for, source_of, traffic_type_for};

/// What a conversation's kept system prompt is against what would be composed
/// now.
#[derive(Debug, serde::Serialize)]
pub struct PromptStatus {
    /// When it was composed. Absent before the first turn.
    pub composed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether it is what would be composed now. False also when only the
    /// agent's own prompt changed, which `skills` cannot show.
    pub current: bool,
    /// The skills whose version moved on, or were added or removed since.
    pub skills: Vec<StaleSkill>,
}

#[derive(Debug, serde::Serialize)]
pub struct StaleSkill {
    pub name: String,
    /// The version ordinal the conversation has; absent for a skill added since.
    pub kept: Option<i32>,
    /// The one it would have now; absent for a skill removed since.
    pub live: Option<i32>,
}

impl Worker {
    /// `summarized`, announced: a compaction is a model call in the middle of
    /// preparing a turn, and a reader left watching a pause with no reason
    /// given is told nothing about why the reply is slow to start. Said only
    /// when a summary is actually attempted.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn compacting(
        &self,
        conversation: &[serde_json::Value],
        sources: &[Uuid],
        system_prompt: &str,
        budget: usize,
        session_id: Uuid,
        workspace_id: Uuid,
        agent_id: Uuid,
        model: &str,
    ) -> Option<Vec<serde_json::Value>> {
        if crate::chat::trim::total_cost(conversation) <= budget
            || self.minter.is_none()
            || self.gateway_url.is_none()
            || crate::chat::summarize::boundary(conversation).is_none()
        {
            return None;
        }
        self.tell(
            workspace_id,
            session_id,
            "chat.compacting",
            serde_json::json!({}),
        )
        .await;
        let summary = self
            .summarized(
                conversation,
                sources,
                system_prompt,
                budget,
                session_id,
                workspace_id,
                agent_id,
                model,
            )
            .await;
        self.tell(
            workspace_id,
            session_id,
            "chat.compacted",
            serde_json::json!({ "summarized": summary.is_some() }),
        )
        .await;
        summary
    }

    /// Compacts a conversation because somebody asked: summarizes what the
    /// usual rule would summarize, whatever the budget, and composes the
    /// system prompt again from the agent and its skills as they stand.
    ///
    /// Not a special case of anything. Asked of a conversation too short to
    /// summarize, the summary is skipped as it always would be and the prompt
    /// is still composed again, since that is what compacting is for here:
    /// taking up what was published since the conversation began.
    pub async fn compact(&self, workspace_id: Uuid, session_id: Uuid) -> anyhow::Result<()> {
        let session = match self.chat.get_session(workspace_id, session_id).await {
            Ok(s) => s,
            Err(crate::chat::ChatError::NotFound) => return Ok(()),
            Err(e) => anyhow::bail!("session: {e}"),
        };
        let (agent, _) = self
            .agent_for_turn(workspace_id, session.agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("agent: {e}"))?;
        let live_skills = self
            .skills
            .resolve_for_agent(workspace_id, agent.id)
            .await
            .map_err(|e| anyhow::anyhow!("skills: {e}"))?;
        let model = model_for(
            &self.pool,
            workspace_id,
            &traffic_type_for(&agent.policy, false),
        )
        .await?;
        let fresh_prompt =
            crate::skill::compose_for_turn(&agent.system_prompt, &live_skills, &model);

        let mut history = self
            .chat
            .turn_history(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("history: {e}"))?;
        // Summarized as a turn would send it, and against the prompt this
        // compaction is about to compose.
        crate::skill::invoked::prepare(
            &mut history.messages,
            &live_skills,
            &live_skills,
            self.storage.as_ref(),
        )
        .await;
        let (projected, sources) = projected_with_sources(&history.messages);

        self.tell(
            workspace_id,
            session_id,
            "chat.compacting",
            serde_json::json!({}),
        )
        .await;
        // A budget of nothing is what "whatever the budget" means to the one
        // check that reads it; everything else is the ordinary path.
        let summarized = self
            .summarized(
                &projected,
                &sources,
                &fresh_prompt,
                0,
                session_id,
                workspace_id,
                agent.id,
                &model,
            )
            .await
            .is_some();
        self.keep_prompt(session_id, &model, &fresh_prompt, &live_skills)
            .await?;
        self.tell(
            workspace_id,
            session_id,
            "chat.compacted",
            serde_json::json!({ "summarized": summarized }),
        )
        .await;
        tracing::info!(%session_id, summarized, "conversation compacted on request");
        Ok(())
    }

    /// Whether a conversation's kept system prompt is still what would be
    /// composed now, and if not, which skills moved on. Asked by the session
    /// page, so a conversation running on an older version says so rather
    /// than looking like an edit that did not take.
    pub async fn prompt_status(
        &self,
        workspace_id: Uuid,
        session_id: Uuid,
        agent_id: Uuid,
    ) -> anyhow::Result<PromptStatus> {
        use sqlx::Row as _;
        let Some(row) = sqlx::query(
            "select model, prompt, skills, composed_at from session_prompts where session_id = $1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(PromptStatus {
                composed_at: None,
                current: true,
                skills: Vec::new(),
            });
        };
        let (agent, _) = self
            .agent_for_turn(workspace_id, agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("agent: {e}"))?;
        let live = self
            .skills
            .resolve_for_agent(workspace_id, agent_id)
            .await
            .map_err(|e| anyhow::anyhow!("skills: {e}"))?;
        let model: String = row.get("model");
        // Another model composes again on the next turn by itself; nothing to
        // offer here.
        let now = model_for(
            &self.pool,
            workspace_id,
            &traffic_type_for(&agent.policy, false),
        )
        .await
        .ok();
        if now.as_deref() != Some(model.as_str()) {
            return Ok(PromptStatus {
                composed_at: Some(row.get("composed_at")),
                current: true,
                skills: Vec::new(),
            });
        }
        let kept: Vec<crate::skill::ResolvedSkill> =
            serde_json::from_value(row.get("skills")).unwrap_or_default();
        let fresh = crate::skill::compose_for_turn(&agent.system_prompt, &live, &model);
        let current = fresh == row.get::<String, _>("prompt");

        let mut ids: Vec<Uuid> = kept.iter().chain(&live).map(|s| s.version_id).collect();
        ids.sort();
        ids.dedup();
        let ordinals: std::collections::HashMap<Uuid, i32> =
            sqlx::query_as("select id, ordinal from skill_versions where id = any($1)")
                .bind(&ids)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .collect();
        let mut skills = Vec::new();
        for s in &live {
            let was = kept.iter().find(|k| k.skill_id == s.skill_id);
            if was.map(|k| k.version_id) != Some(s.version_id) {
                skills.push(StaleSkill {
                    name: s.name.clone(),
                    kept: was.and_then(|k| ordinals.get(&k.version_id).copied()),
                    live: ordinals.get(&s.version_id).copied(),
                });
            }
        }
        for k in kept
            .iter()
            .filter(|k| !live.iter().any(|s| s.skill_id == k.skill_id))
        {
            skills.push(StaleSkill {
                name: k.name.clone(),
                kept: ordinals.get(&k.version_id).copied(),
                live: None,
            });
        }
        Ok(PromptStatus {
            composed_at: Some(row.get("composed_at")),
            current,
            skills,
        })
    }

    /// The system prompt this conversation was given, and the skills it was
    /// composed from -- unless it has none yet, or was composed for another
    /// model, in which case it is composed again.
    ///
    /// Kept rather than rebuilt per turn so every round of every turn sends the
    /// same prefix, which is what a provider's prompt cache keys on. An edit to
    /// the agent or a skill reaches a conversation at its next compaction. A
    /// kept set that no longer reads -- a shape changed by a deploy -- is
    /// composed again rather than failing the turn.
    pub(super) async fn kept_prompt(
        &self,
        session_id: Uuid,
        model: &str,
    ) -> anyhow::Result<Option<(Vec<crate::skill::ResolvedSkill>, String)>> {
        let row =
            sqlx::query("select model, prompt, skills from session_prompts where session_id = $1")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some(row) = row else { return Ok(None) };
        use sqlx::Row as _;
        if row.get::<String, _>("model") != model {
            return Ok(None);
        }
        let skills = match serde_json::from_value(row.get("skills")) {
            Ok(skills) => skills,
            Err(e) => {
                tracing::warn!(%session_id, error = %e, "a kept system prompt no longer reads; composing it again");
                return Ok(None);
            }
        };
        Ok(Some((skills, row.get("prompt"))))
    }

    /// Keeps what a conversation's system prompt was composed from, replacing
    /// whatever it had.
    pub(super) async fn keep_prompt(
        &self,
        session_id: Uuid,
        model: &str,
        prompt: &str,
        skills: &[crate::skill::ResolvedSkill],
    ) -> anyhow::Result<()> {
        sqlx::query(
            "insert into session_prompts (session_id, model, prompt, skills) \
             values ($1, $2, $3, $4) \
             on conflict (session_id) do update \
             set model = excluded.model, prompt = excluded.prompt, \
                 skills = excluded.skills, composed_at = now()",
        )
        .bind(session_id)
        .bind(model)
        .bind(prompt)
        .bind(serde_json::to_value(skills)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replaces the early part of a conversation with a summary of it, when it
    /// is over budget and there is a model to ask.
    ///
    /// Returns `None` whenever it did not happen, for any reason -- no
    /// gateway, nothing worth summarizing, the model refused, the summary came
    /// back empty. Every one of those means the trim underneath does the work
    /// alone, which is exactly what it is for. A failure here must never cost
    /// a turn: the user asked for an answer, not for a summary.
    async fn summarized(
        &self,
        conversation: &[serde_json::Value],
        // Which stored message each projected entry came from, so the summary
        // can be stored against the last one it covers. The projection has no
        // ids in it -- it is what goes to the model -- and a summary that
        // cannot say what it stands in for cannot be carried. Counting the two
        // against each other is what went wrong before; this is told.
        sources: &[uuid::Uuid],
        system_prompt: &str,
        budget: usize,
        session_id: uuid::Uuid,
        workspace_id: uuid::Uuid,
        // Whose turn paid for it. A summary is the platform's own initiative in
        // the sense that the user did not ask for one, but it is made of this
        // agent's conversation, against this agent's system prompt, to fit this
        // agent's context budget -- so the spend is the agent's and the ledger
        // says so. Naming is the other case and stays unattributed: it runs in
        // its own worker for a session that need not have an agent yet.
        agent_id: uuid::Uuid,
        // The model this turn is routed to. The summary goes to the same one:
        // asking a different model would bill a route the turn is not using,
        // and a deployment whose default is unset or unroutable would fail
        // every compaction while the turn itself is fine.
        model: &str,
    ) -> Option<Vec<serde_json::Value>> {
        use crate::chat::summarize;
        use outturn_core::llm::{
            ChatCompletionRequest, ChatCompletionResponse, ContentPart, MessageContent,
        };

        if crate::chat::trim::total_cost(conversation) <= budget {
            return None;
        }
        let (Some(minter), Some(gateway_url)) = (&self.minter, &self.gateway_url) else {
            return None;
        };
        let through = summarize::boundary(conversation)?;

        let request = summarize::request(system_prompt, &conversation[..through]);
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
            // Summarizing is reading, not deciding.
            reasoning_effort: Some("none".into()),
            stream: false,
            stream_options: None,
            facts: None,
        };

        // Signed for the session it is summarizing, so the spend lands on the
        // workspace that caused it: every model call is a row in the usage
        // ledger, and a summary nobody is billed for is a summary nobody can
        // account for. No egress commitment, because summarizing reaches
        // nothing but the model.
        let token = minter
            .mint_turn(
                session_id,
                workspace_id,
                outturn_core::egress::commit::empty_root(),
                // As in `naming`: said rather than absent, and empty because
                // summarizing reaches nothing but the model.
                outturn_core::egress::gate::Gates::none().root(workspace_id),
                None,
            )
            .ok()?;

        // With a deadline. This runs while a turn is being prepared, under a
        // lease the heartbeat keeps renewing for as long as the call takes --
        // so a gateway that accepted the request and never answered would
        // hold the turn, and the runtime's poll for it, for ever. The idle
        // deadline is the one the gateway's own calls use: a summary is one
        // read, so silence that long is silence for the whole of it.
        let response =
            outturn_core::http_client::streaming_client(outturn_core::http_client::IDLE_TIMEOUT)
                .post(format!("{gateway_url}/v1/chat/completions"))
                .bearer_auth(token)
                .header(
                    outturn_core::protocol::TRAFFIC_HEADER,
                    summarize::TRAFFIC_TYPE,
                )
                .json(&body)
                .send()
                .await
                .ok()?;

        if !response.status().is_success() {
            tracing::warn!(
                status = %response.status(),
                "could not summarize; the trim will carry the conversation"
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
        let summary = completion
            .choices
            .first()
            .map(|c| match &c.message.content {
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
        if summary.len() > summarize::SUMMARY_MAX_BYTES {
            tracing::warn!(
                summary_bytes = summary.len(),
                limit = summarize::SUMMARY_MAX_BYTES,
                "the summary came back longer than a summary may be; the trim will carry the conversation"
            );
            return None;
        }

        let replaced = summarize::apply(conversation.to_vec(), summary, through);
        // A summary larger than what it replaced is one that helped nobody,
        // and sending it would be worse than the trim alone.
        if crate::chat::trim::total_cost(&replaced) >= crate::chat::trim::total_cost(conversation) {
            tracing::warn!(
                "the summary was no smaller than the conversation; keeping the original"
            );
            return None;
        }

        // A carried summary that has grown past the bound is folded rather
        // than carried again: it was at the front of what was just summarized,
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
        // Covered is the stored message the last summarized entry came from,
        // which `sources` names outright. It used to be counted instead --
        // `history.len() - TAIL_MESSAGES - 1` -- which assumed the projection
        // and the stored rows were aligned from the end. They are not, and a
        // stored summary breaks the alignment by exactly one: it is a single
        // entry at the *front* of the projection while still being a row near
        // the *end* of history, and the rows it hides are gone from one and
        // present in the other. From the second summarization round onward the
        // count named a message the summary had never read, `project` then
        // dropped it as covered, and every later mark sat above it -- one
        // message silently out of context, permanently, per round.
        //
        // `through` indexes the marked projection, and `marked` inserts its
        // one entry immediately before the final message, which is always
        // after a cut that leaves `TAIL_MESSAGES` behind it. So indices below
        // the cut mean the same thing in both, and the guard below is what
        // says so rather than assuming it.
        if let Some(through_id) = through
            .checked_sub(1)
            .and_then(|last| sources.get(last))
            .copied()
        {
            self.store_summary(session_id, summary, through_id, model)
                .await;
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
        // The session's own label for whose conversation this is. Looked up
        // rather than threaded through, because this is one call at the end of
        // a compaction that usually does not happen; the turn path keeps its
        // copy because it writes a row per round.
        let account = self
            .chat
            .get_session(workspace_id, session_id)
            .await
            .ok()
            .and_then(|s| s.account);
        if let Err(e) = self
            .usage
            .record(crate::usage::RecordUsage {
                workspace_id,
                // The agent whose turn this compacted, so the spend lands with
                // the work that caused it rather than in an unattributed pile
                // nobody can explain. No job id: the summary is not a job of
                // its own, it is part of the turn already running.
                agent_id: Some(agent_id),
                session_id: Some(session_id),
                user_id: None,
                account,
                reply_id: None,
                job_id: None,
                round: 0,
                traffic_type: summarize::TRAFFIC_TYPE.to_string(),
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
                // In the gateway's one meaning, so the cache a summary's
                // prompt hit is on its row like any other call's.
                cache_read_tokens: counted
                    .and_then(|u| u.prompt_tokens_details.as_ref())
                    .map_or(0, |d| d.cached_tokens as i32),
                cache_write_tokens: counted
                    .and_then(|u| u.prompt_tokens_details.as_ref())
                    .map_or(0, |d| d.cache_creation_tokens as i32),
                reasoning_tokens: counted
                    .and_then(|u| u.completion_tokens_details.as_ref())
                    .map_or(0, |d| d.reasoning_tokens as i32),
                // Whole, not streamed: nothing cuts it and nothing estimates.
                usage_source: source_of(&provider_usage, false, false),
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
            "conversation summarized"
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
        use crate::chat::{Delivery, Usage};

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
                serde_json::json!({ crate::chat::summarize::SUMMARY_MARK: through.to_string() }),
            )
            .await
        {
            tracing::warn!(error = %e, "could not mark the summary; it will be written again next turn");
        }
    }
}
