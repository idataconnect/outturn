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
///
/// Without its system prompt or policy. A list names agents and says which can
/// be talked to; the prompt is the largest thing an agent has and nothing that
/// lists them reads it, so it is fetched with the agent itself
/// (`GET /v1/agents/{id}`) by the page that edits it.
#[derive(Debug, serde::Serialize)]
pub struct ListedAgent {
    #[serde(flatten)]
    pub agent: AgentSummary,
    /// Whether this caller may start a conversation with it that it will
    /// answer. The roster is workspace-public but starting a conversation is
    /// narrowed, and a disabled agent accepts a session and then fails its
    /// first turn -- so without this a page offers every agent and a person
    /// finds out which ones work by being refused.
    pub can_chat: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct AgentSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub enabled: bool,
}

impl From<Agent> for AgentSummary {
    fn from(agent: Agent) -> Self {
        Self {
            id: agent.id,
            workspace_id: agent.workspace_id,
            name: agent.name,
            slug: agent.slug,
            description: agent.description,
            enabled: agent.enabled,
        }
    }
}

pub async fn list_agents(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(query): axum::extract::Query<super::PageQuery>,
) -> Result<Json<super::Page<ListedAgent>>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    // The workspace comes from the token, never from the request: a caller can
    // only reach agents in a workspace they hold a minted token for.
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let held = super::router::authorities_of(&state, &claims).await?;
    let reach = super::router::reach_of(&state, &claims).await?;
    let items: Vec<ListedAgent> = state
        .agents
        .list(claims.workspace_id, query.after, limit + 1)
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
            agent: AgentSummary::from(agent),
        })
        .collect();
    Ok(Json(super::Page::from_rows(items, limit, |a| a.agent.id)))
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
    let current = state.agents.get(claims.workspace_id, id).await?;
    template_rules(&state, &current, &input).await?;
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
    let agent = state.agents.get(claims.workspace_id, id).await?;
    // A required template's agent is refused; a default one's removal is
    // remembered, so the next publish does not make it again.
    if let Some(template_id) = agent.template_id {
        state
            .templates
            .removing(claims.workspace_id, template_id)
            .await?;
    }
    state.agents.delete(claims.workspace_id, id).await?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        agent_id = %id,
        "agent deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// What a workspace may change about an agent, which depends on whether it was
/// made from a template.
///
/// A template's agent takes its name, instructions and policy from the
/// template, so those are the operator's to change; what the workspace has is
/// its own section of the prompt, where the template allows one. An agent made
/// by hand has its whole prompt, and no section to add.
async fn template_rules(
    state: &ApiState,
    agent: &Agent,
    input: &UpdateAgent,
) -> Result<(), ApiError> {
    let Some(template_id) = agent.template_id else {
        if input.workspace_addition.is_some() {
            return Err((
                StatusCode::BAD_REQUEST,
                "only an agent made from a template has a workspace section; edit this \
                 agent's instructions instead"
                    .into(),
            ));
        }
        return Ok(());
    };
    if input.name.is_some()
        || input.description.is_some()
        || input.system_prompt.is_some()
        || input.policy.is_some()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "this agent's name, instructions and policy come from its template; write how \
             this business works in its workspace section instead"
                .into(),
        ));
    }
    if let Some(addition) = &input.workspace_addition {
        let template = state.templates.get(template_id).await?;
        if !template.allow_additions && !addition.trim().is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "the operator does not allow additions to this agent's instructions".into(),
            ));
        }
        if addition.len() > super::agent_template::MAX_ADDITION_BYTES {
            return Err((
                StatusCode::BAD_REQUEST,
                format!(
                    "a workspace section is at most {} bytes",
                    super::agent_template::MAX_ADDITION_BYTES
                ),
            ));
        }
    }
    Ok(())
}
