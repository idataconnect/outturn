//! Keeping configured models current with the catalog: a model that tracks
//! "Latest" moves to the newest version in its line when nothing set for it
//! stands in the way, and waits on somebody when something does; a pinned
//! model never moves, and says when it could. See docs/models.md, "Tracking
//! the latest version".
//!
//! Run whenever a catalog is kept, and at startup. Every step is idempotent:
//! a model already where it should be, or already waiting on the same
//! version, is left alone, so running it twice raises nothing twice.

use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use outturn_core::catalog;
use outturn_core::facts::ModelFacts;
use outturn_core::model_settings::{self, PER_MODEL, PLATFORM_WORKSPACE};

use super::actions::{ActionError, ActionStore, NewItem, PostgresActionStore, State, Target};

/// The action item kind for a version waiting on somebody.
pub const KIND: &str = "model.upgrade";

/// One value that approving an upgrade changes, because the new version
/// refuses it.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Adjustment {
    pub workspace_id: Uuid,
    pub agent_id: Uuid,
    pub key: String,
    pub from: Value,
    pub to: Value,
}

/// What a model's upgrade to `entry` would change: each value set for the
/// model, at any level, that the new version refuses, with its nearest
/// valid replacement. Sorted, so two computations of one state compare equal.
async fn adjustments(
    pool: &PgPool,
    model_id: Uuid,
    facts: &ModelFacts,
) -> Result<Vec<Adjustment>, sqlx::Error> {
    let rows = sqlx::query(
        "select workspace_id, agent_id, key, value from setting_overrides \
         where model_id = $1 and key = any($2) \
         order by workspace_id, agent_id, key",
    )
    .bind(model_id)
    .bind(PER_MODEL)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let key: String = r.get("key");
            let from: Value = r.get("value");
            (!model_settings::valid_for(&key, &from, facts)).then(|| Adjustment {
                workspace_id: r.get("workspace_id"),
                agent_id: r.get("agent_id"),
                to: model_settings::adjust(&key, &from, facts),
                key,
                from,
            })
        })
        .collect())
}

/// A model with a catalog link, as the pass reads it.
struct Linked {
    id: Uuid,
    workspace_id: Option<Uuid>,
    name: String,
    model: String,
    provider: String,
    tracks: Option<String>,
    overrides: Value,
    pending_model: Option<String>,
}

/// Recomputes every linked model against the newest catalog. Returns how
/// many moved.
pub async fn track(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let newest: Option<(Uuid, Value)> =
        sqlx::query_as("select id, models from catalog_snapshots order by fetched_at desc limit 1")
            .fetch_optional(pool)
            .await?;
    let Some((snapshot, held)) = newest else {
        return Ok(0);
    };
    let linked: Vec<Linked> = sqlx::query(
        "select id, workspace_id, name, model, catalog_provider, tracks, overrides, pending_model \
         from models where catalog_provider is not null",
    )
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| Linked {
        id: r.get("id"),
        workspace_id: r.get("workspace_id"),
        name: r.get("name"),
        model: r.get("model"),
        provider: r.get("catalog_provider"),
        tracks: r.get("tracks"),
        overrides: r.get("overrides"),
        pending_model: r.get("pending_model"),
    })
    .collect();

    let mut moved = 0;
    for m in linked {
        // A model the catalog no longer has keeps the facts it had, and
        // nothing is proposed for it.
        let Some(next) = catalog::newest_in_line(&held, &m.provider, &m.model) else {
            continue;
        };
        if next == m.model {
            // Whatever was waiting is no longer newer than what it has --
            // withdrawn from the catalog, say -- so nobody is asked about it.
            if m.pending_model.is_some() {
                clear_pending(pool, m.id).await?;
            }
            continue;
        }
        if m.pending_model.as_deref() == Some(next.as_str()) {
            continue;
        }
        let Some(entry) = catalog::entry(&held, &m.provider, &next).cloned() else {
            continue;
        };
        let facts = ModelFacts::of(Some(&entry), Some(&m.overrides));
        let changes = adjustments(pool, m.id, &facts).await?;

        if m.tracks.is_some() && changes.is_empty() {
            if apply(pool, m.id, &m.model, &next, snapshot, &entry, &[]).await? != Applied::Done {
                // Moved by somebody else since it was read; the next pass
                // sees where it is now.
                continue;
            }
            tracing::info!(model = %m.id, from = %m.model, to = %next, "a tracked model moved to the newest in its line");
            moved += 1;
            continue;
        }
        let reason = if m.tracks.is_some() {
            "a value set for this model is one the new version refuses"
        } else {
            "this model is pinned to its version"
        };
        hold(pool, &m, &next, reason, &changes).await?;
    }
    Ok(moved)
}

/// Records a version as waiting, and asks whoever configures the model.
async fn hold(
    pool: &PgPool,
    m: &Linked,
    next: &str,
    reason: &str,
    changes: &[Adjustment],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update models set pending_model = $2, pending_reason = $3, \
             pending_adjustments = $4, pending_since = now() \
         where id = $1",
    )
    .bind(m.id)
    .bind(next)
    .bind(reason)
    .bind(serde_json::to_value(changes).unwrap_or_default())
    .execute(pool)
    .await?;
    // An earlier version that was waiting is not any more.
    settle_items(pool, m.id, State::Cancelled).await?;

    // A workspace's own model is its administrators' to answer; until a
    // workspace can configure one, every linked model is the platform's.
    if m.workspace_id.is_some() {
        return Ok(());
    }
    let operators: Vec<Uuid> =
        sqlx::query_scalar("select user_id from user_system_roles where role = 'system_admin'")
            .fetch_all(pool)
            .await?;
    if operators.is_empty() {
        return Ok(());
    }
    let raised = PostgresActionStore::new(pool.clone())
        .raise(
            PLATFORM_WORKSPACE,
            NewItem {
                kind: KIND.into(),
                event_id: None,
                inhibitor_id: None,
                payload: serde_json::json!({
                    "model_id": m.id,
                    "name": m.name,
                    "from": m.model,
                    "to": next,
                    "reason": reason,
                    "adjustments": changes,
                    "tracked": m.tracks.is_some(),
                }),
                targets: operators.into_iter().map(Target::User).collect(),
                expires_at: None,
            },
        )
        .await;
    if let Err(e) = raised {
        tracing::error!(model = %m.id, error = %e, "could not ask about a model's upgrade");
    }
    Ok(())
}

async fn clear_pending(pool: &PgPool, model_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update models set pending_model = null, pending_reason = null, \
             pending_adjustments = null, pending_since = null \
         where id = $1",
    )
    .bind(model_id)
    .execute(pool)
    .await?;
    settle_items(pool, model_id, State::Cancelled).await
}

/// Withdraws the questions about a model that is going: nobody can answer
/// them once it has.
pub async fn forget(pool: &PgPool, model_id: Uuid) -> Result<(), sqlx::Error> {
    settle_items(pool, model_id, State::Cancelled).await
}

/// Settles the open items about a model's upgrade.
async fn settle_items(pool: &PgPool, model_id: Uuid, state: State) -> Result<(), sqlx::Error> {
    let open: Vec<Uuid> = sqlx::query_scalar(
        // The kind written out, not bound: it is what lets the partial index
        // on open upgrade items (migration 0046) be used.
        "select id from action_items \
         where workspace_id = $1 and kind = 'model.upgrade' and state = 'pending' \
           and payload->>'model_id' = $2",
    )
    .bind(PLATFORM_WORKSPACE)
    .bind(model_id.to_string())
    .fetch_all(pool)
    .await?;
    let store = PostgresActionStore::new(pool.clone());
    for id in open {
        match store.settle(PLATFORM_WORKSPACE, id, state, None).await {
            // Lost to a colleague settling it first is the same outcome.
            Ok(()) | Err(ActionError::NotFound) | Err(ActionError::NotPending(_)) => {}
            // Anything else leaves a question open about a model that has
            // moved on; said, so the stale item has an explanation.
            Err(e) => {
                tracing::error!(model = %model_id, item = %id, error = %e, "could not settle a model's upgrade question")
            }
        }
    }
    Ok(())
}

/// How an `apply` went.
#[derive(Debug, PartialEq)]
enum Applied {
    Done,
    /// The model is no longer on the version that was read.
    ModelMoved,
    /// A value to adjust is no longer the one that was shown.
    ValueChanged,
}

/// Moves a model to `next`, with its facts, and makes the adjustments, in
/// one step: never a new version with values it refuses, never adjusted
/// values on the old one.
///
/// Only from the version it was read on, and only over the values that
/// were shown: two approvals at once, or an approval beside a tracking pass,
/// would otherwise move a model twice -- losing the version a running turn
/// began on -- and an adjustment would overwrite a value somebody set since.
/// Either way nothing is changed, and the caller is told which.
async fn apply(
    pool: &PgPool,
    model_id: Uuid,
    from: &str,
    next: &str,
    snapshot: Uuid,
    entry: &Value,
    changes: &[Adjustment],
) -> Result<Applied, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let on: Option<String> =
        sqlx::query_scalar("select model from models where id = $1 for update")
            .bind(model_id)
            .fetch_optional(&mut *tx)
            .await?;
    if on.as_deref() != Some(from) {
        return Ok(Applied::ModelMoved);
    }
    for change in changes {
        let done = sqlx::query(
            "update setting_overrides set value = $5, updated_at = now() \
             where workspace_id = $1 and agent_id = $2 and model_id = $3 and key = $4 \
               and value = $6",
        )
        .bind(change.workspace_id)
        .bind(change.agent_id)
        .bind(model_id)
        .bind(&change.key)
        .bind(&change.to)
        .bind(&change.from)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(Applied::ValueChanged);
        }
    }
    // The name follows the version where it was the catalog's name for the
    // old one; a name somebody chose stays.
    sqlx::query(
        "update models set previous_model = model, previous_facts = facts, changed_at = now(), \
             model = $2, catalog_model = $2, snapshot_id = $3, facts = $4, \
             name = case when name = facts->>'name' then coalesce($4->>'name', name) else name end, \
             pending_model = null, pending_reason = null, \
             pending_adjustments = null, pending_since = null \
         where id = $1",
    )
    .bind(model_id)
    .bind(next)
    .bind(snapshot)
    .bind(entry)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    settle_items(pool, model_id, State::Resolved).await?;
    Ok(Applied::Done)
}

#[derive(Debug, thiserror::Error)]
pub enum UpgradeError {
    #[error("no such model")]
    NotFound,
    #[error("nothing is waiting for this model")]
    NothingPending,
    #[error("the version waiting is no longer in the catalog")]
    Withdrawn,
    #[error("the values this would change have changed since it was proposed; look again")]
    Changed,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Approves the version waiting for a platform model: moves to it with the
/// adjustments proposed. Refused if what it would change is not what was
/// shown, so nobody approves one thing and gets another.
pub async fn approve(pool: &PgPool, model_id: Uuid) -> Result<String, UpgradeError> {
    let row = sqlx::query(
        "select model, catalog_provider, overrides, pending_model, pending_adjustments \
         from models where id = $1 and workspace_id is null",
    )
    .bind(model_id)
    .fetch_optional(pool)
    .await?
    .ok_or(UpgradeError::NotFound)?;
    let provider: Option<String> = row.get("catalog_provider");
    let overrides: Value = row.get("overrides");
    let from: String = row.get("model");
    let (Some(provider), Some(next)) = (provider, row.get::<Option<String>, _>("pending_model"))
    else {
        return Err(UpgradeError::NothingPending);
    };
    let shown: Vec<Adjustment> = row
        .get::<Option<Value>, _>("pending_adjustments")
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let (snapshot, held): (Uuid, Value) =
        sqlx::query_as("select id, models from catalog_snapshots order by fetched_at desc limit 1")
            .fetch_one(pool)
            .await?;
    let Some(entry) = catalog::entry(&held, &provider, &next).cloned() else {
        clear_pending(pool, model_id).await?;
        return Err(UpgradeError::Withdrawn);
    };
    let facts = ModelFacts::of(Some(&entry), Some(&overrides));
    let now = adjustments(pool, model_id, &facts).await?;
    if now != shown {
        sqlx::query("update models set pending_adjustments = $2 where id = $1")
            .bind(model_id)
            .bind(serde_json::to_value(&now).unwrap_or_default())
            .execute(pool)
            .await?;
        return Err(UpgradeError::Changed);
    }
    match apply(pool, model_id, &from, &next, snapshot, &entry, &now).await? {
        Applied::Done => Ok(next),
        Applied::ModelMoved => Err(UpgradeError::NothingPending),
        Applied::ValueChanged => Err(UpgradeError::Changed),
    }
}
