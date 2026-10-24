//! Who is asked, by the risk a request works out to.
//!
//! See `docs/auto-approval.md`, "The same answer routes the question". `low`
//! goes through where a policy covers it, and asks as `medium` does where none
//! does; `medium` asks anyone holding `approvals:answer`, as every question did
//! before; `high` asks the roles a workspace names for it -- a manager, say --
//! and anyone holding `approvals:answer` where it names none. The queue already
//! targets roles, so this is a choice of targets rather than a new mechanism.
//!
//! A named role has to hold `approvals:answer`, checked when it is named and
//! again when a question is raised: a role that lost the authority since is
//! skipped, and if that leaves nobody the question goes to every approver
//! rather than to nobody, since a question nobody may answer parks a
//! conversation for ever.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use outturn_core::auth::Authority;
use outturn_core::egress::policy::Risk;

use crate::actions::Target;
use crate::router::{ApiError, ApiState, authenticate};

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// The roles a workspace named for high, whatever they hold now.
pub async fn named(pool: &sqlx::PgPool, workspace_id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select role_id from approval_routes \
         where workspace_id = $1 and risk = 'high' order by role_id",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

/// Who a question at this risk goes to: of `answerers`, the roles holding
/// `approvals:answer`.
///
/// Anything but high, or high with no named role that still answers, is every
/// answerer -- the behavior from before risks routed anything.
pub fn targets(risk: Risk, named: &[Uuid], answerers: &[Uuid]) -> Vec<Target> {
    let chosen: Vec<Uuid> = match risk {
        Risk::High => named
            .iter()
            .copied()
            .filter(|r| answerers.contains(r))
            .collect(),
        Risk::Low | Risk::Medium => Vec::new(),
    };
    let chosen = if chosen.is_empty() {
        answerers.to_vec()
    } else {
        chosen
    };
    chosen.into_iter().map(Target::Role).collect()
}

#[derive(Debug, Serialize)]
pub struct RoleRef {
    pub role_id: Uuid,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct Routes {
    /// The roles asked about a high-risk request. Empty: every answerer.
    pub high: Vec<RoleRef>,
    /// The roles that may be named: those holding `approvals:answer`.
    pub answerers: Vec<RoleRef>,
    pub may_write: bool,
}

async fn names(
    state: &ApiState,
    workspace_id: Uuid,
    ids: &[Uuid],
) -> Result<Vec<RoleRef>, ApiError> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "select id, name from roles where workspace_id = $1 and id = any($2) order by name",
    )
    .bind(workspace_id)
    .bind(ids)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    Ok(rows
        .into_iter()
        .map(|(role_id, name)| RoleRef { role_id, name })
        .collect())
}

async fn answerers(state: &ApiState, workspace_id: Uuid) -> Result<Vec<Uuid>, ApiError> {
    state
        .roles
        .roles_with(workspace_id, Authority::ApprovalsAnswer)
        .await
        .map_err(internal)
}

async fn read(state: &ApiState, workspace_id: Uuid, may_write: bool) -> Result<Routes, ApiError> {
    let named = named(&state.pool, workspace_id).await.map_err(internal)?;
    let answerers = answerers(state, workspace_id).await?;
    Ok(Routes {
        high: names(state, workspace_id, &named).await?,
        answerers: names(state, workspace_id, &answerers).await?,
        may_write,
    })
}

/// `GET /v1/approval-routes`.
pub async fn get(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Routes>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_read(&state, &claims).await?;
    let may_write = super::api::may_write(&state, &claims).await.is_ok();
    Ok(Json(read(&state, claims.workspace_id, may_write).await?))
}

#[derive(Debug, Deserialize)]
pub struct WriteRoutes {
    pub role_ids: Vec<Uuid>,
}

/// `PUT /v1/approval-routes/high`: the roles asked about a high-risk request,
/// replacing those named before. Empty goes back to every answerer.
///
/// Written as a policy is, by a person holding both authorities: naming who
/// answers the largest questions is a judgment about the same money.
pub async fn put_high(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<WriteRoutes>,
) -> Result<Json<Routes>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_write(&state, &claims).await?;
    let answerers = answerers(&state, claims.workspace_id).await?;
    if let Some(role) = input.role_ids.iter().find(|r| !answerers.contains(r)) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("role {role} does not hold approvals:answer, so nobody in it could answer"),
        ));
    }
    let mut tx = state.pool.begin().await.map_err(internal)?;
    sqlx::query("delete from approval_routes where workspace_id = $1 and risk = 'high'")
        .bind(claims.workspace_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    for role in &input.role_ids {
        sqlx::query(
            "insert into approval_routes (workspace_id, risk, role_id, updated_by) \
             values ($1, 'high', $2, $3) on conflict do nothing",
        )
        .bind(claims.workspace_id)
        .bind(role)
        .bind(claims.subject)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    }
    tx.commit().await.map_err(internal)?;
    tracing::info!(
        workspace_id = %claims.workspace_id,
        roles = input.role_ids.len(),
        set_by = %claims.subject,
        "who is asked about high-risk requests was set"
    );
    Ok(Json(read(&state, claims.workspace_id, true).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_asks_the_named_roles_that_still_answer() {
        let (manager, approver, gone) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let answerers = [manager, approver];
        let ids = |t: Vec<Target>| t.iter().filter_map(Target::role_id).collect::<Vec<_>>();
        assert_eq!(
            ids(targets(Risk::High, &[manager], &answerers)),
            vec![manager]
        );
        // Low and medium ask every answerer, named roles or not.
        assert_eq!(
            ids(targets(Risk::Medium, &[manager], &answerers)),
            answerers
        );
        assert_eq!(ids(targets(Risk::Low, &[manager], &answerers)), answerers);
        // Nothing named, or nothing named that still answers: every answerer.
        assert_eq!(ids(targets(Risk::High, &[], &answerers)), answerers);
        assert_eq!(ids(targets(Risk::High, &[gone], &answerers)), answerers);
    }
}
