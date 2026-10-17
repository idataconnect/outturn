//! Where the model catalog is kept, and what this deployment holds of it
//! (docs/models.md, *The catalog*).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::ApiState;
use super::router::ApiError;
use outturn_core::catalog;

/// Where the catalog is fetched from: the site, or a mirror of it.
pub const URL_VAR: &str = "OUTTURN_CATALOG_URL";
pub const DEFAULT_URL: &str = "https://models.dev/api.json";

/// `off` stops this deployment fetching the catalog at all; it keeps what it
/// shipped with and what it is given.
pub const FETCH_VAR: &str = "OUTTURN_CATALOG_FETCH";

/// How often the catalog is checked: new models arrive weekly at most.
const EVERY: chrono::Duration = chrono::Duration::days(7);

/// The most an operator may ask for a check, so a button pressed in a loop is
/// not a loop of fetches against somebody else's free service.
const AT_MOST_EVERY: chrono::Duration = chrono::Duration::hours(1);

/// A file to load the catalog from at startup -- a mounted ConfigMap, say --
/// for a deployment that keeps it with the rest of its configuration.
pub const FILE_VAR: &str = "OUTTURN_CATALOG_FILE";

/// Keeps a catalog as a snapshot, unless the newest one already says the
/// same. Returns the new snapshot's id, or None when nothing was new.
pub async fn keep(
    pool: &PgPool,
    source: &str,
    etag: Option<&str>,
    models: &serde_json::Value,
) -> Result<Option<Uuid>, sqlx::Error> {
    let sha = catalog::digest(models);
    let newest: Option<String> =
        sqlx::query_scalar("select sha256 from catalog_snapshots order by fetched_at desc limit 1")
            .fetch_optional(pool)
            .await?;
    if newest.as_deref() == Some(sha.as_str()) {
        return Ok(None);
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into catalog_snapshots (id, source, etag, sha256, models) values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(source)
    .bind(etag)
    .bind(&sha)
    .bind(models)
    .execute(pool)
    .await?;
    Ok(Some(id))
}

/// What a deployment starts with: the file it was pointed at, if any, and
/// otherwise -- when it holds no catalog at all -- the one this release
/// shipped. Never a fetch: starting must not depend on anything outside.
pub async fn at_startup(pool: &PgPool) {
    if let Ok(path) = std::env::var(FILE_VAR)
        && !path.is_empty()
    {
        match std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| catalog::read(&bytes).map_err(|e| e.to_string()))
        {
            Ok(models) => match keep(pool, &format!("file:{path}"), None, &models).await {
                Ok(Some(_)) => tracing::info!(%path, "the catalog was loaded from a file"),
                Ok(None) => {}
                Err(e) => tracing::error!(%path, error = %e, "could not keep the catalog file"),
            },
            // Said and carried on: the shipped catalog, or the one already
            // held, is still a catalog.
            Err(e) => tracing::error!(%path, error = %e, "the catalog file could not be read"),
        }
    }

    // The first check comes at a random hour of the first day, so a fleet
    // of deployments started together does not ask together.
    let _ = sqlx::query(
        "insert into catalog_checks (next_check_at) \
         values (now() + make_interval(secs => 3600 + floor(random() * 82800))) \
         on conflict do nothing",
    )
    .execute(pool)
    .await;

    let held: Option<i64> = sqlx::query_scalar("select 1::bigint from catalog_snapshots limit 1")
        .fetch_optional(pool)
        .await
        .unwrap_or(Some(1));
    if held.is_none()
        && let Err(e) = keep(pool, catalog::SHIPPED_SOURCE, None, &catalog::shipped()).await
    {
        tracing::error!(error = %e, "could not keep the shipped catalog");
    }
}

/// The catalog this deployment holds, as the Platform page shows it.
#[derive(Debug, Serialize)]
pub struct Held {
    pub source: String,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub providers: usize,
    pub models: usize,
    /// How many versions of it have been kept.
    pub snapshots: i64,
    /// When the source was last asked, when it will be next, and what went
    /// wrong last time, if anything did.
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub next_check_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
    /// Whether this deployment fetches at all.
    pub fetching: bool,
}

pub async fn view(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Held>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    if !claims.is_system_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator sees the catalog".into(),
        ));
    }
    let internal = |e: sqlx::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    let row = sqlx::query(
        "select source, fetched_at, models, (select count(*) from catalog_snapshots) as snapshots \
         from catalog_snapshots order by fetched_at desc limit 1",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?
    .ok_or((StatusCode::NOT_FOUND, "no catalog is held".to_string()))?;
    let models: serde_json::Value = row.get("models");
    let (providers, model_count) = catalog::counts(&models);
    let checks = sqlx::query("select checked_at, next_check_at, last_error from catalog_checks")
        .fetch_optional(&state.pool)
        .await
        .map_err(internal)?;
    Ok(Json(Held {
        source: row.get("source"),
        fetched_at: row.get("fetched_at"),
        providers,
        models: model_count,
        snapshots: row.get("snapshots"),
        checked_at: checks.as_ref().and_then(|c| c.get("checked_at")),
        next_check_at: checks.as_ref().map(|c| c.get("next_check_at")),
        last_error: checks.as_ref().and_then(|c| c.get("last_error")),
        fetching: !fetching_off(),
    }))
}

/// What a check found.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Checked {
    /// The source said nothing changed, or sent what is already held.
    Unchanged,
    /// A newer catalog, now kept.
    Kept,
}

fn source_url() -> String {
    std::env::var(URL_VAR)
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_string())
}

fn fetching_off() -> bool {
    std::env::var(FETCH_VAR).is_ok_and(|v| v.eq_ignore_ascii_case("off"))
}

/// Asks the source whether the catalog changed, and keeps it if it did.
///
/// Conditional on what the source said last time, so an unchanged catalog
/// costs it a "not modified". Fetched through the gateway, which is the tier
/// that reaches the outside (`crate::fetch`). Records when it asked and when
/// it next will, whatever came of it.
pub async fn check(
    pool: &PgPool,
    minter: &outturn_core::auth::TokenMinter,
    gateway: &str,
    actor: Uuid,
) -> Result<Checked, String> {
    let url = source_url();
    let etag: Option<String> =
        sqlx::query_scalar("select etag from catalog_checks where source = $1")
            .bind(&url)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?
            .flatten();

    let mut headers = vec![
        ("accept", "application/json"),
        ("accept-encoding", "identity"),
    ];
    if let Some(etag) = &etag {
        headers.push(("if-none-match", etag.as_str()));
    }
    let outcome = async {
        let fetched = crate::fetch::get(minter, gateway, actor, &url, &headers)
            .await
            .map_err(|e| e.to_string())?;
        match fetched.status {
            304 => Ok((Checked::Unchanged, etag.clone())),
            200..=299 if fetched.truncated => Err(format!("{url} is larger than a catalog may be")),
            200..=299 => {
                let models = catalog::read(fetched.body.as_bytes()).map_err(|e| e.to_string())?;
                let etag = fetched.header("etag").map(str::to_string);
                let kept = keep(pool, &url, etag.as_deref(), &models)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok((
                    if kept.is_some() {
                        Checked::Kept
                    } else {
                        Checked::Unchanged
                    },
                    etag,
                ))
            }
            status => Err(format!("{url} answered {status}")),
        }
    }
    .await;

    let (etag, error) = match &outcome {
        Ok((_, etag)) => (etag.clone(), None),
        Err(e) => (etag.clone(), Some(e.clone())),
    };
    sqlx::query(
        "update catalog_checks set source = $1, etag = $2, checked_at = now(), last_error = $3, \
         next_check_at = now() + make_interval(secs => $4 + floor(random() * 86400))",
    )
    .bind(&url)
    .bind(etag)
    .bind(&error)
    .bind(EVERY.num_seconds() as f64)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    match outcome {
        Ok((checked, _)) => {
            tracing::info!(%url, ?checked, "the model catalog was checked");
            Ok(checked)
        }
        Err(e) => {
            tracing::warn!(%url, error = %e, "the model catalog could not be checked");
            Err(e)
        }
    }
}

/// The scheduled check, when it is due and fetching is on. Called from the
/// worker's maintenance tick; never from anything a request waits on.
pub async fn check_if_due(pool: &PgPool, minter: &outturn_core::auth::TokenMinter, gateway: &str) {
    if fetching_off() {
        return;
    }
    let due: Option<bool> = sqlx::query_scalar("select next_check_at <= now() from catalog_checks")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    if due == Some(true) {
        let _ = check(pool, minter, gateway, Uuid::nil()).await;
    }
}

/// The operator's "check now", at most once an hour.
pub async fn refresh(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    if !claims.is_system_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator refreshes the catalog".into(),
        ));
    }
    if fetching_off() {
        return Err((
            StatusCode::CONFLICT,
            format!("fetching the catalog is turned off ({FETCH_VAR}=off)"),
        ));
    }
    let gateway = std::env::var("OUTTURN_GATEWAY_URL").map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "fetching the catalog needs OUTTURN_GATEWAY_URL set on the API".to_string(),
        )
    })?;
    let recent: Option<bool> = sqlx::query_scalar(
        "select checked_at > now() - make_interval(secs => $1) from catalog_checks",
    )
    .bind(AT_MOST_EVERY.num_seconds() as f64)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .flatten();
    if recent == Some(true) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "the catalog was checked within the hour".into(),
        ));
    }
    let checked = check(&state.pool, &state.minter, &gateway, claims.subject)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("the catalog could not be checked: {e}"),
            )
        })?;
    Ok(Json(serde_json::json!({ "checked": checked })))
}

/// The most a catalog file may be: the site's document is several megabytes,
/// and a reduced or gzipped one far less.
pub const IMPORT_LIMIT: usize = 32 * 1024 * 1024;

/// A catalog carried in by hand -- the site's document, a reduced one, or
/// either gzipped -- for a deployment that cannot fetch one.
pub async fn import(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let claims = super::router::authenticate(&state, &headers)?;
    if !claims.is_system_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator imports a catalog".into(),
        ));
    }
    let models =
        catalog::read(&body).map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let kept = keep(&state.pool, "upload", None, &models)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (providers, models) = catalog::counts(&models);
    tracing::info!(actor = %claims.subject, providers, models, "a catalog was imported");
    Ok(Json(serde_json::json!({
        "checked": if kept.is_some() { Checked::Kept } else { Checked::Unchanged },
    })))
}
