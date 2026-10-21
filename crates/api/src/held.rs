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

use uuid::Uuid;

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
