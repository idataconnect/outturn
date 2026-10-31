//! A person's own connections: the keys they connect for the rules whose
//! connection is the asker's. See docs/user-credentials.md, *Connecting one*.
//!
//! Stored by the person and nobody else: storing a key as somebody is acting
//! as them, so these routes take no authority beyond being a member of the
//! workspace, and act only on the caller's own. An administrator may revoke
//! one through `DELETE /v1/credentials/{id}`, without being able to use it.
//!
//! As with the workspace's credentials, this tier never holds a plaintext:
//! what arrives is sealed, and what is checked is the binding it was sealed
//! under. The gateway checks it again, and checks whose turn it is, on every
//! use.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use sqlx::Row;
use uuid::Uuid;

use outturn_core::auth::SessionClaims;
use outturn_core::egress::seal::Kind;

use super::credentials::{SealKey, Sealed, decode, seal_key_of};
use super::router::{ApiError, ApiState, authenticate};

/// One of the workspace's rules whose connection is each person's own, and
/// the caller's key for it, if they have connected one.
#[derive(Debug, serde::Serialize)]
pub struct Connection {
    pub rule_id: Uuid,
    pub host: String,
    pub header: String,
    pub connected: Option<Connected>,
}

/// A key somebody connected: never what it is.
#[derive(Debug, serde::Serialize)]
pub struct Connected {
    pub credential_id: Uuid,
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

#[derive(serde::Serialize)]
pub struct Connections {
    pub connections: Vec<Connection>,
    /// What to seal a key to. Public, and handed out here because connecting
    /// one's own needs no authority, where the workspace's seal-key route does.
    /// Absent when this deployment cannot seal.
    pub seal_key: Option<SealKey>,
}

#[derive(Debug, serde::Deserialize)]
pub struct NewConnection {
    /// Chosen by whoever sealed it, because it is inside the binding.
    pub id: Uuid,
    #[serde(flatten)]
    pub seal: Sealed,
}

/// The caller, if they hold a role in the workspace their token is for.
///
/// A role, not just a token: a system administrator may sign in to any
/// workspace, and a key connected where they are not a member is one that
/// leaving -- which revokes a member's keys -- would never reach.
async fn member(
    state: &ApiState,
    headers: &axum::http::HeaderMap,
) -> Result<SessionClaims, ApiError> {
    let claims = authenticate(state, headers)?;
    let member: bool = sqlx::query_scalar(
        "select exists (select 1 from user_workspace_roles \
         where user_id = $1 and workspace_id = $2)",
    )
    .bind(claims.subject)
    .bind(claims.workspace_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !member {
        return Err((
            StatusCode::FORBIDDEN,
            "only a member of this workspace connects keys in it".to_string(),
        ));
    }
    Ok(claims)
}

pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Connections>, ApiError> {
    let claims = member(&state, &headers).await?;
    let connections = of(&state, claims.workspace_id, claims.subject).await?;
    Ok(Json(Connections {
        connections,
        seal_key: seal_key_of(&state).ok(),
    }))
}

/// Connects the caller's key for a rule, replacing any they connected
/// before.
pub async fn connect(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(rule_id): Path<Uuid>,
    Json(input): Json<NewConnection>,
) -> Result<(StatusCode, Json<Connected>), ApiError> {
    let claims = member(&state, &headers).await?;
    let connected = store(
        &state,
        claims.workspace_id,
        claims.subject,
        claims.subject,
        rule_id,
        &input,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(connected)))
}

/// Stores `user`'s key for a rule whose connection is each person's own,
/// replacing any connected before, on behalf of `actor` -- the person
/// themselves, or an administrator for a machine user.
///
/// Refused unless the seal names `user`, this workspace alone, the rule's host
/// and header, and is a static key: what the gateway will ask of it on every
/// request, said while somebody is looking.
pub(crate) async fn store(
    state: &ApiState,
    workspace_id: Uuid,
    user: Uuid,
    actor: Uuid,
    rule_id: Uuid,
    input: &NewConnection,
) -> Result<Connected, ApiError> {
    let rule = sqlx::query(
        "select host, header from egress_rules \
         where id = $1 and workspace_id = $2 and owner = 'asker'",
    )
    .bind(rule_id)
    .bind(workspace_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .ok_or((
        StatusCode::NOT_FOUND,
        "no connection here is one each person makes".to_string(),
    ))?;
    let host: String = rule.get("host");
    let header: String = rule.get("header");

    let (binding, parsed, sealed) = decode(state, input.id, &input.seal)?;
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    if parsed.user != Some(user) {
        return Err(bad(
            "a key connected here is sealed as its owner's own".into()
        ));
    }
    if parsed.workspaces != [workspace_id.to_string()] {
        return Err(bad("a key is bound to this workspace and no other".into()));
    }
    parsed
        .attaches(workspace_id, &host, Some(&header), Kind::Static, Some(user))
        .map_err(bad)?;

    // The old one revoked and the new one stored together, so a turn
    // resolving between the two finds one key or the other, never neither --
    // and the unique index never sees two live.
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    sqlx::query(
        "update credentials set sealed = null, revoked_at = now(), \
             generation = generation + 1, updated_at = now() \
         where rule_id = $1 and user_id = $2 and revoked_at is null",
    )
    .bind(rule_id)
    .bind(user)
    .execute(&mut *tx)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let connected_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "insert into credentials \
             (id, workspace_id, name, binding, sealed, key_id, created_by, user_id, rule_id) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning created_at",
    )
    .bind(input.id)
    .bind(workspace_id)
    .bind(&host)
    .bind(&binding)
    .bind(&sealed)
    .bind(&input.seal.key_id)
    .bind(actor)
    .bind(user)
    .bind(rule_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => (
            StatusCode::CONFLICT,
            "a credential with that id already exists".to_string(),
        ),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    })?;
    tx.commit()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    tracing::info!(
        actor = %actor,
        user_id = %user,
        workspace_id = %workspace_id,
        rule_id = %rule_id,
        credential_id = %input.id,
        "own key connected"
    );
    Ok(Connected {
        credential_id: input.id,
        connected_at,
    })
}

/// Revokes `user`'s key for a rule. False when nothing was connected.
pub(crate) async fn revoke(
    state: &ApiState,
    workspace_id: Uuid,
    user: Uuid,
    rule_id: Uuid,
) -> Result<bool, ApiError> {
    let revoked = sqlx::query(
        "update credentials set sealed = null, revoked_at = now(), \
             generation = generation + 1, updated_at = now() \
         where rule_id = $1 and user_id = $2 and workspace_id = $3 and revoked_at is null",
    )
    .bind(rule_id)
    .bind(user)
    .bind(workspace_id)
    .execute(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(revoked.rows_affected() > 0)
}

/// The rules whose connection is each person's own, and `user`'s key for
/// each.
pub(crate) async fn of(
    state: &ApiState,
    workspace_id: Uuid,
    user: Uuid,
) -> Result<Vec<Connection>, ApiError> {
    let rows = sqlx::query(
        "select r.id, r.host, r.header, c.id as credential_id, c.created_at \
         from egress_rules r \
         left join credentials c on c.rule_id = r.id and c.user_id = $2 \
              and c.revoked_at is null \
         where r.workspace_id = $1 and r.owner = 'asker' \
         order by r.host",
    )
    .bind(workspace_id)
    .bind(user)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(rows
        .iter()
        .map(|row| Connection {
            rule_id: row.get("id"),
            host: row.get("host"),
            header: row.get("header"),
            connected: row
                .get::<Option<Uuid>, _>("credential_id")
                .map(|credential_id| Connected {
                    credential_id,
                    connected_at: row.get("created_at"),
                }),
        })
        .collect())
}

/// Disconnects the caller's key for a rule. Their turns then reach the host
/// with nothing to send, and are told so.
pub async fn disconnect(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(rule_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = member(&state, &headers).await?;
    let revoked = revoke(&state, claims.workspace_id, claims.subject, rule_id).await?;
    if !revoked {
        return Err((
            StatusCode::NOT_FOUND,
            "nothing is connected for that".to_string(),
        ));
    }
    tracing::info!(
        actor = %claims.subject,
        workspace_id = %claims.workspace_id,
        rule_id = %rule_id,
        "own key disconnected"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// What a prompt says before its turn runs, when the turn may reach a host
/// as the person who sent it in a conversation somebody else is in. See
/// docs/user-credentials.md, *Several people in one session*.
///
/// Whatever their key, or the gateway's statement of who is asking, brings
/// back lands in a transcript everybody in the session reads. So the first
/// time it could, the prompt says so, for everybody reading: whose, which
/// hosts, and who else is here. Once per person per host per session -- a
/// host they connect later is said when it is first in reach.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Shared {
    /// The person, by the name they had then.
    pub name: String,
    /// The hosts newly in reach as them.
    pub hosts: Vec<String>,
    /// Everybody else in the session, by name: whoever started it, and
    /// whoever has sent a prompt in it.
    pub with: Vec<String>,
}

/// The key the notice is stored under on a prompt's metadata.
pub const SHARED: &str = "shared";

/// The notice a prompt `asker` is about to send in `session_id` carries, if
/// any.
///
/// The hosts are those a turn of theirs reaches as them: one whose
/// connection is each person's own, where they have connected a key, and one
/// that is told who is asking. A host they have not connected is not reached
/// as them -- the gateway refuses it -- so it is not said until they do.
pub(crate) async fn shared_notice(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    session_id: Uuid,
    asker: Uuid,
) -> Result<Option<Shared>, sqlx::Error> {
    let with: Vec<String> = sqlx::query_scalar(
        "select u.display_name from users u \
         where u.id <> $2 and u.machine_of is null and u.id in ( \
             select user_id from agent_messages \
              where session_id = $1 and role = 'user' and user_id is not null \
             union \
             select user_id from agent_sessions where id = $1 and user_id is not null) \
         order by u.display_name",
    )
    .bind(session_id)
    .bind(asker)
    .fetch_all(pool)
    .await?;
    if with.is_empty() {
        return Ok(None);
    }
    let hosts: Vec<String> = sqlx::query_scalar(
        "select r.host from egress_rules r \
         where r.workspace_id = $1 and r.enabled \
           and (r.on_behalf_of or (r.owner = 'asker' and exists ( \
               select 1 from credentials c \
                where c.rule_id = r.id and c.user_id = $2 and c.revoked_at is null))) \
           and r.host not in ( \
               select jsonb_array_elements_text(m.metadata -> 'shared' -> 'hosts') \
                 from agent_messages m \
                where m.session_id = $3 and m.user_id = $2 and m.role = 'user' \
                  and m.metadata ? 'shared') \
         order by r.host",
    )
    .bind(workspace_id)
    .bind(asker)
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    if hosts.is_empty() {
        return Ok(None);
    }
    let name: String = sqlx::query_scalar("select display_name from users where id = $1")
        .bind(asker)
        .fetch_one(pool)
        .await?;
    Ok(Some(Shared { name, hosts, with }))
}
