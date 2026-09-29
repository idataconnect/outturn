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

/// An agent as the roster shows it to one caller.
#[derive(Debug, serde::Serialize)]
pub struct ListedAgent {
    #[serde(flatten)]
    pub agent: Agent,
    /// Whether this caller may start a conversation with it that it will
    /// answer. The roster is workspace-public but starting a conversation is
    /// narrowed, and a disabled agent accepts a session and then fails its
    /// first turn -- so without this a page offers every agent and a person
    /// finds out which ones work by being refused.
    pub can_chat: bool,
}

pub async fn list_agents(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<ListedAgent>>, ApiError> {
    // The workspace comes from the token, never from the request: a caller can
    // only reach agents in a workspace they hold a minted token for.
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let held = super::router::authorities_of(&state, &claims).await?;
    let reach = super::router::reach_of(&state, &claims).await?;
    Ok(Json(
        state
            .agents
            .list(claims.workspace_id)
            .await?
            .into_iter()
            .map(|agent| ListedAgent {
                // The two things `create_session` checks, in its order.
                can_chat: super::router::may_for_agent(
                    &held,
                    &reach,
                    Authority::SessionsCreate,
                    agent.id,
                ) && agent.takes_conversations(),
                agent,
            })
            .collect(),
    ))
}

pub async fn create_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<CreateAgent>,
) -> Result<(StatusCode, Json<Agent>), ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsCreate).await?;
    let agent = state.agents.create(claims.workspace_id, input).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
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
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    Ok(Json(state.agents.get(claims.workspace_id, id).await?))
}

pub async fn update_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateAgent>,
) -> Result<Json<Agent>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    let agent = state.agents.update(claims.workspace_id, id, input).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
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
    let claims = authorize(&state, &headers, Authority::AgentsDelete).await?;
    state.agents.delete(claims.workspace_id, id).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        agent_id = %id,
        "agent deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}
