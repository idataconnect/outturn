//! A class of traffic's chain: the models it walks, in order, each at its
//! provider account (docs/models.md). Read by the gateway, which walks it,
//! and by the API, which sizes a turn's conversation for every model on it.

use std::sync::Arc;

use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::facts::ModelFacts;
use crate::protocol::{BACKGROUND_TRAFFIC_TYPE, DEFAULT_TRAFFIC_TYPE};

/// One link: a model, and the account it is asked at.
#[derive(Debug, Clone)]
pub struct Link {
    pub model_id: Uuid,
    pub model: String,
    pub facts: Arc<ModelFacts>,
    pub account: Uuid,
    pub protocol: String,
    pub base_url: String,
    pub credential_env: Option<String>,
    pub credential_id: Option<Uuid>,
    /// What the model asked for before it last moved, with its facts, and
    /// when it moved: a turn begun before then is finished on it.
    pub previous: Option<(String, Arc<ModelFacts>)>,
    pub changed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Link {
    /// The version a turn that began at `began` is asked of. A version
    /// changes between turns, never between a turn's rounds: a thinking
    /// block is valid only for the model that wrote it, and the cache is per
    /// model, so a turn that crossed versions would lose both partway.
    pub fn for_turn_begun(mut self, began: Option<chrono::DateTime<chrono::Utc>>) -> Self {
        if let (Some(began), Some(changed), Some(_)) = (began, self.changed_at, &self.previous)
            && began < changed
            && let Some((model, facts)) = self.previous.take()
        {
            self.model = model;
            self.facts = facts;
        }
        self
    }
}

/// The class to borrow a chain from when this one has none.
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

/// A traffic type's links, in the order to try them.
///
/// A workspace's own chain replaces the platform's rather than extending it:
/// a workspace that has configured where its traffic goes should not have
/// requests quietly fall through to somebody else's endpoint.
pub async fn links_for(
    pool: &PgPool,
    workspace_id: Uuid,
    traffic_type: &str,
) -> Result<Vec<Link>, sqlx::Error> {
    let links = links_of(pool, workspace_id, traffic_type).await?;
    match borrows_from(traffic_type) {
        Some(parent) if links.is_empty() => links_of(pool, workspace_id, parent).await,
        _ => Ok(links),
    }
}

/// One class's own chain, with no borrowing.
async fn links_of(
    pool: &PgPool,
    workspace_id: Uuid,
    traffic_type: &str,
) -> Result<Vec<Link>, sqlx::Error> {
    let rows = sqlx::query(
        "select m.id as model_id, m.model, m.facts, m.overrides, m.previous_model, \
                m.previous_facts, m.changed_at, a.id as account, \
                a.protocol, a.base_url, a.credential_env, a.credential_id \
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
            let previous_facts: Option<serde_json::Value> = r.get("previous_facts");
            let previous = r.get::<Option<String>, _>("previous_model").map(|model| {
                let facts = ModelFacts::of(previous_facts.as_ref(), Some(&overrides));
                (model, Arc::new(facts))
            });
            Link {
                previous,
                changed_at: r.get("changed_at"),
                model_id: r.get("model_id"),
                model: r.get("model"),
                facts: Arc::new(ModelFacts::of(facts.as_ref(), Some(&overrides))),
                account: r.get("account"),
                protocol: r.get("protocol"),
                base_url: r.get("base_url"),
                credential_env: r.get("credential_env"),
                credential_id: r.get("credential_id"),
            }
        })
        .collect())
}
