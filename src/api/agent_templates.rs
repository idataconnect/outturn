//! Agent templates over HTTP: the operator's routes for making and publishing
//! them, and a workspace's catalog of them. See docs/agent-templates.md.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::auth::Authority;

use super::agent_template::{
    CatalogEntry, NewTemplate, NewVersion, Template, TemplateError, TemplateVersion, UpdateTemplate,
};
use super::router::{ApiError, ApiState, authenticate, authorize};

impl From<TemplateError> for ApiError {
    fn from(e: TemplateError) -> Self {
        let status = match e {
            TemplateError::NotFound => StatusCode::NOT_FOUND,
            TemplateError::DuplicateSlug(_) => StatusCode::CONFLICT,
            TemplateError::Invalid(_) => StatusCode::BAD_REQUEST,
            TemplateError::Refused(_) => StatusCode::CONFLICT,
            TemplateError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

/// Templates are the operator's: they reach every workspace, so they are
/// written only by a system administrator, as platform skills are.
fn as_operator(state: &ApiState, headers: &axum::http::HeaderMap) -> Result<Uuid, ApiError> {
    let claims = authenticate(state, headers)?;
    if !claims.is_system_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator manages agent templates".into(),
        ));
    }
    Ok(claims.subject)
}

pub async fn list_templates(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<super::PageQuery>,
) -> Result<Json<super::Page<Template>>, ApiError> {
    as_operator(&state, &headers)?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let items = state.templates.list(query.after, limit + 1).await?;
    Ok(Json(super::Page::from_rows(items, limit, |t| t.id)))
}

pub async fn get_template(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Template>, ApiError> {
    as_operator(&state, &headers)?;
    Ok(Json(state.templates.get(id).await?))
}

pub async fn create_template(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<NewTemplate>,
) -> Result<(StatusCode, Json<Template>), ApiError> {
    let actor = as_operator(&state, &headers)?;
    let template = state.templates.create(input, Some(actor)).await?;
    tracing::info!(%actor, template_id = %template.id, "agent template created");
    Ok((StatusCode::CREATED, Json(template)))
}

pub async fn publish_template(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<NewVersion>,
) -> Result<Json<Template>, ApiError> {
    let actor = as_operator(&state, &headers)?;
    let template = state.templates.publish(id, input, Some(actor)).await?;
    tracing::info!(
        %actor,
        template_id = %id,
        ordinal = template.current.ordinal,
        "agent template published"
    );
    Ok(Json(template))
}

pub async fn update_template(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateTemplate>,
) -> Result<Json<Template>, ApiError> {
    let actor = as_operator(&state, &headers)?;
    let template = state.templates.update(id, input).await?;
    tracing::info!(%actor, template_id = %id, "agent template updated");
    Ok(Json(template))
}

pub async fn list_template_versions(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<super::PageQuery>,
) -> Result<Json<super::Page<TemplateVersion>>, ApiError> {
    as_operator(&state, &headers)?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let items = state.templates.versions(id, query.after, limit + 1).await?;
    Ok(Json(super::Page::from_rows(items, limit, |v| v.id)))
}

/// The templates this workspace may have, and which it has.
pub async fn catalog(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(query): Query<super::PageQuery>,
) -> Result<Json<super::Page<CatalogEntry>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    // Owed agents made first, so a workspace whose provisioning failed when it
    // was created, or at a publish, gets them the next time anyone looks. One
    // query when nothing is owed.
    if let Err(e) = state
        .templates
        .provision(Some(claims.workspace_id), None)
        .await
    {
        tracing::warn!(workspace_id = %claims.workspace_id, error = %e, "could not provision template agents");
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let items = state
        .templates
        .catalog(claims.workspace_id, query.after, limit + 1)
        .await?;
    Ok(Json(super::Page::from_rows(items, limit, |e| {
        e.template_id
    })))
}

#[derive(serde::Serialize)]
pub struct Installed {
    pub agent_id: Uuid,
}

/// Adds a template's agent to this workspace, or finds the one it has.
pub async fn install(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Installed>), ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsCreate).await?;
    let agent_id = state.templates.install(claims.workspace_id, id).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        template_id = %id,
        %agent_id,
        "agent added from a template"
    );
    Ok((StatusCode::CREATED, Json(Installed { agent_id })))
}
