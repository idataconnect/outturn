//! Turning a gateway refusal into somebody being asked.
//!
//! See `docs/approvals.md`. The gate is enforced in the gateway, because that is
//! the tier that makes the request, holds the credential and can refuse before
//! money moves. But the gateway holds a method, a URL and a token: it does not
//! own the job or the queue, and giving it a write path to either would put
//! policy in the tier `egress/mod.rs` is careful to keep free of it. So it
//! refuses, in words the guest can read, and stops there.
//!
//! This is what carries the refusal the rest of the way. The runtime host
//! relays the shape of the request that was refused; this tier decides whether
//! that shape really is gated for this turn, and only then writes the hold and
//! the queue item.
//!
//! **The relay is not trusted.** The runtime executes workspace code, so a claim
//! it makes about what was refused is a claim from the tier being defended
//! against. If this took the host's word, a compromised runtime could raise
//! approvals naming a `requires` nobody declared -- parking sessions at will and
//! filling somebody's queue with questions about acts that do not exist. So the
//! gates are re-derived here from the turn's own skills, exactly as
//! `worker::prepare_turn` derived them, and a shape that matches none is refused
//! and logged rather than raised. What a compromised runtime can do is decline
//! to ask, which it could do anyway by not making the call.
//!
//! The turn parks at its next round boundary rather than here. A round cut
//! partway has its tool calls refused wholesale -- see the truncation guard in
//! `agents/default/src/lib.rs` -- so parking mid-round would open exactly the
//! window `docs/idempotency.md` is about. The refusal goes back as a tool result,
//! the guest finishes the round, and the hold takes effect at the boundary. That
//! also leaves the agent free to finish other work it can still do before it
//! stops.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Authority;

use super::actions::{NewItem, Target};
use super::inhibitor::{InhibitorStore, Scope, Strength, TakeInhibitor};
use super::router::{ApiError, ApiState};

/// What the runtime says was refused.
///
/// The shape of a request rather than a conclusion about it: this tier decides
/// what it means. `requires` is deliberately absent -- naming the act is the
/// decision being taken away from the runtime.
#[derive(Debug, Deserialize)]
pub struct Refused {
    pub session_id: Uuid,
    pub job_id: Uuid,
    pub method: String,
    pub host: String,
    pub path: String,
    /// The body the request would have carried, for reading a `covers` unit out
    /// of. Read here rather than accepted as a named unit, because a guest that
    /// could name its own unit could name the one already approved.
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Raised {
    /// The queue item, when one was raised. Absent when this turn already holds
    /// a grant covering the request, which is what a resumed turn looks like.
    pub item_id: Option<Uuid>,
    /// Whether the turn should park at its next boundary.
    pub park: bool,
}

/// Raises an approval for a refused request, if it really is gated.
pub async fn raise(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<Refused>,
) -> Result<Json<Raised>, ApiError> {
    // The same authority `/v1/work` takes, because this is the runtime tier
    // talking and no workspace role may reach it. A workspace Admin raising an
    // approval does it through `/v1/approvals`, which is authorised differently
    // and says who asked.
    super::router::authorize(&state, &headers, Authority::WorkTake).await?;

    let job = crate::jobs::get(&state.pool, input.job_id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "no such turn".to_string()))?;

    // The lease, as `work::report` and `work::abandon` require it. Without it
    // `WorkTake` alone is enough to raise an approval against *any* job, so a
    // runtime holding one turn could park other tenants' conversations and fill
    // their approvers' queues -- AGENTS.md calls the lease "the only thing
    // joining a claim to the runtime running it", and this endpoint was taking
    // the runtime's word for which turn it was speaking about.
    let lease = super::work::lease_from(&headers)?;
    if job.lease_token != Some(lease) {
        return Err((
            StatusCode::CONFLICT,
            "that turn is not leased to this runtime".to_string(),
        ));
    }

    let payload: super::worker::ChatTurnPayload = serde_json::from_value(job.payload.clone())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("payload: {e}")))?;

    // The session the runtime named has to be the one the job is actually for.
    // Otherwise a runtime holding one turn's token could raise an approval
    // against another conversation.
    if payload.session_id != input.session_id {
        return Err((
            StatusCode::FORBIDDEN,
            "that turn is not for that conversation".to_string(),
        ));
    }

    // Re-derived rather than taken from the caller. This is the whole of why the
    // endpoint exists: what is gated for this turn is a statement this tier
    // makes, and the runtime relaying a refusal does not get to decide what it
    // was a refusal of.
    let gates = super::worker::gates_for(&state.pool, &payload)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let method = input.method.to_ascii_uppercase();
    let Some(gate) = gates.covering(&input.host, &method, &input.path) else {
        // Not gated, so there is nothing to approve. Logged at warn because the
        // only ways here are a runtime that is confused and a runtime that is
        // lying, and both are worth seeing.
        tracing::warn!(
            workspace_id = %payload.workspace_id,
            session_id = %payload.session_id,
            method = %method,
            host = %input.host,
            "a refusal was relayed for a request no gate covers"
        );
        return Err((
            StatusCode::FORBIDDEN,
            "that request is not one this turn has to have approved".to_string(),
        ));
    };

    // No grant check here. The gateway holds the grants this turn carries, in
    // the same signed commitment as the gates, so a request somebody had already
    // approved never reached a refusal and never got here. A refusal that did is
    // one nothing covers.

    // What a grant would be keyed on, recorded on the item so answering it can
    // mint one without re-deriving any of this.
    let shape = crate::egress::grant::digest(
        gate,
        &method,
        &input.host,
        &input.path,
        input.body.as_deref(),
    );
    let unit = gate
        .identified_by
        .as_deref()
        .and_then(|field| super::grant::unit_from_body(input.body.as_deref(), field));

    // One pending approval per conversation. Without this an agent that retries
    // a refused call in the same round raises a second identical question, and
    // the person answering cannot tell which is which.
    //
    // Per conversation rather than per act, and that is the wider of the two on
    // purpose: the turn is about to park either way, so a second act's question
    // would sit unanswerable behind the first until somebody cleared it. Asked
    // through the hold, which is what says a request is still open.
    if let Some(existing) = state
        .actions
        .approval_on_session(payload.workspace_id, payload.session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return Ok(Json(Raised {
            item_id: Some(existing.id),
            park: true,
        }));
    }

    let inhibitors = super::inhibitor::PostgresInhibitorStore::new(state.pool.clone());
    let reason = format!("{} {} on {} needs approval", method, input.path, input.host);
    let held = inhibitors
        .take(TakeInhibitor {
            scope: Scope::Session {
                workspace_id: payload.workspace_id,
                session_id: payload.session_id,
            },
            strength: Strength::Suspended,
            reason: reason.clone(),
            // Recorded as the platform rather than as a person: nobody asked for
            // this hold, a rule did. `docs/triggers.md` draws the same line for a
            // turn nobody is waiting on.
            held_by: "gate".to_string(),
        })
        .await?;

    // Who gets asked. The workspace's approvers, resolved as a role rather than
    // as the people in it, for the reason `action_targets` exists: membership
    // outlives the moment.
    let targets = approvers(&state, payload.workspace_id).await?;
    if targets.is_empty() {
        // Nobody can answer, so the turn would park for ever. The hold is given
        // back and the request stays refused -- which is the safe direction, and
        // the agent already has words saying so.
        let _ = inhibitors.release(held.id).await;
        tracing::warn!(
            workspace_id = %payload.workspace_id,
            "a request needed approval but the workspace has nobody who may approve"
        );
        return Err((
            StatusCode::FORBIDDEN,
            "that request needs approval and nobody in this workspace may give it".to_string(),
        ));
    }

    let mut payload_json = serde_json::json!({
        "requires": gate.requires,
        "reason": reason,
        "inhibitor_id": held.id.to_string(),
        "session_id": payload.session_id.to_string(),
        "job_id": input.job_id.to_string(),
        "method": method,
        "host": input.host,
        "path": input.path,
        "shape": shape,
    });
    // Offered, never granted. The approver ticks it; it is never inferred from
    // what was asked, because a person approves the instance they were shown and
    // not the class it belongs to.
    if let (Some(field), Some(value)) = (gate.identified_by.as_deref(), unit.as_deref()) {
        payload_json["covers"] = serde_json::json!({ "field": field, "unit": value });
    }

    let item = state
        .actions
        .raise(
            payload.workspace_id,
            NewItem {
                kind: format!("approval.{}", gate.requires),
                event_id: None,
                inhibitor_id: Some(held.id),
                payload: payload_json,
                targets,
                expires_at: None,
            },
        )
        .await;

    let item = match item {
        Ok(id) => id,
        Err(e) => {
            // Nothing is going to ask anybody, so the hold would park this
            // conversation for ever.
            let _ = inhibitors.release(held.id).await;

            // Losing the race is not a failure. The check above and this write
            // are two statements, so a second refusal arriving between them --
            // a guest making two gated calls in one round does exactly that --
            // gets here with somebody else's approval already pending. The
            // unique index is what makes that impossible rather than unlikely,
            // and this is the loser reading its own violation: the question was
            // asked, which is all this caller wanted.
            if let Some(existing) = state
                .actions
                .approval_on_session(payload.workspace_id, payload.session_id)
                .await
                .ok()
                .flatten()
            {
                return Ok(Json(Raised {
                    item_id: Some(existing.id),
                    park: true,
                }));
            }
            return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };

    tracing::info!(
        workspace_id = %payload.workspace_id,
        session_id = %payload.session_id,
        requires = %gate.requires,
        inhibitor_id = %held.id,
        item_id = %item,
        "a gated request raised an approval"
    );

    Ok(Json(Raised {
        item_id: Some(item),
        park: true,
    }))
}

/// The roles in this workspace whose holders may answer an approval.
///
/// Resolved from the authority rather than from a named role, so a workspace
/// that made its own role with `approvals:answer` in it is asked too. A role is
/// what is stored, never its members.
async fn approvers(state: &ApiState, workspace_id: Uuid) -> Result<Vec<Target>, ApiError> {
    let roles = state
        .roles
        .roles_with(workspace_id, Authority::ApprovalsAnswer)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(roles.into_iter().map(Target::Role).collect())
}

/// What a reader needs to answer an approval from the conversation it holds.
///
/// Only what the row already carries, and only what every target of it may see:
/// the queue applies no agent narrowing, so what an agent said stays behind the
/// event rather than travelling on a held banner.
pub fn answerable(item: &super::actions::ActionItem) -> serde_json::Value {
    serde_json::json!({
        "item_id": item.id,
        "requires": item.payload.get("requires"),
        "reason": item.payload.get("reason"),
        // The offer, so the client can show the tickbox. Never a grant: it is
        // the approver's act that widens the extent, not the request's.
        "covers": item.payload.get("covers"),
    })
}
