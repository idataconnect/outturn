//! Machine users: accounts that exist to hold connections for the turns
//! nobody sent. See docs/user-credentials.md, *Nobody asked*.
//!
//! A turn a schedule or a webhook started has no person behind it, so a rule
//! whose connection is each person's own has nobody's key to send. An agent
//! may name one of these instead: its turns nobody sent carry the machine user
//! as their asker, and every such rule resolves to the key connected for it.
//! The backend then sees a caller of its own, which it can give its own
//! narrow permissions to.
//!
//! Nobody signs in as one and it holds no role -- the database refuses both --
//! which is why an administrator may connect its keys, when for a person only
//! they may: it is nobody, and acting as it is acting as nobody.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use sqlx::Row;
use uuid::Uuid;

use outturn_core::auth::Authority;

use super::connections::{Connected, Connection, NewConnection};
use super::credentials::{SealKey, seal_key_of};
use super::router::{ApiError, ApiState, authorize};

#[derive(Debug, serde::Serialize)]
pub struct MachineUser {
    pub id: Uuid,
    pub display_name: String,
    /// The rules whose connection is each person's own, and this account's
    /// key for each.
    pub connections: Vec<Connection>,
    /// The agents whose turns nobody sent carry this account.
    pub agents: Vec<Uuid>,
}

#[derive(serde::Serialize)]
pub struct MachineUsers {
    pub machine_users: Vec<MachineUser>,
    /// What to seal a key to, so the page needs no second request.
    pub seal_key: Option<SealKey>,
}

#[derive(Debug, serde::Deserialize)]
pub struct NewMachineUser {
    pub display_name: String,
}

fn internal(e: sqlx::Error) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// That `id` is a machine user of `workspace_id`; 404 otherwise, so an id is
/// not an oracle for what exists elsewhere.
async fn ours(state: &ApiState, workspace_id: Uuid, id: Uuid) -> Result<(), ApiError> {
    let found: bool =
        sqlx::query_scalar("select exists (select 1 from users where id = $1 and machine_of = $2)")
            .bind(id)
            .bind(workspace_id)
            .fetch_one(&state.pool)
            .await
            .map_err(internal)?;
    if found {
        Ok(())
    } else {
        Err((StatusCode::NOT_FOUND, "no such machine user".to_string()))
    }
}

pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<MachineUsers>, ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsRead).await?;
    let rows = sqlx::query(
        "select u.id, u.display_name, \
             coalesce(array_agg(a.id order by a.name) filter (where a.id is not null), '{}') \
                 as agents \
         from users u left join agents a on a.machine_user_id = u.id \
         where u.machine_of = $1 \
         group by u.id order by u.display_name",
    )
    .bind(claims.workspace_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    let mut machine_users = Vec::with_capacity(rows.len());
    for row in rows {
        let id: Uuid = row.get("id");
        machine_users.push(MachineUser {
            id,
            display_name: row.get("display_name"),
            connections: super::connections::of(&state, claims.workspace_id, id).await?,
            agents: row.get("agents"),
        });
    }
    Ok(Json(MachineUsers {
        machine_users,
        seal_key: seal_key_of(&state).ok(),
    }))
}

pub async fn create(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<NewMachineUser>,
) -> Result<(StatusCode, Json<MachineUser>), ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    let name = input.display_name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err((
            StatusCode::BAD_REQUEST,
            "a machine user needs a name".to_string(),
        ));
    }
    let id = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name, machine_of) values ($1, $2, $3)")
        .bind(id)
        .bind(name)
        .bind(claims.workspace_id)
        .execute(&state.pool)
        .await
        .map_err(internal)?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        machine_user_id = %id,
        "machine user created"
    );
    Ok((
        StatusCode::CREATED,
        Json(MachineUser {
            id,
            display_name: name.to_string(),
            connections: super::connections::of(&state, claims.workspace_id, id).await?,
            agents: Vec::new(),
        }),
    ))
}

/// Deletes one, and with it every key connected for it. Agents that named it
/// go back to having nobody for their turns nobody sent.
pub async fn delete(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    ours(&state, claims.workspace_id, id).await?;
    // Revoked rather than left to the cascade, so the gateway hears of each
    // key going as it does any revocation, and the rows say when.
    sqlx::query(
        "update credentials set sealed = null, revoked_at = now(), \
             generation = generation + 1, updated_at = now() \
         where user_id = $1 and revoked_at is null",
    )
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    sqlx::query("delete from users where id = $1 and machine_of = $2")
        .bind(id)
        .bind(claims.workspace_id)
        .execute(&state.pool)
        .await
        .map_err(internal)?;
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        machine_user_id = %id,
        "machine user deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

pub async fn connect(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((id, rule_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<NewConnection>,
) -> Result<(StatusCode, Json<Connected>), ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    ours(&state, claims.workspace_id, id).await?;
    let connected = super::connections::store(
        &state,
        claims.workspace_id,
        id,
        claims.subject,
        rule_id,
        &input,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(connected)))
}

pub async fn disconnect(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path((id, rule_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::CredentialsWrite).await?;
    ours(&state, claims.workspace_id, id).await?;
    if !super::connections::revoke(&state, claims.workspace_id, id, rule_id).await? {
        return Err((
            StatusCode::NOT_FOUND,
            "nothing is connected for that".to_string(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct AgentMachineUser {
    pub machine_user_id: Option<Uuid>,
}

/// The machine user an agent's turns nobody sent carry, if any.
pub async fn of_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(agent): Path<Uuid>,
) -> Result<Json<AgentMachineUser>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let machine_user_id: Option<Option<Uuid>> = sqlx::query_scalar(
        "select machine_user_id from agents where id = $1 and workspace_id = $2",
    )
    .bind(agent)
    .bind(claims.workspace_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(internal)?;
    let machine_user_id =
        machine_user_id.ok_or((StatusCode::NOT_FOUND, "no such agent".to_string()))?;
    Ok(Json(AgentMachineUser { machine_user_id }))
}

/// Names, or clears, the machine user an agent's turns nobody sent carry.
///
/// Whose keys go with those turns is a credential decision as well as one
/// about the agent, so both authorities.
pub async fn set_for_agent(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(agent): Path<Uuid>,
    Json(input): Json<AgentMachineUser>,
) -> Result<Json<AgentMachineUser>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    authorize(&state, &headers, Authority::CredentialsWrite).await?;
    if let Some(id) = input.machine_user_id {
        ours(&state, claims.workspace_id, id).await?;
    }
    let updated = sqlx::query(
        "update agents set machine_user_id = $3, updated_at = now() \
         where id = $1 and workspace_id = $2",
    )
    .bind(agent)
    .bind(claims.workspace_id)
    .bind(input.machine_user_id)
    .execute(&state.pool)
    .await
    .map_err(internal)?;
    if updated.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "no such agent".to_string()));
    }
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        agent_id = %agent,
        machine_user_id = ?input.machine_user_id,
        "agent's machine user set"
    );
    Ok(Json(input))
}
