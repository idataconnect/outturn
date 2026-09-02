use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::auth::Authority;

use super::agent::{Agent, AgentError, CreateAgent, UpdateAgent};
use super::router::{ApiError, ApiState, authorize};

impl From<AgentError> for ApiError {
    fn from(e: AgentError) -> Self {
        let status = match e {
            AgentError::NotFound => StatusCode::NOT_FOUND,
            AgentError::DuplicateSlug(_) => StatusCode::CONFLICT,
            AgentError::Invalid(_) => StatusCode::BAD_REQUEST,
            AgentError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, e.to_string())
    }
}

pub async fn list_agents(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Agent>>, ApiError> {
    // The tenant comes from the token, never from the request: a caller can
    // only reach agents in a tenant they hold a minted token for.
    let claims = authorize(&state, &headers, Authority::AgentsRead)?;
    Ok(Json(state.agents.list(claims.tenant_id).await?))
}

pub async fn create_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateAgent>,
) -> Result<(StatusCode, Json<Agent>), ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsCreate)?;
    let agent = state.agents.create(claims.tenant_id, input).await?;
    tracing::info!(
        actor = %claims.session_id,
        tenant_id = %claims.tenant_id,
        agent_id = %agent.id,
        "agent created"
    );
    Ok((StatusCode::CREATED, Json(agent)))
}

pub async fn get_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Agent>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead)?;
    Ok(Json(state.agents.get(claims.tenant_id, id).await?))
}

pub async fn update_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateAgent>,
) -> Result<Json<Agent>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate)?;
    let agent = state.agents.update(claims.tenant_id, id, input).await?;
    tracing::info!(
        actor = %claims.session_id,
        tenant_id = %claims.tenant_id,
        agent_id = %agent.id,
        "agent updated"
    );
    Ok(Json(agent))
}

pub async fn delete_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsDelete)?;
    state.agents.delete(claims.tenant_id, id).await?;
    tracing::info!(
        actor = %claims.session_id,
        tenant_id = %claims.tenant_id,
        agent_id = %id,
        "agent deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}
