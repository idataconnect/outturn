//! Resolving a class of traffic to the providers that serve it.
//!
//! The caller names what the work is for -- "assistant" for a user waiting on
//! a reply -- and the gateway decides where it goes. That division exists
//! because only the gateway knows which endpoints are currently healthy, and
//! because a caller that named a provider directly could not be failed over
//! without lying to it about what it asked for.
//!
//! A route also carries the model, so switching a class of traffic to a
//! cheaper model is configuration rather than a deployment.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::llm::provider::{LlmProvider, anthropic::AnthropicProvider, openai::OpenAiProvider};

/// The class of work a request belongs to, when the caller does not say.
pub const DEFAULT_TRAFFIC_TYPE: &str = "assistant";

/// One attempt: where to go, and what to ask for.
#[derive(Debug, Clone)]
pub struct Route {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub credential_ref: Option<String>,
}

impl Route {
    /// Matches the key the circuit breaker records health against.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.provider, self.base_url)
    }
}

/// Routes for a traffic type, in the order to try them.
///
/// A tenant's own routes replace the system defaults rather than extending
/// them: a tenant that has configured where its traffic goes should not have
/// requests quietly fall through to somebody else's endpoint.
pub async fn routes_for(
    pool: &PgPool,
    tenant_id: Uuid,
    traffic_type: &str,
) -> Result<Vec<Route>, sqlx::Error> {
    let rows = sqlx::query(
        "select provider, base_url, model, credential_ref \
         from traffic_routes \
         where enabled and traffic_type = $2 \
           and tenant_id is not distinct from ( \
               select case when exists ( \
                   select 1 from traffic_routes \
                   where enabled and tenant_id = $1 and traffic_type = $2 \
               ) then $1 else null end \
           ) \
         order by priority",
    )
    .bind(tenant_id)
    .bind(traffic_type)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| Route {
            provider: r.get("provider"),
            base_url: r.get("base_url"),
            model: r.get("model"),
            credential_ref: r.get("credential_ref"),
        })
        .collect())
}

/// Builds providers from routes, keeping one per endpoint.
///
/// Providers own a connection pool, so constructing one per request would
/// throw away keep-alive and make every call pay a fresh TLS handshake. They
/// are keyed by endpoint rather than by route because two routes differing
/// only in model share a connection perfectly well.
#[derive(Default)]
pub struct ProviderCache {
    built: tokio::sync::Mutex<HashMap<String, Arc<dyn LlmProvider>>>,
}

impl ProviderCache {
    pub async fn get(&self, route: &Route) -> Option<Arc<dyn LlmProvider>> {
        let key = route.endpoint();
        let mut built = self.built.lock().await;

        if let Some(existing) = built.get(&key) {
            return Some(Arc::clone(existing));
        }

        // Resolved from the environment at construction, so the secret is
        // never in the database and never in a log line.
        let credential = route
            .credential_ref
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
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
            other => {
                tracing::warn!(provider = other, "unknown provider in route, skipping");
                return None;
            }
        };

        built.insert(key, Arc::clone(&provider));
        Some(provider)
    }
}
