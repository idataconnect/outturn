//! Naming a conversation nobody named.
//!
//! A session starts with no title and shows as "New Session" until it has
//! one. A person can name it at any time, and that name stands. When the
//! first turn ends and it is still unnamed, a job is queued to ask a model
//! for a few words that say what the conversation is about -- the way a mail
//! client fills in a subject line from the first message.
//!
//! The request goes through the gateway under its own traffic class,
//! `session-name`, so an operator can route it to a small, cheap model with
//! a row in `traffic_routes` rather than a deployment. Nothing here names a
//! model: absent a route the gateway's default serves it, which is the same
//! model that answered the turn.
//!
//! Runs in the API tier, like extraction: it is not work a turn is waiting
//! on, and only this tier can write the title.

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::auth::TokenMinter;
use crate::gateway::llm::types::{ChatCompletionRequest, ChatCompletionResponse, Message, MessageContent, Role};
use crate::{events, jobs};

use super::chat::{AgentSession, ChatStore};

/// The job kind. Serialised on the session so a fast second turn cannot
/// queue a second namer racing the first.
pub const NAME: &str = "session.name";

/// The class of traffic a naming request is, for the gateway to route.
pub const TRAFFIC_TYPE: &str = "session-name";

/// The most a title may be, whoever writes it. Long enough for a subject
/// line, short enough to fit a sidebar.
pub const MAX_TITLE_CHARS: usize = 80;

/// How much of the conversation the model is shown. The first exchange is
/// what the conversation is about; the rest is what it became.
const TRANSCRIPT_CHARS: usize = 4000;

const INSTRUCTION: &str = "Give this conversation a title of at most six words: \
    a noun phrase saying what it is about, the way a subject line would. \
    Reply with the title only -- no quotes, no punctuation at the end, \
    no explanation.";

/// Queues a namer for a session, unless it already has a name.
///
/// Called when a turn finishes. The check here is a courtesy that saves a
/// job; the namer checks again before it writes, because a person may have
/// named the session while the job waited.
pub async fn enqueue_if_unnamed(pool: &sqlx::PgPool, session: &AgentSession) {
    if !session.title.is_empty() {
        return;
    }
    let payload = serde_json::json!({ "session_id": session.id });
    if let Err(e) = jobs::enqueue(
        pool,
        session.workspace_id,
        NAME,
        payload,
        None,
        Some(&format!("name:{}", session.id)),
        // Nobody waits on a title.
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    {
        tracing::warn!(error = %e, session_id = %session.id, "could not queue session naming");
    }
}

/// Tells the feed a session's name changed, whoever changed it.
pub async fn announce(pool: &sqlx::PgPool, session: &AgentSession) {
    if let Err(e) = events::append(
        pool,
        session.workspace_id,
        Some(session.id),
        "session.renamed",
        serde_json::json!({ "title": session.title }),
    )
    .await
    {
        tracing::warn!(error = %e, session_id = %session.id, "could not announce rename");
    }
}

/// Claims naming work and does it, until shutdown.
pub fn spawn(
    pool: sqlx::PgPool,
    chat: Arc<dyn ChatStore>,
    minter: Arc<TokenMinter>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let Ok(gateway_url) = std::env::var("OUTTURN_GATEWAY_URL") else {
        tracing::info!("no OUTTURN_GATEWAY_URL; sessions will not be named");
        return;
    };
    let model = std::env::var("OUTTURN_DEFAULT_MODEL").unwrap_or_else(|_| "llama3.1".into());

    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = shutdown.notified() => return,
                _ = ticker.tick() => {
                    let claimed = match jobs::claim(&pool, &[NAME], 1, jobs::DEFAULT_LEASE).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(error = %e, "could not claim naming work");
                            continue;
                        }
                    };
                    for handle in claimed {
                        let job = handle.job;
                        let done = match run_one(&client, &gateway_url, &model, &pool, chat.as_ref(), &minter, &job).await {
                            Ok(()) => jobs::complete(&pool, job.id, job.lease_token).await,
                            Err(e) => {
                                tracing::warn!(job_id = %job.id, error = %e, "session naming failed");
                                jobs::fail(&pool, job.id, &e.to_string(), Duration::from_secs(30), job.lease_token).await
                            }
                        };
                        if let Err(e) = done {
                            tracing::warn!(job_id = %job.id, error = %e, "could not close naming job");
                        }
                    }
                }
            }
        }
    });
}

async fn run_one(
    client: &reqwest::Client,
    gateway_url: &str,
    model: &str,
    pool: &sqlx::PgPool,
    chat: &dyn ChatStore,
    minter: &TokenMinter,
    job: &jobs::Job,
) -> anyhow::Result<()> {
    let session_id: Uuid = job
        .payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("job has no session_id"))?;

    // A session deleted while the job waited is nothing to name; one named
    // meanwhile is somebody's decision, and stands.
    let session = match chat.get_session(job.workspace_id, session_id).await {
        Ok(s) => s,
        Err(super::chat::ChatError::NotFound) => return Ok(()),
        Err(e) => anyhow::bail!("session: {e}"),
    };
    if !session.title.is_empty() {
        return Ok(());
    }

    let history = chat.messages(session_id).await.map_err(|e| anyhow::anyhow!("messages: {e}"))?;
    let transcript = transcript(history.messages.iter().map(|m| (m.role.as_str(), m.content.as_str())));
    if transcript.is_empty() {
        return Ok(());
    }

    // This call names a session; it never runs workspace code and carries no
    // egress rules, so the commitment it mints is the same empty one every
    // workspace with no rules gets -- there is nothing here for the gateway
    // to check against.
    let token = minter.mint_turn(session_id, job.workspace_id, crate::egress::commit::empty_root())?;
    let request = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![
            Message { role: Role::System, content: MessageContent::Text(INSTRUCTION.into()), name: None, tool_calls: None, tool_call_id: None },
            Message { role: Role::User, content: MessageContent::Text(transcript), name: None, tool_calls: None, tool_call_id: None },
        ],
        temperature: Some(0.2),
        max_tokens: Some(32),
        tools: None,
        // A title is not something to think about.
        reasoning_effort: Some("none".into()),
        stream: false,
        stream_options: None,
    };

    let response = client
        .post(format!("{gateway_url}/v1/chat/completions"))
        .bearer_auth(token)
        .header(crate::gateway::TRAFFIC_HEADER, TRAFFIC_TYPE)
        .json(&request)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        anyhow::bail!("gateway returned {status}: {detail}");
    }
    // Read before the body is consumed, and the only place the endpoint and
    // payer are named: the completion itself does not say.
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

    let completion: ChatCompletionResponse = response.json().await?;

    // Naming a session is a model call the user did not ask for and is billed
    // for regardless, so it belongs in the ledger like any other. Recorded
    // before the title is validated: a reply that turned out to be unusable
    // still cost what it cost.
    let provider_usage = completion
        .usage
        .as_ref()
        .and_then(|u| serde_json::to_value(u).ok());
    let counted = completion.usage.as_ref();
    let usage_store = crate::api::usage::PostgresUsageStore::new(pool.clone());
    if let Err(e) = crate::api::usage::UsageStore::record(
        &usage_store,
        crate::api::usage::RecordUsage {
            workspace_id: job.workspace_id,
            // No agent and no job of the agent's: naming is the platform's own
            // work on behalf of a session.
            agent_id: None,
            session_id: Some(session_id),
            user_id: None,
            account: None,
            reply_id: None,
            job_id: None,
            round: 0,
            traffic_type: TRAFFIC_TYPE.to_string(),
            endpoint,
            model: if completion.model.is_empty() {
                model.to_string()
            } else {
                completion.model.clone()
            },
            credential_owner: paid_by,
            fallback: "none".to_string(),
            prompt_tokens: counted.map(|u| u.prompt_tokens as i32).unwrap_or(0),
            completion_tokens: counted.map(|u| u.completion_tokens as i32).unwrap_or(0),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            usage_source: match &provider_usage {
                Some(u) if !u.is_null() => crate::api::usage::UsageSource::Reported,
                _ => crate::api::usage::UsageSource::Unknown,
            },
            provider_usage,
            service_tier: None,
        },
    )
    .await
    {
        // Loud, like every other ledger write: the bill is wrong without it.
        tracing::error!(
            workspace_id = %job.workspace_id,
            session_id = %session_id,
            error = %e,
            "could not record the session naming's usage"
        );
    }

    let raw = completion
        .choices
        .first()
        .map(|c| match &c.message.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    crate::gateway::llm::types::ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        })
        .unwrap_or_default();

    let Some(title) = tidy(&raw) else {
        anyhow::bail!("model returned no usable title: {raw:?}");
    };

    // Written only over nothing. The read above and this write are not one
    // transaction, and a person naming the session in between must win.
    let renamed = sqlx::query_scalar::<_, Uuid>(
        "update agent_sessions set title = $2, updated_at = now() \
         where id = $1 and title = '' returning id",
    )
    .bind(session_id)
    .bind(&title)
    .fetch_optional(pool)
    .await?;
    if renamed.is_none() {
        return Ok(());
    }
    tracing::info!(session_id = %session_id, title = %title, "session named");
    announce(pool, &AgentSession { title, ..session }).await;
    Ok(())
}

/// The conversation as the namer sees it: who said what, up to a budget,
/// with an agent's empty placeholders and tool chatter left out.
fn transcript<'a>(messages: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::new();
    for (role, content) in messages {
        let content = content.trim();
        if content.is_empty() || !(role == "user" || role == "assistant") {
            continue;
        }
        let line = format!("{role}: {content}\n");
        if out.len() + line.len() > TRANSCRIPT_CHARS {
            let room = TRANSCRIPT_CHARS.saturating_sub(out.len());
            let cut = line.char_indices().map(|(i, _)| i).take_while(|i| *i <= room).last().unwrap_or(0);
            out.push_str(&line[..cut]);
            break;
        }
        out.push_str(&line);
    }
    out
}

/// A model's reply as a title, or None if there is nothing in it.
///
/// Models given "title only" still wrap it in quotes, prefix it with
/// "Title:", or end it with a full stop; none of that is the title.
fn tidy(raw: &str) -> Option<String> {
    let first = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut t = first;
    for prefix in ["Title:", "title:", "TITLE:"] {
        t = t.strip_prefix(prefix).unwrap_or(t).trim();
    }
    let t = t.trim_matches(|c: char| matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’' | '*' | '`'));
    let t = t.trim_end_matches(|c: char| matches!(c, '.' | '!' | ':' | ';' | ','));
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.chars().take(MAX_TITLE_CHARS).collect::<String>().trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_strips_dressing() {
        assert_eq!(tidy("\"Quarterly Revenue Question\""), Some("Quarterly Revenue Question".into()));
        assert_eq!(tidy("Title: Weather in Reykjavik."), Some("Weather in Reykjavik".into()));
        assert_eq!(tidy("**Invoice Archive Review**\nMore words"), Some("Invoice Archive Review".into()));
        assert_eq!(tidy("  \n  "), None);
        assert_eq!(tidy("\"\""), None);
    }

    #[test]
    fn tidy_caps_length() {
        let long = "x".repeat(200);
        assert_eq!(tidy(&long).unwrap().chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn transcript_keeps_speech_and_drops_the_rest() {
        let t = transcript(
            [("user", "hello"), ("assistant", ""), ("tool", "{}"), ("assistant", "hi there")].into_iter(),
        );
        assert_eq!(t, "user: hello\nassistant: hi there\n");
    }

    #[test]
    fn transcript_is_bounded() {
        let big = "y".repeat(TRANSCRIPT_CHARS * 2);
        let t = transcript([("user", big.as_str()), ("assistant", "never seen")].into_iter());
        assert!(t.len() <= TRANSCRIPT_CHARS);
        assert!(!t.contains("never seen"));
    }
}
