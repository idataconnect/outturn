//! The platform's models: provider accounts, the models at them, the chains
//! they are walked in, and which chain each class of traffic takes. See
//! docs/models.md. The operator's alone; a workspace's own come with the
//! decision about who may configure them.
//!
//! The gateway reads all of this on every request, so what is checked here is
//! what it can assume: platform rows reference platform rows, a URL is a URL,
//! a key is sealed as a provider key for its account's host.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use outturn_core::catalog;
use outturn_core::egress::seal::Kind;
use outturn_core::model_settings::PLATFORM_WORKSPACE;

use super::router::{ApiError, ApiState};

const PROTOCOLS: &[&str] = &["openai", "anthropic", "gemini", "bedrock"];

fn operator(state: &ApiState, headers: &axum::http::HeaderMap) -> Result<Uuid, ApiError> {
    let claims = super::router::authenticate(state, headers)?;
    if !claims.is_system_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator configures the platform's models".into(),
        ));
    }
    Ok(claims.subject)
}

fn internal(e: sqlx::Error) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// A delete refused by a foreign key: something still uses the row.
fn in_use(what: &str) -> impl Fn(sqlx::Error) -> ApiError + '_ {
    move |e| match e.as_database_error().and_then(|d| d.code()) {
        Some(code) if code == "23001" || code == "23503" => {
            (StatusCode::CONFLICT, format!("this {what} is still in use"))
        }
        _ => internal(e),
    }
}

fn not_found(what: &str) -> ApiError {
    (StatusCode::NOT_FOUND, format!("no such {what}"))
}

fn host_of(base_url: &str) -> Result<String, ApiError> {
    let url = reqwest::Url::parse(base_url)
        .map_err(|_| (StatusCode::BAD_REQUEST, format!("{base_url} is not a URL")))?;
    if !["http", "https"].contains(&url.scheme()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("{base_url} is not http or https"),
        ));
    }
    url.host_str()
        .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
        .ok_or((StatusCode::BAD_REQUEST, format!("{base_url} names no host")))
}

// -- Reading -------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub protocol: String,
    pub base_url: String,
    pub credential_env: Option<String>,
    /// The sealed key's id, never anything of the key.
    pub credential_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct Model {
    pub id: Uuid,
    pub account_id: Uuid,
    pub name: String,
    pub model: String,
    pub catalog_provider: Option<String>,
    pub catalog_model: Option<String>,
    pub facts: Option<serde_json::Value>,
    pub overrides: serde_json::Value,
    /// The family it tracks, for "Latest"; none when pinned.
    pub tracks: Option<String>,
    /// A newer version waiting on somebody, why, and what approving it would
    /// change.
    pub pending_model: Option<String>,
    pub pending_reason: Option<String>,
    pub pending_adjustments: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Chain {
    pub id: Uuid,
    pub name: String,
    /// Model ids, in the order they are tried.
    pub models: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct Configured {
    pub accounts: Vec<Account>,
    pub models: Vec<Model>,
    pub chains: Vec<Chain>,
    /// Traffic class to chain id.
    pub assignments: std::collections::BTreeMap<String, Uuid>,
}

/// Everything the platform has configured, in one read for one page.
pub async fn view(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Configured>, ApiError> {
    operator(&state, &headers)?;
    let pool = &state.pool;
    let accounts = sqlx::query(
        "select id, name, protocol, base_url, credential_env, credential_id \
         from provider_accounts where workspace_id is null order by id",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?
    .iter()
    .map(|r| Account {
        id: r.get("id"),
        name: r.get("name"),
        protocol: r.get("protocol"),
        base_url: r.get("base_url"),
        credential_env: r.get("credential_env"),
        credential_id: r.get("credential_id"),
    })
    .collect();
    let models = sqlx::query(
        "select id, account_id, name, model, catalog_provider, catalog_model, facts, overrides, \
                tracks, pending_model, pending_reason, pending_adjustments \
         from models where workspace_id is null order by id",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?
    .iter()
    .map(|r| Model {
        id: r.get("id"),
        account_id: r.get("account_id"),
        name: r.get("name"),
        model: r.get("model"),
        catalog_provider: r.get("catalog_provider"),
        catalog_model: r.get("catalog_model"),
        facts: r.get("facts"),
        overrides: r.get("overrides"),
        tracks: r.get("tracks"),
        pending_model: r.get("pending_model"),
        pending_reason: r.get("pending_reason"),
        pending_adjustments: r.get("pending_adjustments"),
    })
    .collect();
    let chains = sqlx::query(
        "select c.id, c.name, \
                coalesce(array_agg(l.model_id order by l.position) \
                         filter (where l.model_id is not null), '{}') as models \
         from chains c left join chain_links l on l.chain_id = c.id \
         where c.workspace_id is null group by c.id order by c.id",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?
    .iter()
    .map(|r| Chain {
        id: r.get("id"),
        name: r.get("name"),
        models: r.get("models"),
    })
    .collect();
    let assignments = sqlx::query(
        "select traffic_type, chain_id from chain_assignments where workspace_id is null",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?
    .iter()
    .map(|r| (r.get("traffic_type"), r.get("chain_id")))
    .collect();
    Ok(Json(Configured {
        accounts,
        models,
        chains,
        assignments,
    }))
}

// -- Accounts ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NewAccount {
    pub name: String,
    pub protocol: String,
    pub base_url: String,
    /// A variable in the gateway's environment holding the key. Leave it out
    /// for an endpoint that needs none, or to seal one afterwards.
    #[serde(default)]
    pub credential_env: Option<String>,
}

pub async fn create_account(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<NewAccount>,
) -> Result<(StatusCode, Json<Account>), ApiError> {
    let actor = operator(&state, &headers)?;
    if !PROTOCOLS.contains(&input.protocol.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "{} is not a protocol: {}",
                input.protocol,
                PROTOCOLS.join(", ")
            ),
        ));
    }
    host_of(&input.base_url)?;
    let env = input
        .credential_env
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into provider_accounts (id, workspace_id, name, protocol, base_url, credential_env) \
         values ($1, null, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(&input.name)
    .bind(&input.protocol)
    .bind(&input.base_url)
    .bind(&env)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    tracing::info!(%actor, account = %id, protocol = %input.protocol, "provider account created");
    Ok((
        StatusCode::CREATED,
        Json(Account {
            id,
            name: input.name,
            protocol: input.protocol,
            base_url: input.base_url,
            credential_env: env,
            credential_id: None,
        }),
    ))
}

/// Seals an account's key, or rotates it. The seal must be a provider key,
/// for every workspace -- the platform's accounts serve them all -- and for
/// exactly the account's host, so the key can go nowhere else.
pub async fn seal_account_key(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(account): Path<Uuid>,
    Json(input): Json<super::credentials::NewCredential>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let row = sqlx::query(
        "select base_url, credential_id from provider_accounts \
         where id = $1 and workspace_id is null",
    )
    .bind(account)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(|| not_found("account"))?;
    let base_url: String = row.get("base_url");
    let existing: Option<Uuid> = row.get("credential_id");
    if existing.is_some_and(|id| id != input.id) {
        return Err(bad(
            "this account's key is rotated under its own credential id".into(),
        ));
    }
    let (binding, parsed, sealed) = super::credentials::decode(&state, input.id, &input.seal)?;
    if parsed.kind != Kind::Provider {
        return Err(bad("an account's key is sealed as a provider key".into()));
    }
    if parsed.workspaces != ["*"] {
        return Err(bad(
            "a platform account serves every workspace, so its key is bound to \"*\"".into(),
        ));
    }
    let host = host_of(&base_url)?;
    if parsed.hosts != [host.clone()] {
        return Err(bad(format!(
            "an account's key is bound to its host, {host}, alone"
        )));
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    let written = sqlx::query(
        "insert into credentials (id, workspace_id, name, binding, sealed, key_id, created_by) \
         values ($1, null, $2, $3, $4, $5, $6) \
         on conflict (id) do update set binding = $3, sealed = $4, key_id = $5, \
             generation = credentials.generation + 1, updated_at = now(), revoked_at = null \
         where credentials.workspace_id is null",
    )
    .bind(input.id)
    .bind(&input.name)
    .bind(&binding)
    .bind(&sealed)
    .bind(&input.seal.key_id)
    .bind(actor)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    // An id that is a workspace's credential writes nothing, and the account
    // must not be pointed at it: its key is not the platform's to use.
    if written.rows_affected() != 1 {
        return Err((
            StatusCode::CONFLICT,
            "that credential id belongs to a workspace; seal the key under a new one".into(),
        ));
    }
    // The environment variable goes: an account has one key or the other.
    sqlx::query(
        "update provider_accounts set credential_id = $2, credential_env = null where id = $1",
    )
    .bind(account)
    .bind(input.id)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    tracing::info!(%actor, %account, credential_id = %input.id, "provider account key sealed");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_account(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    let mut tx = state.pool.begin().await.map_err(internal)?;
    let key: Option<Option<Uuid>> = sqlx::query_scalar(
        "delete from provider_accounts where id = $1 and workspace_id is null \
         returning credential_id",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(in_use("account"))?;
    let Some(key) = key else {
        return Err(not_found("account"));
    };
    // Its key had no other use, so it is revoked rather than left to open.
    if let Some(key) = key {
        sqlx::query(
            "update credentials set sealed = null, revoked_at = now(), \
                 generation = generation + 1 \
             where id = $1 and workspace_id is null and revoked_at is null",
        )
        .bind(key)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    }
    tx.commit().await.map_err(internal)?;
    tracing::info!(%actor, account = %id, "provider account deleted");
    Ok(StatusCode::NO_CONTENT)
}

// -- Models --------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NewModel {
    pub account_id: Uuid,
    /// What the account is asked for.
    pub model: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Typed facts, for a model the catalog does not have. Left out, the
    /// newest catalog's entry is used, and a model it does not have is
    /// refused rather than saved knowing nothing.
    #[serde(default)]
    pub facts: Option<serde_json::Value>,
    #[serde(default)]
    pub overrides: Option<serde_json::Value>,
    /// Track "Latest" in this model's line rather than pin it: it is
    /// configured as the newest there now, and moves when a newer one comes.
    #[serde(default)]
    pub tracks: bool,
}

pub async fn create_model(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<NewModel>,
) -> Result<(StatusCode, Json<Model>), ApiError> {
    let actor = operator(&state, &headers)?;
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let overrides = input.overrides.unwrap_or_else(|| serde_json::json!({}));
    if !overrides.is_object() {
        return Err(bad("overrides is an object of catalog fields".into()));
    }
    let row = sqlx::query(
        "select protocol, base_url from provider_accounts where id = $1 and workspace_id is null",
    )
    .bind(input.account_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(|| not_found("account"))?;
    let protocol: String = row.get("protocol");
    let base_url: String = row.get("base_url");

    if input.tracks && input.facts.is_some() {
        return Err(bad(
            "a model described by hand has no line in the catalog to track".into(),
        ));
    }
    let mut asked = input.model.clone();
    let mut tracks = None;
    let (link, facts) = match input.facts {
        Some(facts) if facts.is_object() => (None, Some(facts)),
        Some(_) => return Err(bad("facts is an object, shaped as a catalog entry".into())),
        None => {
            let newest: Option<(Uuid, serde_json::Value)> = sqlx::query_as(
                "select id, models from catalog_snapshots order by fetched_at desc limit 1",
            )
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?;
            let found = newest.and_then(|(snapshot, held)| {
                let provider = catalog::provider_for(&held, &protocol, &base_url)?;
                catalog::entry(&held, &provider, &input.model)?;
                if input.tracks {
                    asked = catalog::newest_in_line(&held, &provider, &input.model)?;
                }
                let entry = catalog::entry(&held, &provider, &asked)?.clone();
                Some(((provider, snapshot), entry))
            });
            match found {
                Some((link, entry)) => (Some(link), Some(entry)),
                None => {
                    return Err(bad(format!(
                        "the catalog has no {} at this account; describe it with facts",
                        input.model
                    )));
                }
            }
        }
    };
    if input.tracks {
        tracks = facts
            .as_ref()
            .and_then(|f| f["family"].as_str().map(str::to_string));
        if tracks.is_none() {
            return Err(bad(format!(
                "the catalog gives {} no family to track",
                input.model
            )));
        }
    }
    let name = input
        .name
        .filter(|n| !n.trim().is_empty())
        .or_else(|| facts.as_ref()?["name"].as_str().map(str::to_string))
        .unwrap_or_else(|| asked.clone());
    let id = Uuid::now_v7();
    let (catalog_provider, snapshot) = link.map_or((None, None), |(p, s)| (Some(p), Some(s)));
    let catalog_model = catalog_provider.as_ref().map(|_| asked.clone());
    sqlx::query(
        "insert into models (id, workspace_id, account_id, name, model, catalog_provider, \
             catalog_model, snapshot_id, facts, overrides, tracks) \
         values ($1, null, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(input.account_id)
    .bind(&name)
    .bind(&asked)
    .bind(&catalog_provider)
    .bind(&catalog_model)
    .bind(snapshot)
    .bind(&facts)
    .bind(&overrides)
    .bind(&tracks)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    tracing::info!(%actor, model = %id, name = %asked, tracking = tracks.is_some(), "model configured");
    Ok((
        StatusCode::CREATED,
        Json(Model {
            id,
            account_id: input.account_id,
            name,
            model: asked,
            catalog_provider,
            catalog_model,
            facts,
            overrides,
            tracks,
            pending_model: None,
            pending_reason: None,
            pending_adjustments: None,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ModelChange {
    #[serde(default)]
    pub name: Option<String>,
    /// Replaces the overrides whole; `{}` clears them.
    #[serde(default)]
    pub overrides: Option<serde_json::Value>,
    /// Track "Latest" (true) or pin to the version it is on (false).
    #[serde(default)]
    pub tracks: Option<bool>,
}

pub async fn update_model(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<ModelChange>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    if input.overrides.as_ref().is_some_and(|o| !o.is_object()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "overrides is an object of catalog fields".into(),
        ));
    }
    // Tracking takes the family from the facts it already has; a model
    // entered by hand has none, and is refused rather than tracking nothing.
    let done = sqlx::query(
        "update models set name = coalesce($2, name), overrides = coalesce($3, overrides), \
             tracks = case $4::bool when true then facts->>'family' when false then null \
                      else tracks end \
         where id = $1 and workspace_id is null \
           and ($4::bool is not true or (catalog_provider is not null and facts ? 'family'))",
    )
    .bind(id)
    .bind(input.name.filter(|n| !n.trim().is_empty()))
    .bind(input.overrides)
    .bind(input.tracks)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    if done.rows_affected() == 0 {
        return Err(if input.tracks == Some(true) {
            (
                StatusCode::BAD_REQUEST,
                "no such catalog model to track".to_string(),
            )
        } else {
            not_found("model")
        });
    }
    // Turning tracking on or off changes what should be waiting.
    if input.tracks.is_some()
        && let Err(e) = super::tracking::track(&state.pool).await
    {
        tracing::error!(error = %e, "could not bring tracked models up to date");
    }
    tracing::info!(%actor, model = %id, "model changed");
    Ok(StatusCode::NO_CONTENT)
}

/// Approves the version waiting for a model: it moves there, with the
/// adjustments that were proposed. 409 when what it would change is no
/// longer what was proposed; the new proposal is then in the model's view.
pub async fn upgrade_model(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use super::tracking::UpgradeError;
    let actor = operator(&state, &headers)?;
    match super::tracking::approve(&state.pool, id).await {
        Ok(to) => {
            tracing::info!(%actor, model = %id, %to, "model upgrade approved");
            Ok(Json(serde_json::json!({ "model": to })))
        }
        Err(UpgradeError::NotFound) => Err(not_found("model")),
        Err(
            e @ (UpgradeError::NothingPending | UpgradeError::Withdrawn | UpgradeError::Changed),
        ) => Err((StatusCode::CONFLICT, e.to_string())),
        Err(UpgradeError::Database(e)) => Err(internal(e)),
    }
}

pub async fn delete_model(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    let done = sqlx::query("delete from models where id = $1 and workspace_id is null")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(in_use("model"))?;
    if done.rows_affected() == 0 {
        return Err(not_found("model"));
    }
    if let Err(e) = super::tracking::forget(&state.pool, id).await {
        tracing::error!(model = %id, error = %e, "could not withdraw a deleted model's questions");
    }
    tracing::info!(%actor, model = %id, "model deleted");
    Ok(StatusCode::NO_CONTENT)
}

// -- Chains --------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ChainInput {
    pub name: String,
    /// Model ids, in the order they are tried.
    pub models: Vec<Uuid>,
}

/// Writes a chain's links, after checking each is a platform model.
async fn write_links(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    chain: Uuid,
    models: &[Uuid],
) -> Result<(), ApiError> {
    if models.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "a chain has at least one model".into(),
        ));
    }
    let known: i64 = sqlx::query_scalar(
        "select count(*) from models where id = any($1) and workspace_id is null",
    )
    .bind(models)
    .fetch_one(&mut **tx)
    .await
    .map_err(internal)?;
    let distinct: std::collections::HashSet<_> = models.iter().collect();
    if known as usize != distinct.len() {
        return Err((
            StatusCode::BAD_REQUEST,
            "a chain names only the platform's own models".into(),
        ));
    }
    sqlx::query("delete from chain_links where chain_id = $1")
        .bind(chain)
        .execute(&mut **tx)
        .await
        .map_err(internal)?;
    sqlx::query(
        "insert into chain_links (chain_id, position, model_id) \
         select $1, (n - 1)::int, m from unnest($2::uuid[]) with ordinality as t(m, n)",
    )
    .bind(chain)
    .bind(models)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    Ok(())
}

pub async fn create_chain(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<ChainInput>,
) -> Result<(StatusCode, Json<Chain>), ApiError> {
    let actor = operator(&state, &headers)?;
    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await.map_err(internal)?;
    sqlx::query("insert into chains (id, workspace_id, name) values ($1, null, $2)")
        .bind(id)
        .bind(&input.name)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    write_links(&mut tx, id, &input.models).await?;
    tx.commit().await.map_err(internal)?;
    tracing::info!(%actor, chain = %id, "chain created");
    Ok((
        StatusCode::CREATED,
        Json(Chain {
            id,
            name: input.name,
            models: input.models,
        }),
    ))
}

pub async fn update_chain(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<ChainInput>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    let mut tx = state.pool.begin().await.map_err(internal)?;
    let done = sqlx::query("update chains set name = $2 where id = $1 and workspace_id is null")
        .bind(id)
        .bind(&input.name)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    if done.rows_affected() == 0 {
        return Err(not_found("chain"));
    }
    write_links(&mut tx, id, &input.models).await?;
    tx.commit().await.map_err(internal)?;
    tracing::info!(%actor, chain = %id, "chain changed");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_chain(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    let done = sqlx::query("delete from chains where id = $1 and workspace_id is null")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(in_use("chain"))?;
    if done.rows_affected() == 0 {
        return Err(not_found("chain"));
    }
    tracing::info!(%actor, chain = %id, "chain deleted");
    Ok(StatusCode::NO_CONTENT)
}

// -- Assignments ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Assignment {
    pub chain_id: Uuid,
}

/// Which chain a class of traffic walks, for every workspace without its own.
pub async fn assign(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(traffic_type): Path<String>,
    Json(input): Json<Assignment>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    let ours: Option<i64> =
        sqlx::query_scalar("select 1::bigint from chains where id = $1 and workspace_id is null")
            .bind(input.chain_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?;
    if ours.is_none() {
        return Err(not_found("chain"));
    }
    sqlx::query(
        "insert into chain_assignments (id, workspace_id, traffic_type, chain_id) \
         values ($1, null, $2, $3) \
         on conflict (traffic_type) where workspace_id is null do update set chain_id = $3",
    )
    .bind(Uuid::now_v7())
    .bind(&traffic_type)
    .bind(input.chain_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    tracing::info!(%actor, %traffic_type, chain = %input.chain_id, "traffic class assigned");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unassign(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(traffic_type): Path<String>,
) -> Result<StatusCode, ApiError> {
    let actor = operator(&state, &headers)?;
    let done = sqlx::query(
        "delete from chain_assignments where workspace_id is null and traffic_type = $1",
    )
    .bind(&traffic_type)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    if done.rows_affected() == 0 {
        return Err(not_found("assignment"));
    }
    tracing::info!(%actor, %traffic_type, "traffic class unassigned");
    Ok(StatusCode::NO_CONTENT)
}

// -- What a workspace sees -----------------------------------------------------

/// A model as a workspace sees it: enough to choose settings for it, and
/// nothing about the account behind it or its key.
#[derive(Debug, Serialize)]
pub struct Offered {
    pub id: Uuid,
    pub name: String,
    pub model: String,
    pub facts: Option<serde_json::Value>,
}

/// The models a workspace may set values for: the platform's, and its own.
pub async fn offered(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Offered>>, ApiError> {
    let claims = super::router::authorize(
        &state,
        &headers,
        outturn_core::auth::Authority::SettingsRead,
    )
    .await?;
    let rows = sqlx::query(
        "select id, name, model, facts from models \
         where workspace_id is null or workspace_id = $1 order by name, id",
    )
    .bind(claims.workspace_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(Json(
        rows.iter()
            .map(|r| Offered {
                id: r.get("id"),
                name: r.get("name"),
                model: r.get("model"),
                facts: r.get("facts"),
            })
            .collect(),
    ))
}

// -- For the configuration page ------------------------------------------------

/// The gateway's sealing key, for the operator sealing an account's key in
/// the browser. As `/v1/credentials/seal-key`, without a workspace's
/// authority: the operator's keys are the platform's.
pub async fn seal_key(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<super::credentials::SealKey>, ApiError> {
    operator(&state, &headers)?;
    super::credentials::seal_key_of(&state).map(Json)
}

/// One model the catalog has at an account's provider, for choosing one.
#[derive(Debug, Serialize)]
pub struct Listed {
    pub id: String,
    pub name: Option<String>,
    pub family: Option<String>,
    pub release_date: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AccountCatalog {
    /// The catalog's provider for the account, or none: an endpoint the
    /// catalog does not know, whose models are entered by hand.
    pub provider: Option<String>,
    /// Newest first.
    pub models: Vec<Listed>,
}

pub async fn account_catalog(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(account): Path<Uuid>,
) -> Result<Json<AccountCatalog>, ApiError> {
    operator(&state, &headers)?;
    let row = sqlx::query(
        "select protocol, base_url from provider_accounts where id = $1 and workspace_id is null",
    )
    .bind(account)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or_else(|| not_found("account"))?;
    let protocol: String = row.get("protocol");
    let base_url: String = row.get("base_url");
    let held: Option<serde_json::Value> =
        sqlx::query_scalar("select models from catalog_snapshots order by fetched_at desc limit 1")
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?;
    let Some(held) = held else {
        return Ok(Json(AccountCatalog {
            provider: None,
            models: Vec::new(),
        }));
    };
    let provider = catalog::provider_for(&held, &protocol, &base_url);
    let text = |m: &serde_json::Value, k: &str| m[k].as_str().map(str::to_string);
    let mut models: Vec<Listed> = provider
        .as_deref()
        .and_then(|p| held[p]["models"].as_object())
        .into_iter()
        .flatten()
        .map(|(id, m)| Listed {
            id: id.clone(),
            name: text(m, "name"),
            family: text(m, "family"),
            release_date: text(m, "release_date"),
            status: text(m, "status"),
        })
        .collect();
    models.sort_by(|a, b| b.release_date.cmp(&a.release_date).then(a.id.cmp(&b.id)));
    Ok(Json(AccountCatalog { provider, models }))
}

// -- A test call ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TestCall {
    /// The class of traffic to send it as; `assistant` when left out.
    #[serde(default)]
    pub traffic_type: Option<String>,
}

/// What answered a test call, or why nothing did.
#[derive(Debug, Serialize)]
pub struct Tested {
    pub ok: bool,
    /// The model that answered, as the gateway reported it.
    pub model: Option<String>,
    /// The endpoint it was reached at.
    pub endpoint: Option<String>,
    pub reply: Option<String>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
}

/// One short request through the gateway, as the platform's own traffic of a
/// class: whether the chain that class walks answers, and which model did.
/// Spends a few tokens of whatever the chain's first working model costs.
pub async fn test_call(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<TestCall>,
) -> Result<Json<Tested>, ApiError> {
    use outturn_core::llm::{ChatCompletionRequest, Message, MessageContent, Role};
    operator(&state, &headers)?;
    let traffic = input
        .traffic_type
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| outturn_core::protocol::DEFAULT_TRAFFIC_TYPE.to_string());
    let gateway = std::env::var("OUTTURN_GATEWAY_URL").map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "this deployment names no gateway (OUTTURN_GATEWAY_URL)".to_string(),
        )
    })?;
    // A turn of nobody's: a session id that names no conversation, in the
    // platform's workspace, reaching the model and nothing else.
    let token = state
        .minter
        .mint_turn(
            Uuid::now_v7(),
            PLATFORM_WORKSPACE,
            outturn_core::egress::commit::empty_root(),
            outturn_core::egress::gate::Gates::none().root(PLATFORM_WORKSPACE),
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let request = ChatCompletionRequest {
        model: String::new(),
        messages: vec![Message {
            role: Role::User,
            content: MessageContent::Text(
                "Reply with a short greeting, five words at most.".into(),
            ),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: Vec::new(),
            signed_by: None,
        }],
        temperature: None,
        max_tokens: Some(256),
        tools: None,
        reasoning_effort: Some("none".into()),
        stream: false,
        stream_options: None,
        facts: None,
    };
    let started = std::time::Instant::now();
    let sent = outturn_core::http_client::reporting_client()
        .post(format!(
            "{}/v1/chat/completions",
            gateway.trim_end_matches('/')
        ))
        .bearer_auth(&token)
        .header(outturn_core::protocol::TRAFFIC_HEADER, &traffic)
        .timeout(std::time::Duration::from_secs(120))
        .json(&request)
        .send()
        .await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let failed = |error: String| Tested {
        ok: false,
        model: None,
        endpoint: None,
        reply: None,
        error: Some(error),
        elapsed_ms,
    };
    let response = match sent {
        Ok(r) => r,
        Err(e) => {
            return Ok(Json(failed(format!(
                "the gateway could not be reached: {e}"
            ))));
        }
    };
    let endpoint = response
        .headers()
        .get("x-outturn-provider")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let status = response.status();
    let body = match response.text().await {
        Ok(body) => body,
        Err(e) => return Ok(Json(failed(format!("the answer could not be read: {e}")))),
    };
    if !status.is_success() {
        return Ok(Json(failed(body.chars().take(500).collect())));
    }
    // A pass is a reply: an answer that is not one -- a proxy's page, an
    // empty body -- is a failure, said with what came instead.
    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return Ok(Json(failed(format!(
                "the answer was not a model's reply: {}",
                body.chars().take(200).collect::<String>()
            ))));
        }
    };
    let Some(reply) = parsed["choices"][0]["message"]["content"].as_str() else {
        return Ok(Json(failed("the answer held no reply".into())));
    };
    Ok(Json(Tested {
        ok: true,
        model: parsed["model"].as_str().map(str::to_string),
        endpoint,
        reply: Some(reply.trim().to_string()),
        error: None,
        elapsed_ms,
    }))
}

// -- Recipes -------------------------------------------------------------------

/// A provider a recipe needs an account at.
#[derive(Debug, Serialize)]
pub struct Vendor {
    pub key: &'static str,
    pub name: &'static str,
    pub protocol: &'static str,
    pub base_url: &'static str,
    /// Where its key is usually found, for an operator whose keys live in
    /// the gateway's environment.
    pub env: &'static str,
    /// The catalog's id for it.
    #[serde(skip)]
    catalog: &'static str,
}

const ANTHROPIC: Vendor = Vendor {
    key: "anthropic",
    name: "Anthropic",
    protocol: "anthropic",
    base_url: "https://api.anthropic.com",
    env: "ANTHROPIC_API_KEY",
    catalog: "anthropic",
};

const GEMINI: Vendor = Vendor {
    key: "gemini",
    name: "Google Gemini",
    protocol: "gemini",
    base_url: "https://generativelanguage.googleapis.com",
    env: "GEMINI_API_KEY",
    catalog: "google",
};

/// Pairs of the same tier from two vendors, so a fallback answers about as
/// well as what it stands in for. Named by family, never by model: which
/// model a tier is today is the catalog's to say.
const RECIPES: &[(&str, &str, &str, &[(&Vendor, &str)])] = &[
    (
        "high",
        "High tier",
        "Claude Opus, falling back to Gemini Pro.",
        &[(&ANTHROPIC, "claude-opus"), (&GEMINI, "gemini-pro")],
    ),
    (
        "medium",
        "Medium tier",
        "Claude Sonnet, falling back to Gemini Flash.",
        &[(&ANTHROPIC, "claude-sonnet"), (&GEMINI, "gemini-flash")],
    ),
    (
        "low",
        "Low tier",
        "Claude Haiku, falling back to Gemini Flash-Lite.",
        &[(&ANTHROPIC, "claude-haiku"), (&GEMINI, "gemini-flash-lite")],
    ),
];

#[derive(Debug, Serialize)]
pub struct RecipeLink {
    pub vendor: &'static Vendor,
    /// The family's newest chat model in the catalog held, or none when the
    /// catalog has none -- and the recipe cannot be set up.
    pub model: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Recipe {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    /// In order: the first is asked, the next when it is unavailable.
    pub links: Vec<RecipeLink>,
}

pub async fn recipes(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Recipe>>, ApiError> {
    operator(&state, &headers)?;
    let held: serde_json::Value =
        sqlx::query_scalar("select models from catalog_snapshots order by fetched_at desc limit 1")
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?
            .unwrap_or_default();
    Ok(Json(
        RECIPES
            .iter()
            .map(|(id, title, summary, links)| Recipe {
                id,
                title,
                summary,
                links: links
                    .iter()
                    .map(|(vendor, family)| {
                        let model = catalog::newest_of_family(&held, vendor.catalog, family);
                        let name = model
                            .as_deref()
                            .and_then(|m| catalog::entry(&held, vendor.catalog, m))
                            .and_then(|e| e["name"].as_str())
                            .map(str::to_string);
                        RecipeLink {
                            vendor,
                            model,
                            name,
                        }
                    })
                    .collect(),
            })
            .collect(),
    ))
}
