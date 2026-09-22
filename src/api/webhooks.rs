//! `/v1/webhook-triggers` to manage them, and `/v1/hooks/{path}` to be reached.
//!
//! The two halves are guarded very differently, which is the whole shape of
//! this file. Management is an ordinary authenticated endpoint under the agent
//! authorities. Delivery is reachable by anyone who has the URL, and every
//! line of it is about refusing.

use std::sync::Arc;

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::router::{ApiError, ApiState, authorize};
use super::webhook::{self, Refusal, Trigger, TriggerInput, postgres};
use crate::auth::rbac::Authority;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    agent_id: Option<Uuid>,
}

/// A trigger, plus the URL to give whoever is sending.
///
/// Assembled rather than stored: the host a deployment is reached at is not
/// something the API knows reliably, so the path is what is kept and the rest
/// is said as a suffix somebody completes.
#[derive(Debug, Serialize)]
pub struct WithUrl {
    #[serde(flatten)]
    trigger: Trigger,
    /// What to append to the deployment's own base URL.
    endpoint: String,
}

fn decorate(trigger: Trigger) -> WithUrl {
    let endpoint = format!("/v1/hooks/{}", trigger.path);
    WithUrl { trigger, endpoint }
}

/// A path nobody can guess, and a secret nobody can guess.
///
/// Both from the same source, because the path is not a credential and the
/// secret is -- so the secret must be long enough that it never becomes the
/// weak half of a scheme that exists to be strong.
fn unguessable(bytes: usize) -> String {
    use rand::RngCore;
    let mut raw = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut raw);
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn list_triggers(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<WithUrl>>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    let rows = postgres::list(&state.pool, claims.workspace_id, q.agent_id)
        .await
        .map_err(internal)?;
    Ok(Json(rows.into_iter().map(decorate).collect()))
}

pub async fn get_trigger(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<WithUrl>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsRead).await?;
    match postgres::get(&state.pool, claims.workspace_id, id)
        .await
        .map_err(internal)?
    {
        Some(t) => Ok(Json(decorate(t))),
        None => Err((StatusCode::NOT_FOUND, "no such trigger".to_string())),
    }
}

/// What a trigger's creation returns, once.
///
/// The secret is shown here and never again -- `Trigger` does not serialise
/// it, so a later read cannot recover it. Rotating is how somebody who lost it
/// gets a working trigger back, which is the same trade every platform that
/// issues credentials makes, and for the same reason: a secret readable
/// forever is a secret in every backup and every support transcript.
#[derive(Debug, Serialize)]
pub struct Created {
    #[serde(flatten)]
    trigger: WithUrl,
    secret: String,
}

pub async fn create_trigger(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(input): Json<TriggerInput>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;

    // Proves the agent is this workspace's before pointing a public endpoint
    // at it. Without it the row stores one workspace beside another's agent,
    // and a delivery would run an agent its sender has no business starting.
    state
        .agents
        .get(claims.workspace_id, input.agent_id)
        .await?;

    // And that this person may start sessions with *this* agent. A hook is a
    // standing instruction to do exactly that, so it clears the same bar
    // `sessions::create_session` clears -- `agents:update` is deliberately not
    // narrowed per agent, and a public endpoint pointed at an agent somebody
    // was scoped away from is a way around the narrowing rather than a use of
    // it.
    super::router::require_for_agent(&state, &claims, Authority::SessionsCreate, input.agent_id)
        .await?;

    validate(&input).map_err(bad_request)?;

    // 16 bytes of path, 32 of secret. The path only has to be unguessable;
    // the secret has to stay unguessable against somebody who wants it.
    let path = unguessable(16);
    let secret = unguessable(32);

    let created = postgres::create(
        &state.pool,
        claims.workspace_id,
        Some(claims.subject),
        &path,
        &secret,
        &input,
    )
    .await
    .map_err(internal)?;

    tracing::info!(
        actor = %claims.subject,
        trigger_id = %created.id,
        agent_id = %created.agent_id,
        scheme = %created.scheme,
        "webhook trigger created"
    );

    Ok((
        StatusCode::CREATED,
        Json(Created {
            trigger: decorate(created),
            secret,
        }),
    ))
}

pub async fn update_trigger(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<TriggerInput>,
) -> Result<Json<WithUrl>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    state
        .agents
        .get(claims.workspace_id, input.agent_id)
        .await?;
    super::router::require_for_agent(&state, &claims, Authority::SessionsCreate, input.agent_id)
        .await?;
    validate(&input).map_err(bad_request)?;

    // Which agent a trigger starts is not editable, for the same reason a
    // schedule's is not: the sessions it has already produced hang off the
    // agent it had.
    match postgres::get(&state.pool, claims.workspace_id, id)
        .await
        .map_err(internal)?
    {
        Some(t) if t.agent_id != input.agent_id => {
            return Err(bad_request(
                "a trigger cannot be moved to another agent".to_string(),
            ));
        }
        Some(_) => {}
        None => return Err((StatusCode::NOT_FOUND, "no such trigger".to_string())),
    }

    match postgres::update(&state.pool, claims.workspace_id, id, &input)
        .await
        .map_err(internal)?
    {
        Some(t) => {
            tracing::info!(actor = %claims.subject, trigger_id = %t.id, "webhook trigger updated");
            Ok(Json(decorate(t)))
        }
        None => Err((StatusCode::NOT_FOUND, "no such trigger".to_string())),
    }
}

pub async fn delete_trigger(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    if postgres::delete(&state.pool, claims.workspace_id, id)
        .await
        .map_err(internal)?
    {
        tracing::info!(actor = %claims.subject, trigger_id = %id, "webhook trigger deleted");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "no such trigger".to_string()))
    }
}

/// Issues a new secret, invalidating the one before it.
///
/// Its own endpoint rather than a field on the update, because rotating stops
/// whoever is currently sending until they are given the new one -- which is a
/// decision somebody makes deliberately, not a side effect of renaming
/// something.
pub async fn rotate_secret(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsUpdate).await?;
    let secret = unguessable(32);
    let rotated = sqlx::query(
        "update webhook_triggers set secret = $3, updated_at = now() \
         where workspace_id = $1 and id = $2",
    )
    .bind(claims.workspace_id)
    .bind(id)
    .bind(&secret)
    .execute(&state.pool)
    .await
    .map_err(internal)?;

    if rotated.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "no such trigger".to_string()));
    }
    tracing::info!(actor = %claims.subject, trigger_id = %id, "webhook secret rotated");
    Ok(Json(serde_json::json!({ "secret": secret })))
}

fn validate(input: &TriggerInput) -> Result<(), String> {
    if input.name.trim().is_empty() {
        return Err("a trigger needs a name".to_string());
    }
    if input.prompt.trim().is_empty() {
        return Err("a trigger needs something to say".to_string());
    }
    if !matches!(input.scheme.as_str(), "hmac" | "shared_secret") {
        return Err(format!(
            "'{}' is not a scheme; use hmac or shared_secret",
            input.scheme
        ));
    }
    if input.max_per_hour <= 0 {
        return Err("the hourly ceiling must be at least one".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The public half
// ---------------------------------------------------------------------------

/// Receives a delivery.
///
/// The only endpoint here reachable by somebody this platform never gave a
/// credential to, so the order of what follows is the order of what is
/// cheapest to refuse: size, then existence, then whether it is turned on,
/// then the credential, then the ceiling.
///
/// The ceiling is counted last among the refusals because it is the only one
/// that costs something to be wrong about -- counting an unauthenticated
/// request against a workspace's ceiling would let anybody with the URL
/// exhaust it without ever proving they may use it.
pub async fn deliver(
    State(state): State<Arc<ApiState>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if body.len() > webhook::MAX_BODY_BYTES {
        return refuse_quietly(Refusal::TooLarge, &path);
    }

    let trigger = match postgres::by_path(&state.pool, &path).await {
        Ok(Some(t)) => t,
        Ok(None) => return refuse_quietly(Refusal::NoSuchTrigger, &path),
        Err(e) => {
            tracing::error!(error = %e, "could not read a webhook trigger");
            return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
        }
    };

    // Everything from here until the credential is proved is logged and not
    // written. The ordering below puts the ceiling after verification so an
    // unauthenticated caller cannot spend a workspace's allowance -- and the
    // same reasoning applies to writing at all, which an earlier version
    // missed. A refusal recorded before the credential is proved means anyone
    // holding the URL can drive row-locked updates at line rate, on the row
    // every real delivery needs to update, while filling the operator's only
    // diagnostic with their own noise.
    //
    // It is also what made the two 404s distinguishable: an unknown path did a
    // select and a bad signature did a select and a write, which is 1.3ms of
    // difference over HTTP and a reliable oracle for which paths are real.
    if !trigger.enabled {
        return refuse_quietly(Refusal::Disabled, &path);
    }

    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let replayable_until = match webhook::verify(
        &trigger,
        &body,
        header(webhook::SIGNATURE_HEADER),
        header(webhook::TIMESTAMP_HEADER),
        header(webhook::TOKEN_HEADER),
        Utc::now(),
    ) {
        Ok(until) => until,
        Err(why) => return refuse_quietly(why, &path),
    };

    // Seen before? A signature stays valid until its own timestamp ages out,
    // so without this the same captured request works repeatedly for the whole
    // tolerance window -- and each acceptance is a new session and a new turn
    // against an agent that may act on the world.
    //
    // `replayable_until` is the signed timestamp plus the tolerance, so the
    // record outlives the credential it refuses by exactly nothing. Anchoring
    // it to arrival instead would leave a future-dated signature verifiable
    // after its own record had been swept.
    //
    // Only `hmac` reaches this: `shared_secret` yields no anchor and nothing
    // time-bound to key on, so it is admitted without a replay check rather
    // than refused on legitimate duplicate payloads. See `delivery_digest`.
    //
    // After verification, so an unauthenticated caller cannot fill the table;
    // before the ceiling, so a replay does not consume the hourly allowance a
    // legitimate delivery needs.
    // The scheme decides, not the presence of a header: `shared_secret` is
    // exempt because it binds no time, and reading that off the signature
    // would make the exemption a consequence of `verify`'s control flow rather
    // than a decision. A scheme that later returned no anchor would then
    // silently admit unlimited replays.
    let mut claimed = None;
    let claimant = uuid::Uuid::now_v7();
    if trigger.scheme != "shared_secret"
        && let (Some(signature), Some(until)) =
            (header(webhook::SIGNATURE_HEADER), replayable_until)
    {
        let digest = webhook::delivery_digest(signature);
        match postgres::remember(&state.pool, trigger.id, &digest, until, claimant).await {
            Ok(true) => claimed = Some(digest),
            Ok(false) => return refuse(&state, &trigger, Refusal::Replayed).await,
            Err(e) => {
                tracing::error!(error = %e, trigger_id = %trigger.id, "could not record a delivery");
                return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
            }
        }
    }

    match postgres::admit(&state.pool, trigger.id).await {
        Ok(true) => {}
        Ok(false) => {
            release(&state, claimed.as_deref(), trigger.id, claimant).await;
            return refuse(&state, &trigger, Refusal::RateLimited).await;
        }
        Err(e) => {
            tracing::error!(error = %e, trigger_id = %trigger.id, "could not count a delivery");
            release(&state, claimed.as_deref(), trigger.id, claimant).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
        }
    }

    // Lossy on purpose: a body that is not UTF-8 still becomes a prompt, badly
    // rather than not at all. Refusing here would mean a sender's encoding
    // choice silently stopping their integration, and the agent reading
    // mangled text can at least say so.
    let text = String::from_utf8_lossy(&body).into_owned();

    let started = crate::api::trigger::start(
        &state.pool,
        crate::api::trigger::Started {
            workspace_id: trigger.workspace_id,
            agent_id: trigger.agent_id,
            title: trigger.name.clone(),
            prompt: webhook::prompt_for(&trigger.prompt, &text),
            account: trigger.account.clone(),
            // No zone: nobody sent this from anywhere, so the agent's clock
            // reads UTC rather than pretending to know better.
            timezone: None,
            source: crate::api::trigger::Source::Webhook(trigger.id),
            metadata: serde_json::json!({
                "webhook_trigger_id": trigger.id,
                "webhook_trigger_name": trigger.name,
            }),
        },
    )
    .await;

    match started {
        Ok(session_id) => {
            tracing::info!(
                trigger_id = %trigger.id,
                session_id = %session_id,
                bytes = body.len(),
                "a webhook started a turn"
            );
            let _ = postgres::record_delivery(&state.pool, trigger.id, "ok", None).await;
            // 202 rather than 200: the turn has been accepted and has not
            // happened yet, and a sender that reads 200 as "done" would be
            // told something untrue.
            (StatusCode::ACCEPTED, "").into_response()
        }
        Err(e) => {
            tracing::error!(trigger_id = %trigger.id, error = %e, "a webhook could not start a turn");
            let _ = postgres::record_delivery(&state.pool, trigger.id, "failed", Some(&e)).await;
            release(&state, claimed.as_deref(), trigger.id, claimant).await;
            (StatusCode::INTERNAL_SERVER_ERROR, "").into_response()
        }
    }
}

/// Releases a claimed delivery on a path that did not start a turn.
///
/// `remember` runs before the ceiling and before the turn starts, so a replay
/// spends neither. The cost is that a delivery refused by the ceiling, or one
/// whose turn failed to start, has already claimed its slot -- and left there,
/// the byte-identical retry that a 429 or a 500 asks for is refused as a
/// replay and the event is lost silently. Releasing is what keeps the claim
/// meaning "this delivery became a turn" rather than "this delivery arrived".
///
/// `None` means nothing was claimed, which is every `shared_secret` delivery.
async fn release(
    state: &ApiState,
    claimed: Option<&[u8]>,
    trigger_id: uuid::Uuid,
    claimant: uuid::Uuid,
) {
    let Some(digest) = claimed else { return };
    if let Err(e) = postgres::forget(&state.pool, trigger_id, digest, claimant).await {
        // Not fatal: the sweep removes it once the signature ages out, so the
        // cost is that this one delivery cannot be retried until then.
        tracing::warn!(error = %e, trigger_id = %trigger_id, "could not release a delivery");
    }
}

/// Refuses a delivery that proved its credential, recording why.
///
/// Only reached past verification, which is what makes the write safe to do:
/// whoever triggered it holds the secret, so they are a sender this workspace
/// chose rather than anybody with the URL. That is also what makes `refused`
/// mean one thing -- it counts what the ceiling turned away and nothing else,
/// so an operator seeing it climb knows to look at the rate rather than at a
/// clock or a signature.
async fn refuse(state: &ApiState, trigger: &Trigger, why: Refusal) -> axum::response::Response {
    tracing::warn!(
        trigger_id = %trigger.id,
        workspace_id = %trigger.workspace_id,
        reason = why.reason(),
        "a webhook delivery was refused"
    );
    // Only the ceiling's refusals move the counter the column is named for.
    let counts = why == Refusal::RateLimited;
    let _ = postgres::record_refusal(&state.pool, trigger.id, why.reason(), counts).await;
    (why.status(), "").into_response()
}

/// Refuses without writing anything.
///
/// Every refusal decided before the credential is proved comes here, whether
/// or not there is a row it could have been recorded against. A path somebody
/// is probing should not create work, and a path they are probing with a bad
/// signature should not either -- which is the same sentence, and the reason
/// the two 404s are now indistinguishable in time as well as in status.
///
/// The operator still learns of it, in the log, which is where somebody
/// allowed to know the difference can look.
fn refuse_quietly(why: Refusal, path: &str) -> axum::response::Response {
    tracing::warn!(
        reason = why.reason(),
        path,
        "a webhook delivery was refused"
    );
    (why.status(), "").into_response()
}

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn bad_request(e: String) -> ApiError {
    (StatusCode::BAD_REQUEST, e)
}
