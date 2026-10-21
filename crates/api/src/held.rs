//! Requests held for a person to approve, and what became of each once one did.
//!
//! See `docs/approvals.md`, *The approval carries the call*. `gated::raise`
//! holds the refused request beside the queue item it raises; answering the
//! item decides it; the next turn of the conversation is handed it to send
//! before its guest runs, and reports here what came back. The model is then
//! told the answer, by `worker::answered`, rather than asked to make the call
//! again.
//!
//! A held request is keyed to itself, not to the turn that was refused: a
//! person's yes outlives a turn that ended badly, and is good for exactly one
//! answer. This is the store and the report endpoint; what travels between the
//! tiers is in `outturn_core::egress::held`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use uuid::Uuid;

use outturn_core::egress::held::{self as wire, Outcome};

use super::router::{ApiError, ApiState};

/// A request as `gated::raise` holds it.
#[derive(Debug, Clone)]
pub struct NewHeld {
    pub workspace_id: Uuid,
    pub item_id: Uuid,
    pub session_id: Uuid,
    pub job_id: Uuid,
    pub requires: String,
    pub shape: String,
    pub operation: Option<String>,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// Holds one.
pub async fn hold(pool: &sqlx::PgPool, held: NewHeld) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into held_requests \
         (workspace_id, id, item_id, session_id, job_id, requires, shape, operation, \
          method, url, headers, body) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(held.workspace_id)
    .bind(id)
    .bind(held.item_id)
    .bind(held.session_id)
    .bind(held.job_id)
    .bind(&held.requires)
    .bind(&held.shape)
    .bind(&held.operation)
    .bind(&held.method)
    .bind(&held.url)
    .bind(serde_json::to_value(&held.headers).unwrap_or_default())
    .bind(&held.body)
    .execute(pool)
    .await?;
    Ok(id)
}

/// What a person decided about a held request, written with the settle of the
/// item that asked them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Yes: the next turn sends it.
    Approve,
    /// No: it is never sent.
    Decline,
    /// It could not be sent, or nobody knows whether it was, and the person
    /// asked for it to be sent again.
    Resend,
    /// The same, and the person checked and said nothing more is needed.
    Done,
}

impl Decision {
    /// From which states, and to which.
    fn transition(self) -> (&'static [&'static str], &'static str) {
        match self {
            Decision::Approve => (&["held"], "approved"),
            Decision::Decline => (&["held"], "declined"),
            Decision::Resend => (&["unsent", "unknown"], "approved"),
            Decision::Done => (&["unsent", "unknown"], "done"),
        }
    }
}

/// Writes a decision about the request an item holds, inside the settle's
/// transaction. Zero rows is not an error: an item raised by hand, or by a
/// runtime that relayed only the shape, holds nothing.
pub async fn decide_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    item_id: Uuid,
    decision: Decision,
    by: Option<Uuid>,
) -> Result<u64, sqlx::Error> {
    let (from, to) = decision.transition();
    let from: Vec<String> = from.iter().map(|s| s.to_string()).collect();
    // A resend starts its tries over: the person asking has looked, and what
    // was spent before was spent on a connection they may since have fixed.
    let approving = matches!(decision, Decision::Approve | Decision::Resend);
    Ok(sqlx::query(
        "update held_requests set state = $4, \
                approved_by = case when $5 then $6 else approved_by end, \
                approved_at = case when $5 then now() else approved_at end, \
                attempts = case when $5 then 0 else attempts end \
         where workspace_id = $1 and item_id = $2 and state = any($3)",
    )
    .bind(workspace_id)
    .bind(item_id)
    .bind(&from)
    .bind(to)
    .bind(approving)
    .bind(by)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Whether an item holds a request, so answering it decides that rather than
/// minting a grant for its turn.
pub async fn held_for_item(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists (select 1 from held_requests where workspace_id = $1 and item_id = $2)",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_one(pool)
    .await
}

/// A held request as a turn being prepared sees it.
#[derive(Debug, Clone)]
pub struct Held {
    pub id: Uuid,
    pub item_id: Uuid,
    pub requires: String,
    pub shape: String,
    pub state: String,
    pub attempts: i32,
    pub operation: Option<String>,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub outcome: Option<Outcome>,
    /// When a send of it was last settled, which is where a note about it
    /// goes when its refusal cannot be found.
    pub settled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Who said yes, by name, as the model is told it.
    pub approved_by: String,
}

impl Held {
    /// Whether the next turn should send it.
    pub fn waiting(&self) -> bool {
        self.state == "approved"
    }

    /// Whether a runtime said it was sending it and has not said what came of
    /// it.
    fn in_flight(&self) -> bool {
        self.state == "sending"
    }

    /// The grant that lets exactly this request through the gate it was
    /// refused by.
    pub fn grant(&self) -> outturn_core::egress::grant::Granted {
        outturn_core::egress::grant::Granted {
            requires: self.requires.clone(),
            extent: outturn_core::egress::grant::Extent::Call,
            keyed_on: self.shape.clone(),
        }
    }

    /// What the model reads in place of the refusal, where something is
    /// settled enough to say. `None` while it is waiting on a person, or
    /// declined -- the approval record says that.
    pub fn told(&self) -> Option<String> {
        match (self.state.as_str(), &self.outcome) {
            ("spent" | "unsent" | "unknown" | "done", Some(outcome)) => Some(wire::result_text(
                &self.method,
                &self.url,
                &self.approved_by,
                outcome,
            )),
            _ => None,
        }
    }

    pub fn call(&self, tool_call_id: Option<String>) -> wire::HeldCall {
        wire::HeldCall {
            id: self.id,
            method: self.method.clone(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            body: self.body.clone(),
            operation: self.operation.clone(),
            tool_call_id,
            approved_by: self.approved_by.clone(),
        }
    }
}

/// The mark on the note a resent request's turn begins with, so it is drawn
/// as the platform's and framed as the platform's on the way to the model.
pub const RESEND_MARK: &str = "resend";

/// Starts a turn to send a request a person asked to be sent again, now.
///
/// The way a wake starts one: a note in the user position that nobody typed,
/// saying what happened, and a turn queued to answer it. The turn sends the
/// request before its guest runs, as every turn handed an approved one does,
/// and the model reads what came back where the refusal was. Without this the
/// resend waited for whatever the conversation said next, which for a
/// conversation that had stopped was never -- and the person who pressed the
/// button saw nothing happen.
///
/// Written together, so a note exists only with a turn to answer it. False
/// where the item holds no request waiting to go.
pub async fn resend_now(
    state: &ApiState,
    workspace_id: Uuid,
    item_id: Uuid,
    by: Uuid,
) -> Result<bool, sqlx::Error> {
    let protocol = |e: String| sqlx::Error::Protocol(e);
    let mut tx = state.pool.begin().await?;
    let found: Option<(Uuid, Uuid, String, String, String, Uuid)> = sqlx::query_as(
        "select h.id, h.session_id, h.requires, h.method, h.url, s.agent_id \
         from held_requests h join agent_sessions s on s.id = h.session_id \
         where h.workspace_id = $1 and h.item_id = $2 and h.state = 'approved'",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((held_id, session_id, requires, method, url, agent_id)) = found else {
        return Ok(false);
    };
    let who: Option<String> = sqlx::query_scalar("select display_name from users where id = $1")
        .bind(by)
        .fetch_optional(&mut *tx)
        .await?;
    let who = who.unwrap_or_else(|| SOMEBODY.to_string());
    // The zone the conversation's last turn was asked in: nobody is sending
    // this one from anywhere.
    let timezone: Option<String> = sqlx::query_scalar(
        "select payload->>'timezone' from jobs \
         where kind = $1 and payload->>'session_id' = $2 order by id desc limit 1",
    )
    .bind(super::worker::CHAT_TURN)
    .bind(session_id.to_string())
    .fetch_optional(&mut *tx)
    .await?
    .flatten();

    let content = format!(
        "{who} asked for the approved {requires} ({method} {url}) to be sent again. It was sent \
         before this turn began; what came back is where it was refused."
    );
    let metadata = serde_json::json!({
        RESEND_MARK: {
            "held_id": held_id,
            "requires": requires,
            "by": who,
        }
    });
    // No `user_id`: nobody sent it, as for a wake.
    let note_id = Uuid::now_v7();
    sqlx::query(
        "insert into agent_messages (id, session_id, role, content, user_id, metadata) \
         values ($1, $2, 'user', $3, null, $4)",
    )
    .bind(note_id)
    .bind(session_id)
    .bind(&content)
    .bind(&metadata)
    .execute(&mut *tx)
    .await?;
    // Counted towards the pods the fleet wants, as a woken turn is.
    sqlx::query(
        "insert into live_sessions (session_id, expires_at) \
         values ($1, now() + interval '5 minutes') \
         on conflict (session_id) do update set expires_at = excluded.expires_at",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await?;
    let payload = serde_json::to_value(super::worker::ChatTurnPayload {
        workspace_id,
        session_id,
        agent_id,
        message_id: note_id,
        timezone,
        user_id: None,
    })
    .map_err(|e| protocol(e.to_string()))?;
    // A person pressed the button and is watching for the answer.
    outturn_core::jobs::enqueue(
        &mut *tx,
        workspace_id,
        super::worker::CHAT_TURN,
        payload,
        None,
        Some(&session_id.to_string()),
        outturn_core::jobs::PRIORITY_REALTIME,
    )
    .await
    .map_err(|e| protocol(e.to_string()))?;
    outturn_core::events::append_on(
        &mut tx,
        workspace_id,
        Some(session_id),
        "chat.message",
        serde_json::json!({
            "id": note_id,
            "session_id": session_id,
            "role": "user",
            "content": content,
            "metadata": metadata,
        }),
    )
    .await
    .map_err(|e| protocol(e.to_string()))?;
    tx.commit().await?;
    Ok(true)
}

/// Who approved, when the account that did is gone.
const SOMEBODY: &str = "somebody";

/// Every request held in a conversation that a person has answered, oldest
/// first. Read whole rather than only the waiting ones, because the model has
/// to be told what became of each on every turn that replays its refusal.
pub async fn in_session(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    session_id: Uuid,
) -> Result<Vec<Held>, sqlx::Error> {
    type Row = (
        Uuid,
        Uuid,
        String,
        String,
        String,
        i32,
        Option<String>,
        String,
        String,
        serde_json::Value,
        Option<String>,
        Option<serde_json::Value>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select h.id, h.item_id, h.requires, h.shape, h.state, h.attempts, h.operation, \
                h.method, h.url, h.headers, h.body, h.outcome, h.settled_at, u.display_name \
         from held_requests h left join users u on u.id = h.approved_by \
         where h.workspace_id = $1 and h.session_id = $2 and h.state not in ('held', 'declined') \
         order by h.id",
    )
    .bind(workspace_id)
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                item_id,
                requires,
                shape,
                state,
                attempts,
                operation,
                method,
                url,
                headers,
                body,
                outcome,
                settled_at,
                name,
            )| Held {
                id,
                item_id,
                requires,
                shape,
                state,
                attempts,
                operation,
                method,
                url,
                headers: serde_json::from_value(headers).unwrap_or_default(),
                body,
                outcome: outcome.and_then(|o| serde_json::from_value(o).ok()),
                settled_at,
                approved_by: name.unwrap_or_else(|| SOMEBODY.to_string()),
            },
        )
        .collect())
}

/// What the runtime is told back: the sentence the model reads in place of the
/// refusal, decided here because this is where the next step is decided.
#[derive(Debug, Serialize)]
pub struct Recorded {
    pub told: String,
}

/// What a sent request's outcome means for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Answered: spent.
    Spent,
    /// Not sent, or safe to send again: the next turn tries it.
    Again,
    /// Tried as often as it will be, or refused on this side: a person is
    /// asked again, as approved but not sent.
    Unsent,
    /// It may have landed and nothing would dedupe a second: a person checks.
    Unknown,
}

/// Decides it, given how many tries never left before this one and whether
/// the recipient dedupes on a key the request carries.
fn next(outcome: &Outcome, attempts_before: i32, keyed: bool) -> Next {
    let again = |attempts_before: i32| {
        if attempts_before + 1 >= wire::MAX_ATTEMPTS {
            Next::Unsent
        } else {
            Next::Again
        }
    };
    match outcome {
        Outcome::Answered { .. } => Next::Spent,
        Outcome::NeverSent { .. } => again(attempts_before),
        // A recipient that dedupes on the key makes a second send the same
        // write, so it is sent again as one that never left would be.
        Outcome::NoAnswer { .. } if keyed => again(attempts_before),
        Outcome::NoAnswer { .. } => Next::Unknown,
        Outcome::Refused { .. } => Next::Unsent,
    }
}

/// Marks a held request as going, just before a runtime sends it.
///
/// Under the lease, as the report is. Conditional on still waiting, so two
/// runtimes cannot both be told to go, and a request a person decided since
/// is not sent. A runtime refused here does not send.
pub async fn sending(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<wire::Sending>,
) -> Result<StatusCode, ApiError> {
    let (_, payload) = super::work::leased_turn(&state, &headers, input.job_id).await?;
    let marked = sqlx::query(
        "update held_requests set state = 'sending', sending_job = $4, sending_at = now() \
         where workspace_id = $1 and session_id = $2 and id = $3 and state = 'approved'",
    )
    .bind(payload.workspace_id)
    .bind(payload.session_id)
    .bind(input.held_id)
    .bind(input.job_id)
    .execute(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .rows_affected();
    if marked == 0 {
        return Err((
            StatusCode::CONFLICT,
            "no such request is waiting to be sent in this conversation".to_string(),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Settles what a lost turn left in flight, before a turn of its conversation
/// is prepared.
///
/// A request still `sending` here went, or may have, and nobody heard what
/// came back: the turn that marked it is gone, since this conversation's next
/// turn could not be prepared while it ran. So it is answered as no answer
/// would be -- sent again only where the recipient dedupes on a key, and
/// otherwise put in front of a person to check. Never simply sent again: that
/// is the second charge this exists to prevent.
///
/// Best effort. A request left `sending` by a failure here is not sent by the
/// turn about to run, which reads it as neither waiting nor settled; the next
/// turn tries again.
pub async fn recover(state: &ApiState, payload: &super::worker::ChatTurnPayload) {
    let lost = match in_session(&state.pool, payload.workspace_id, payload.session_id).await {
        Ok(held) => held.into_iter().filter(Held::in_flight).collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!(session_id = %payload.session_id, error = %e, "could not read held requests in flight");
            return;
        }
    };
    for held in lost {
        let outcome = Outcome::NoAnswer {
            message: wire::LOST_IN_FLIGHT.to_string(),
        };
        if let Err((_, e)) = settle(state, payload, &held, &outcome).await {
            tracing::warn!(held_id = %held.id, error = %e, "a held request lost in flight was not settled");
        }
    }
}

/// Receives what became of a held request a runtime sent.
///
/// Under the lease, as every other report: the runtime tier's key says only
/// "a runtime", and the lease says this pod is running a turn of the
/// conversation the request was held in. Without the second a runtime holding
/// one turn could spend another conversation's approval.
pub async fn report(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<wire::Report>,
) -> Result<Json<Recorded>, ApiError> {
    let (_, payload) = super::work::leased_turn(&state, &headers, input.job_id).await?;
    let internal = |e: sqlx::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());

    let held = in_session(&state.pool, payload.workspace_id, payload.session_id)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|h| h.id == input.held_id)
        .ok_or((
            StatusCode::NOT_FOUND,
            "no such request is held in this conversation".to_string(),
        ))?;
    if !held.waiting() && !held.in_flight() {
        // Already answered by another turn, or decided by a person since.
        // Refused rather than overwritten: one answer is what a yes is worth.
        return Err((
            StatusCode::CONFLICT,
            format!("that request is {}, not waiting to be sent", held.state),
        ));
    }

    let next = settle(&state, &payload, &held, &input.outcome).await?;

    let told = match next {
        Next::Again => wire::pending_text(&held.approved_by),
        _ => wire::result_text(&held.method, &held.url, &held.approved_by, &input.outcome),
    };
    Ok(Json(Recorded { told }))
}

/// Writes what a send came to, and asks a person again where it did not go
/// or may have.
async fn settle(
    state: &ApiState,
    payload: &super::worker::ChatTurnPayload,
    held: &Held,
    outcome: &Outcome,
) -> Result<Next, ApiError> {
    let internal = |e: sqlx::Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    let keyed = match &held.operation {
        Some(name) => keyed(state, payload, name).await,
        None => false,
    };
    let next = next(outcome, held.attempts, keyed);
    let recorded = serde_json::to_value(outcome).unwrap_or_default();

    let mut tx = state.pool.begin().await.map_err(internal)?;
    let (to, spent) = match next {
        Next::Spent => ("spent", true),
        Next::Again => ("approved", false),
        Next::Unsent => ("unsent", false),
        Next::Unknown => ("unknown", false),
    };
    // Conditional on still waiting or going, so two reports for one request
    // -- a turn retried after its pod was lost, say -- produce one answer.
    // Waiting as well as going, for a runtime that reports without marking.
    let updated = sqlx::query(
        "update held_requests set state = $3, outcome = $4, settled_at = now(), \
                attempts = attempts + case when $5 then 1 else 0 end, \
                spent_at = case when $6 then now() else spent_at end \
         where workspace_id = $1 and id = $2 and state in ('approved', 'sending')",
    )
    .bind(payload.workspace_id)
    .bind(held.id)
    .bind(to)
    .bind(&recorded)
    .bind(matches!(outcome, Outcome::NeverSent { .. }) || next == Next::Again)
    .bind(spent)
    .execute(&mut *tx)
    .await
    .map_err(internal)?
    .rows_affected();
    if updated == 0 {
        return Err((
            StatusCode::CONFLICT,
            "that request was answered meanwhile".to_string(),
        ));
    }
    tx.commit().await.map_err(internal)?;

    if matches!(next, Next::Unsent | Next::Unknown) {
        ask_again(state, held, next == Next::Unknown, outcome).await;
    }

    tracing::info!(
        workspace_id = %payload.workspace_id,
        session_id = %payload.session_id,
        held_id = %held.id,
        state = to,
        "a held request was sent"
    );
    Ok(next)
}

/// Whether the operation a request was declared as sends the recipient a key
/// to dedupe on, read from the skills as they stand.
async fn keyed(state: &ApiState, payload: &super::worker::ChatTurnPayload, name: &str) -> bool {
    use super::skill::SkillStore as _;
    let Ok(skills) = super::skill::PostgresSkillStore::new(state.pool.clone())
        .resolve_for_agent(payload.workspace_id, payload.agent_id)
        .await
    else {
        // Not known, so not assumed: the safe reading is that a second send
        // would be a second write.
        return false;
    };
    super::skill::operations_for_turn(&skills)
        .iter()
        .find(|op| op.name == name)
        .is_some_and(|op| op.key_header.is_some() || op.key_field.is_some())
}

/// Puts an approved request that did not go back in front of a person.
///
/// A new item rather than the old one reopened. The old one is the record of
/// the yes, and a turn's attempt is chosen by whether its approval was
/// answered (`grant::was_answered`); reopening it would unanswer it. The new
/// one carries no turn and takes no hold: the conversation carries on, told
/// what happened, and the person's answer decides the request, not the turn.
async fn ask_again(state: &ApiState, held: &Held, unknown: bool, outcome: &Outcome) {
    let Some((item_workspace, item_payload)) = original(state, held).await else {
        return;
    };
    let why = match outcome {
        Outcome::NeverSent { message }
        | Outcome::NoAnswer { message }
        | Outcome::Refused { message } => message.clone(),
        Outcome::Answered { .. } => return,
    };
    let reason = if unknown {
        format!(
            "{} was approved and sent, and no answer came back, so whether it happened is not \
             known: {why}",
            held.requires
        )
    } else {
        format!(
            "{} was approved, but could not be sent: {why}",
            held.requires
        )
    };
    let mut payload = serde_json::json!({
        "requires": held.requires,
        "reason": reason,
        "session_id": item_payload.get("session_id"),
        "method": item_payload.get("method"),
        "host": item_payload.get("host"),
        "path": item_payload.get("path"),
        "binds": item_payload.get("binds"),
        "held_id": held.id.to_string(),
        "follows": held.item_id.to_string(),
        "followup": if unknown { "unknown" } else { "unsent" },
    });
    if let Some(object) = payload.as_object_mut() {
        object.retain(|_, v| !v.is_null());
    }

    let targets = match super::gated::approvers(state, item_workspace).await {
        Ok(targets) => targets,
        Err((_, e)) => {
            tracing::warn!(held_id = %held.id, error = %e, "nobody could be asked about a held request");
            return;
        }
    };
    let raised = state
        .actions
        .raise(
            item_workspace,
            super::actions::NewItem {
                kind: format!("approval.{}", held.requires),
                event_id: None,
                inhibitor_id: None,
                payload,
                targets,
                expires_at: None,
            },
        )
        .await;
    let item_id = match raised {
        Ok(id) => id,
        Err(e) => {
            // The request stays as it is, unsent or unknown, and its sentence
            // still tells the model so. What is lost is the person being asked,
            // which is worth seeing.
            tracing::warn!(held_id = %held.id, error = %e, "could not ask about a held request again");
            return;
        }
    };
    // The request now answers to the new item, so deciding that one decides it.
    let moved =
        sqlx::query("update held_requests set item_id = $3 where workspace_id = $1 and id = $2")
            .bind(item_workspace)
            .bind(held.id)
            .bind(item_id)
            .execute(&state.pool)
            .await;
    if let Err(e) = moved {
        tracing::warn!(held_id = %held.id, error = %e, "a held request was not tied to its new item");
    }
}

/// The workspace and payload of the item a request was held under.
async fn original(state: &ApiState, held: &Held) -> Option<(Uuid, serde_json::Value)> {
    sqlx::query_as(
        "select a.workspace_id, a.payload from action_items a \
         join held_requests h on h.workspace_id = a.workspace_id and h.item_id = a.id \
         where h.id = $1",
    )
    .bind(held.id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never_sent() -> Outcome {
        Outcome::NeverSent {
            message: "connection refused".into(),
        }
    }

    fn no_answer() -> Outcome {
        Outcome::NoAnswer {
            message: "reset".into(),
        }
    }

    #[test]
    fn any_answer_spends_it() {
        let declined = Outcome::Answered {
            status: 402,
            headers: vec![],
            body: "declined".into(),
            truncated: false,
        };
        assert_eq!(next(&declined, 0, false), Next::Spent);
    }

    #[test]
    fn one_that_never_left_is_tried_three_times_and_then_asked_about() {
        assert_eq!(next(&never_sent(), 0, false), Next::Again);
        assert_eq!(next(&never_sent(), 1, false), Next::Again);
        assert_eq!(next(&never_sent(), 2, false), Next::Unsent);
    }

    #[test]
    fn one_that_may_have_landed_is_sent_again_only_where_the_recipient_dedupes() {
        assert_eq!(next(&no_answer(), 0, false), Next::Unknown);
        assert_eq!(next(&no_answer(), 0, true), Next::Again);
        assert_eq!(next(&no_answer(), 2, true), Next::Unsent);
    }

    #[test]
    fn a_refusal_on_this_side_is_asked_about_at_once() {
        let refused = Outcome::Refused {
            message: "not allowed".into(),
        };
        assert_eq!(next(&refused, 0, true), Next::Unsent);
    }
}
