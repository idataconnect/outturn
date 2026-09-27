//! Asking a person to say yes before an agent acts.
//!
//! See `docs/approvals.md`. Three things happen together and must not come
//! apart: a suspended hold stops the work, a queue item says who is being
//! asked, and the item names the hold so answering one resolves the other.
//!
//! The hold is the truth of whether the request is still open -- `action_items`
//! carries no second copy of that (`docs/action-queue.md`), so releasing the
//! hold is what answering *is*, and the queue row is a read model over it.
//!
//! What raises one, today, is this endpoint, called by hand. The automatic gate
//! is built -- `egress::gate` commits to a turn's gates and the gateway refuses a
//! request that matches one -- but it *refuses*; it does not raise an approval.
//! Nothing turns that refusal into a request for somebody's word, and nothing
//! mints the capability `docs/approvals.md` describes, so a turn that is approved
//! and resumes into the same gate is refused a second time.
//!
//! That is the honest shape of it: the gate stops the money moving, and the
//! approval path is driven from outside. Closing the loop is the capability plus a
//! producer, and until both exist this endpoint is the whole of how an approval
//! comes to be.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Authority;

use super::actions::{NewItem, Target};
use super::inhibitor::{InhibitorStore, Scope, Strength, TakeInhibitor};
use super::router::{ApiError, ApiState, authenticate, authorize, require_in};

#[derive(Debug, Deserialize)]
pub struct RequestApproval {
    /// The conversation whose turn is waiting. Scoped to the session rather
    /// than the agent: what is held is this piece of work, and holding the
    /// agent would stop every other conversation it is having.
    pub session_id: Uuid,
    /// The act being asked about, in a word -- the `requires` of
    /// `docs/approvals.md`. Compared rather than read, so the queue can group
    /// by it and a grant can say what it covered.
    pub requires: String,
    /// Why, for the person deciding. Shown in the queue row and on the hold.
    pub reason: String,
    /// Who is being asked. Roles, not people: who may approve a charge is a
    /// question about the workspace's organisation, and it changes without the
    /// pending request changing.
    #[serde(default)]
    pub roles: Vec<Uuid>,
    /// Named individuals, where the request really is one person's.
    #[serde(default)]
    pub users: Vec<Uuid>,
    /// What the row renders without a second fetch. Only what every target may
    /// see: the queue applies no agent narrowing, so what an agent said belongs
    /// behind the event rather than in here. See `NewItem`.
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct PendingApproval {
    /// The queue item. What a person answers is this.
    pub id: Uuid,
    /// The hold it is waiting on, which is what actually stops the work.
    pub inhibitor_id: Uuid,
}

/// Raises an approval: holds the work, and asks somebody.
///
/// `AgentsInhibit` rather than `ApprovalsAnswer`, because this stops work rather
/// than releasing it. Asking and answering are different acts and the authority
/// that gates one must not gate the other -- otherwise anybody who may approve a
/// payment may also park any conversation in the workspace.
pub async fn request(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<RequestApproval>,
) -> Result<(StatusCode, Json<PendingApproval>), ApiError> {
    let claims = authorize(&state, &headers, Authority::AgentsInhibit).await?;

    if input.requires.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "an approval has to say what it is asking about".to_string(),
        ));
    }
    if input.reason.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "an approval has to say why, or nobody can answer it".to_string(),
        ));
    }

    let targets: Vec<Target> = input
        .roles
        .iter()
        .map(|id| Target::Role(*id))
        .chain(input.users.iter().map(|id| Target::User(*id)))
        .collect();
    if targets.is_empty() {
        // An approval nobody is asked is a turn parked for ever. Refused here
        // rather than raised and orphaned, because the orphan is only findable
        // by somebody who thinks to look.
        return Err((
            StatusCode::BAD_REQUEST,
            "an approval has to be addressed to a role or a person".to_string(),
        ));
    }

    // The session has to be this workspace's, or a caller could park somebody
    // else's conversation by id. Read before anything is written.
    state
        .chat
        .get_session(claims.workspace_id, input.session_id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "no such conversation".to_string()))?;

    let inhibitors = super::inhibitor::PostgresInhibitorStore::new(state.pool.clone());
    let held = inhibitors
        .take(TakeInhibitor {
            scope: Scope::Session {
                workspace_id: claims.workspace_id,
                session_id: input.session_id,
            },
            // The first thing in this codebase to take one. Suspended parks the
            // turn and resumes it when the hold lifts; stopped would end it and
            // latch the session, which is the opposite of what an approval
            // means -- nobody has decided anything yet.
            strength: Strength::Suspended,
            reason: input.reason.clone(),
            held_by: claims.subject.to_string(),
        })
        .await?;

    let payload = match input.payload {
        serde_json::Value::Null => serde_json::json!({}),
        other => other,
    };
    let mut payload = payload;
    if let Some(object) = payload.as_object_mut() {
        // Written by this tier rather than taken from the caller, so a row
        // cannot claim to be about a hold that is not the one holding it.
        object.insert("requires".into(), input.requires.clone().into());
        object.insert("reason".into(), input.reason.clone().into());
        object.insert("inhibitor_id".into(), held.id.to_string().into());
        object.insert("session_id".into(), input.session_id.to_string().into());
    }

    let item = state
        .actions
        .raise(
            claims.workspace_id,
            NewItem {
                kind: format!("approval.{}", input.requires.trim()),
                // Not the hold: `event_id` names an event, and
                // docs/action-queue.md records that this should become
                // `inhibitor_id` once the state columns beside it go. Until
                // then the hold travels in the payload, where the reader can
                // find it and nothing depends on the column's name.
                event_id: None,
                payload,
                targets,
                expires_at: None,
            },
        )
        .await;

    let item = match item {
        Ok(id) => id,
        Err(e) => {
            // The hold is already taken and nothing is going to ask anybody
            // about it, so it would park this conversation for ever. Released
            // rather than left, and the failure is reported rather than
            // swallowed.
            let _ = inhibitors.release(held.id).await;
            return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    };

    tracing::info!(
        workspace_id = %claims.workspace_id,
        session_id = %input.session_id,
        requires = %input.requires,
        inhibitor_id = %held.id,
        item_id = %item,
        "an approval was raised and the conversation was held"
    );

    Ok((
        StatusCode::CREATED,
        Json(PendingApproval {
            id: item,
            inhibitor_id: held.id,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct AnswerApproval {
    /// True approves, false declines. Both settle the item; only one lets the
    /// work proceed.
    pub approved: bool,
    /// What the answerer wants recorded, kept on the item beside who settled it
    /// and when. Optional: "yes" needs no explanation, and a decline without one
    /// leaves the agent unable to say why -- worth encouraging, not worth
    /// refusing over, since an answer nobody could give for want of a sentence is
    /// worse than an answer with no sentence.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Answered {
    /// How many parked turns were given back. Zero on a decline, and zero on an
    /// approval whose turn had already been cancelled.
    pub resumed: u64,
}

/// Answers one. Releasing the hold is what lets the turn run again.
pub async fn answer(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Path(item_id): Path<Uuid>,
    Json(input): Json<AnswerApproval>,
) -> Result<Json<Answered>, ApiError> {
    // Authenticated here and authorised below, once the item is known, because
    // the queue reads across workspaces: the authority has to be resolved in the
    // *item's* workspace rather than the token's. Checked against the token it
    // would mean approvals:answer in one workspace authorised payments in every
    // other one the person belongs to -- which it did, until a test said so.
    let claims = authenticate(&state, &headers)?;

    // Read through the caller's own queue rather than by id alone: an item
    // addressed to a role they do not hold is not theirs to answer, and reading
    // it any other way would make the targeting decorative.
    let mine = state
        .actions
        .queue_for_user_everywhere(claims.subject, MAX_QUEUE_SCAN)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let item = match mine.into_iter().find(|i| i.id == item_id) {
        Some(item) => item,
        None => {
            // The queue holds only what is still open, so an item that was
            // answered a moment ago is absent from it -- and "not yours" and
            // "already decided" are different things to be told. The second is
            // what two people answering at once produces, and reporting it as
            // not-found would have the loser looking for an id they watched
            // somebody else act on.
            //
            // Told apart by asking whether it was ever theirs, which needs the
            // same targeting rule rather than a bare read by id.
            let answered = state
                .actions
                .settled_for_user(claims.subject, item_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            return Err(match answered {
                Some(state) => (
                    StatusCode::CONFLICT,
                    format!("this approval was already {}", state.as_str()),
                ),
                // Not-found rather than forbidden: an item somebody was never
                // asked about is one they have no business knowing exists.
                None => (StatusCode::NOT_FOUND, "no such approval".to_string()),
            });
        }
    };

    // Now that the item is known, and before anything is written.
    require_in(
        &state,
        &claims,
        item.workspace_id,
        Authority::ApprovalsAnswer,
    )
    .await?;

    let held: Option<Uuid> = item
        .payload
        .get("inhibitor_id")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    // Settled, the hold lifted and the parked turn given back, in one
    // transaction. Separately, a release that failed after the settle committed
    // left the item answered and the turn parked with no way back: nothing can
    // answer it again, no queue offers it, and nothing else moves a job out of
    // `parked`. The conversation stayed silent and only an operator releasing the
    // hold by id could free it, with nothing saying so.
    //
    // A decline passes no hold, which is what keeps the conversation paused:
    // nothing has changed about whether the work may proceed, and lifting the
    // hold would let it run having been refused. Stopping the turn is the way out
    // that says so.
    let resumed = state
        .actions
        .settle_and_release(
            item.workspace_id,
            item.id,
            if input.approved {
                super::actions::State::Resolved
            } else {
                super::actions::State::Cancelled
            },
            Some(claims.subject),
            input.note.as_deref(),
            if input.approved { held } else { None },
        )
        .await
        .map_err(|e| match e {
            super::actions::ActionError::NotPending(state) => (
                StatusCode::CONFLICT,
                format!("this approval was already {state}"),
            ),
            other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        })?;

    tracing::info!(
        workspace_id = %item.workspace_id,
        actor = %claims.subject,
        approved = input.approved,
        note = input.note.as_deref().unwrap_or(""),
        resumed,
        "an approval was answered"
    );

    Ok(Json(Answered { resumed }))
}

/// How far into the caller's queue an answer will look for its item.
///
/// The read is bounded, so a person with more waiting than this could not answer
/// the oldest of it. Generous enough that it is not reachable in practice, and
/// bounded because an unbounded read here is one somebody else's backlog sets
/// the cost of.
const MAX_QUEUE_SCAN: i64 = 2_000;
