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
    Ok(Json(Held {
        source: row.get("source"),
        fetched_at: row.get("fetched_at"),
        providers,
        models: model_count,
        snapshots: row.get("snapshots"),
    }))
}
