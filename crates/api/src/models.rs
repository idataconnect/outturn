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
        "select id, account_id, name, model, catalog_provider, catalog_model, facts, overrides \
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
    sqlx::query(
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
                let entry = catalog::entry(&held, &provider, &input.model)?.clone();
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
    let name = input
        .name
        .filter(|n| !n.trim().is_empty())
        .or_else(|| facts.as_ref()?["name"].as_str().map(str::to_string))
        .unwrap_or_else(|| input.model.clone());
    let id = Uuid::now_v7();
    let (catalog_provider, snapshot) = link.map_or((None, None), |(p, s)| (Some(p), Some(s)));
    let catalog_model = catalog_provider.as_ref().map(|_| input.model.clone());
    sqlx::query(
        "insert into models (id, workspace_id, account_id, name, model, catalog_provider, \
             catalog_model, snapshot_id, facts, overrides) \
         values ($1, null, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(input.account_id)
    .bind(&name)
    .bind(&input.model)
    .bind(&catalog_provider)
    .bind(&catalog_model)
    .bind(snapshot)
    .bind(&facts)
    .bind(&overrides)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    tracing::info!(%actor, model = %id, name = %input.model, "model configured");
    Ok((
        StatusCode::CREATED,
        Json(Model {
            id,
            account_id: input.account_id,
            name,
            model: input.model,
            catalog_provider,
            catalog_model,
            facts,
            overrides,
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
    let done = sqlx::query(
        "update models set name = coalesce($2, name), overrides = coalesce($3, overrides) \
         where id = $1 and workspace_id is null",
    )
    .bind(id)
    .bind(input.name.filter(|n| !n.trim().is_empty()))
    .bind(input.overrides)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    if done.rows_affected() == 0 {
        return Err(not_found("model"));
    }
    tracing::info!(%actor, model = %id, "model changed");
    Ok(StatusCode::NO_CONTENT)
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
