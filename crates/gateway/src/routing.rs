//! Resolving a class of traffic to the providers that serve it.
//!
//! The caller names what the work is for -- "assistant" for a user waiting on
//! a reply -- and the gateway decides where it goes. That division exists
//! because only the gateway knows which endpoints are currently healthy, and
//! because a caller that named a provider directly could not be failed over
//! without lying to it about what it asked for.
//!
//! A class walks a chain of models (docs/models.md), each at a provider
//! account, so switching a class of traffic to a cheaper model is
//! configuration rather than a deployment. Each link becomes a route: where
//! to go, what to ask for, and what that model accepts.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use outturn_core::egress::seal::Kind;
use outturn_core::facts::ModelFacts;

use crate::egress::sealed::{Credentials, Opened};

use super::llm::provider::{
    LlmProvider, anthropic::AnthropicProvider, gemini::GeminiProvider, openai::OpenAiProvider,
};

pub use outturn_core::protocol::{BACKGROUND_TRAFFIC_TYPE, DEFAULT_TRAFFIC_TYPE};

/// The class to borrow routes from when this one has none.
///
/// Background work is assistant work nobody is watching, so until somebody
/// routes it separately it goes where assistant traffic goes. Not to the
/// static providers, which is where an unrouted class otherwise lands: a
/// deployment that confined its assistant traffic to one provider would find
/// its scheduled turns at a different vendor the day this class appeared.
fn borrows_from(traffic_type: &str) -> Option<&'static str> {
    match traffic_type {
        BACKGROUND_TRAFFIC_TYPE => Some(DEFAULT_TRAFFIC_TYPE),
        _ => None,
    }
}

/// One attempt: where to go, what to ask for, and what it accepts.
#[derive(Debug, Clone)]
pub struct Route {
    /// The provider account: what a built provider is kept under.
    pub account: Uuid,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    /// The account's key, named in the gateway's environment or sealed;
    /// at most one.
    pub credential_ref: Option<String>,
    pub credential_id: Option<Uuid>,
    pub facts: Arc<ModelFacts>,
}

impl Route {
    /// Matches the key the circuit breaker records health against.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.provider, self.base_url)
    }
}

/// Routes for a traffic type, in the order to try them: its chain's links.
///
/// A workspace's own chain replaces the platform's rather than extending it:
/// a workspace that has configured where its traffic goes should not have
/// requests quietly fall through to somebody else's endpoint.
pub async fn routes_for(
    pool: &PgPool,
    workspace_id: Uuid,
    traffic_type: &str,
) -> Result<Vec<Route>, sqlx::Error> {
    let routes = routes_of(pool, workspace_id, traffic_type).await?;
    match borrows_from(traffic_type) {
        Some(parent) if routes.is_empty() => routes_of(pool, workspace_id, parent).await,
        _ => Ok(routes),
    }
}

/// One class's own chain, with no borrowing.
async fn routes_of(
    pool: &PgPool,
    workspace_id: Uuid,
    traffic_type: &str,
) -> Result<Vec<Route>, sqlx::Error> {
    let rows = sqlx::query(
        "select a.id as account, a.protocol, a.base_url, a.credential_env, a.credential_id, m.model, m.facts, m.overrides \
         from chain_assignments c \
         join chain_links l on l.chain_id = c.chain_id \
         join models m on m.id = l.model_id \
         join provider_accounts a on a.id = m.account_id \
         where c.traffic_type = $2 \
           and c.workspace_id is not distinct from ( \
               select case when exists ( \
                   select 1 from chain_assignments \
                   where workspace_id = $1 and traffic_type = $2 \
               ) then $1 else null end \
           ) \
         order by l.position",
    )
    .bind(workspace_id)
    .bind(traffic_type)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            let facts: Option<serde_json::Value> = r.get("facts");
            let overrides: serde_json::Value = r.get("overrides");
            Route {
                account: r.get("account"),
                provider: r.get("protocol"),
                base_url: r.get("base_url"),
                model: r.get("model"),
                credential_ref: r.get("credential_env"),
                credential_id: r.get("credential_id"),
                facts: Arc::new(ModelFacts::of(facts.as_ref(), Some(&overrides))),
            }
        })
        .collect())
}

/// Builds providers from routes, keeping one per account.
///
/// Providers own a connection pool, so constructing one per request would
/// throw away keep-alive and make every call pay a fresh TLS handshake. They
/// are kept per account rather than per route because two models at one
/// account share a connection perfectly well -- and per account rather than
/// per endpoint because two accounts at one endpoint hold different keys.
#[derive(Default)]
pub struct ProviderCache {
    built: tokio::sync::Mutex<HashMap<Uuid, Built>>,
}

struct Built {
    provider: Arc<dyn LlmProvider>,
    /// The sealed key it was built with, so a rotated or revoked one is
    /// noticed: the credential cache hands back a different one, or none.
    sealed: Option<Arc<Opened>>,
}

impl ProviderCache {
    /// The provider for a route, for a request from `workspace`. None when
    /// the account's key cannot be had, so the next route is tried.
    pub async fn get(
        &self,
        route: &Route,
        sealed: &Credentials,
        workspace: Uuid,
    ) -> Option<Arc<dyn LlmProvider>> {
        // Opened afresh each time -- from memory, almost always -- so a key
        // revoked a moment ago is not still being sent.
        let opened = match route.credential_id {
            Some(id) => Some(open_provider_key(sealed, id, route, workspace).await?),
            None => None,
        };

        let mut built = self.built.lock().await;
        if let Some(existing) = built.get(&route.account) {
            let same_key = match (&existing.sealed, &opened) {
                (None, None) => true,
                (Some(was), Some(now)) => Arc::ptr_eq(was, now),
                _ => false,
            };
            if same_key {
                return Some(Arc::clone(&existing.provider));
            }
        }

        // From the seal, or resolved from the environment at construction,
        // so the secret is never in the database in the clear and never in
        // a log line.
        let credential = match &opened {
            Some(opened) => match std::str::from_utf8(&opened.secret) {
                Ok(key) => Some(key.trim().to_string()),
                Err(_) => {
                    tracing::warn!(account = %route.account, "a provider key is not text, skipping");
                    return None;
                }
            },
            None => route
                .credential_ref
                .as_deref()
                .and_then(|name| std::env::var(name).ok()),
        }
        .filter(|value| !value.trim().is_empty());

        let provider: Arc<dyn LlmProvider> = match route.provider.as_str() {
            "openai" => Arc::new(OpenAiProvider::new(route.base_url.clone(), credential)),
            "anthropic" => {
                // Anthropic has no unauthenticated mode, so a route without a
                // resolvable credential is a misconfiguration rather than a
                // local endpoint. Skipped so the next route is tried.
                let Some(key) = credential else {
                    tracing::warn!(
                        base_url = %route.base_url,
                        credential_ref = ?route.credential_ref,
                        "anthropic route has no resolvable credential, skipping"
                    );
                    return None;
                };
                Arc::new(AnthropicProvider::new(route.base_url.clone(), key))
            }
            "bedrock" => {
                // A Bedrock API key where the route names one; otherwise the
                // AWS keys the gateway was given, signing each request. Neither
                // is a misconfiguration, as for Anthropic: Bedrock has no
                // unauthenticated mode.
                use crate::llm::provider::bedrock::{Auth, BedrockProvider};
                let auth = match credential {
                    Some(key) => Auth::ApiKey(key),
                    None => match Auth::from_aws_environment(&route.base_url) {
                        Some(signed) => signed,
                        None => {
                            tracing::warn!(
                                base_url = %route.base_url,
                                credential_ref = ?route.credential_ref,
                                "bedrock route has no API key and the gateway no AWS keys, skipping"
                            );
                            return None;
                        }
                    },
                };
                Arc::new(BedrockProvider::new(route.base_url.clone(), auth))
            }
            "gemini" => {
                // Like Anthropic, there is no unauthenticated mode, so a route
                // without a resolvable credential is a misconfiguration.
                let Some(key) = credential else {
                    tracing::warn!(
                        base_url = %route.base_url,
                        credential_ref = ?route.credential_ref,
                        "gemini route has no resolvable credential, skipping"
                    );
                    return None;
                };
                Arc::new(GeminiProvider::new(
                    route.base_url.clone(),
                    key,
                    route.model.clone(),
                ))
            }
            other => {
                tracing::warn!(provider = other, "unknown provider in route, skipping");
                return None;
            }
        };

        built.insert(
            route.account,
            Built {
                provider: Arc::clone(&provider),
                sealed: opened,
            },
        );
        Some(provider)
    }
}

/// An account's sealed key, if its seal lets this workspace use it at this
/// account's host as a provider key -- and only as one. Which of those failed
/// is logged; the caller only learns it has no key.
async fn open_provider_key(
    sealed: &Credentials,
    id: Uuid,
    route: &Route,
    workspace: Uuid,
) -> Option<Arc<Opened>> {
    let opened = match sealed.get(id).await {
        Ok(opened) => opened,
        Err(why) => {
            tracing::warn!(account = %route.account, credential_id = %id, %why, "a provider account's key is not available, skipping");
            return None;
        }
    };
    let host = reqwest::Url::parse(&route.base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))?;
    if let Err(why) = opened
        .binding
        .allows(workspace, &host, None, Kind::Provider)
    {
        tracing::warn!(account = %route.account, credential_id = %id, %why, "a provider account's key is not sealed for this, skipping");
        return None;
    }
    Some(opened)
}

/// A message the user sent while a turn was already running.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Pending {
    /// Which message this was, so the browser can be told it was taken --
    /// and, carried on to the runtime, so the reply can record where in it
    /// the message was handed to the agent.
    pub id: Uuid,
    pub content: String,
    pub delivery: String,
}

/// Takes anything the user has said since this turn began.
///
/// The gateway does this because it is the only tier on the path of every
/// model call that also has a database: the runtime holds no credentials and
/// no pool by design, and giving it one would widen what a sandbox escape
/// reaches. Riding the response it was already sending costs nothing.
///
/// Handed over exactly once. The update marks each message with the reply that
/// took it, so a second call returns nothing and the turn queued for that
/// message knows it has already been answered.
///
/// A message is pending when nothing has replied to it and nothing has
/// absorbed it. The turn's own prompt is excluded automatically, because the
/// reply being written already hangs off it.
pub async fn take_pending(
    pool: &PgPool,
    session_id: Uuid,
    reply_id: Uuid,
) -> Result<Vec<Pending>, sqlx::Error> {
    let rows = sqlx::query(
        "update agent_messages set absorbed_by = $2 \
         where id in ( \
             select u.id from agent_messages u \
             where u.session_id = $1 \
               and u.role = 'user' \
               and u.absorbed_by is null \
               and not exists ( \
                   select 1 from agent_messages r where r.replies_to = u.id \
               ) \
             order by u.id \
             for update skip locked \
         ) \
         returning id, content, delivery",
    )
    .bind(session_id)
    .bind(reply_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| Pending {
            id: r.get("id"),
            content: r.get("content"),
            delivery: r.get("delivery"),
        })
        .collect())
}
