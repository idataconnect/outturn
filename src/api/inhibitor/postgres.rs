use async_trait::async_trait;
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use super::{Inhibitor, InhibitorError, InhibitorStore, Scope, Strength, TakeInhibitor};

pub struct PostgresInhibitorStore {
    pool: PgPool,
}

impl PostgresInhibitorStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> InhibitorError {
    InhibitorError::Internal(e.to_string())
}

/// The scope columns, as the level says to read them.
///
/// The check constraint guarantees the columns a level needs are present, so a
/// row that does not match one is a row the database should not hold -- said
/// outright rather than guessed at, because guessing here means a hold that
/// covers something other than what it says.
fn read_scope(row: &PgRow) -> Result<Scope, InhibitorError> {
    let level: String = row.get("level");
    let workspace_id: Option<Uuid> = row.get("workspace_id");
    let agent_id: Option<Uuid> = row.get("agent_id");
    let session_id: Option<Uuid> = row.get("session_id");

    match (level.as_str(), workspace_id, agent_id, session_id) {
        ("platform", None, None, None) => Ok(Scope::Platform),
        ("workspace", Some(workspace_id), None, None) => Ok(Scope::Workspace { workspace_id }),
        ("agent", Some(workspace_id), Some(agent_id), None) => {
            Ok(Scope::Agent { workspace_id, agent_id })
        }
        ("session", Some(workspace_id), None, Some(session_id)) => {
            Ok(Scope::Session { workspace_id, session_id })
        }
        _ => Err(InhibitorError::Internal(format!(
            "an inhibitor row says {level} but does not carry that level's columns"
        ))),
    }
}

fn read(row: &PgRow) -> Result<Inhibitor, InhibitorError> {
    let strength: String = row.get("strength");
    Ok(Inhibitor {
        id: row.get("id"),
        scope: read_scope(row)?,
        strength: strength.parse::<Strength>().map_err(InhibitorError::Internal)?,
        reason: row.get("reason"),
        held_by: row.get("held_by"),
        created_at: row.get("created_at"),
    })
}

/// The level, and the three columns it does or does not fill.
fn columns(scope: &Scope) -> (&'static str, Option<Uuid>, Option<Uuid>, Option<Uuid>) {
    match *scope {
        Scope::Platform => ("platform", None, None, None),
        Scope::Workspace { workspace_id } => ("workspace", Some(workspace_id), None, None),
        Scope::Agent { workspace_id, agent_id } => {
            ("agent", Some(workspace_id), Some(agent_id), None)
        }
        Scope::Session { workspace_id, session_id } => {
            ("session", Some(workspace_id), None, Some(session_id))
        }
    }
}

/// The columns every read returns, joined to its own tail at compile time.
///
/// A macro rather than a `format!`, because sqlx refuses SQL built at runtime
/// -- rightly, and the tails here are literals anyway.
macro_rules! select_inhibitors {
    ($tail:literal) => {
        concat!(
            "select id, level, workspace_id, agent_id, session_id, strength, ",
            "reason, held_by, created_at from inhibitors ",
            $tail
        )
    };
}

#[async_trait]
impl InhibitorStore for PostgresInhibitorStore {
    async fn take(&self, input: TakeInhibitor) -> Result<Inhibitor, InhibitorError> {
        let reason = input.reason.trim();
        if reason.is_empty() {
            // Refused rather than defaulted: a hold whose reason is blank is
            // one nobody can act on, and the person looking at a stopped
            // conversation is owed better than an empty string.
            return Err(InhibitorError::Invalid("an inhibitor needs a reason".into()));
        }
        if input.held_by.trim().is_empty() {
            return Err(InhibitorError::Invalid("an inhibitor needs a holder".into()));
        }

        let (level, workspace_id, agent_id, session_id) = columns(&input.scope);
        let row = sqlx::query(
            "insert into inhibitors \
                 (id, level, workspace_id, agent_id, session_id, strength, reason, held_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8) \
             returning id, level, workspace_id, agent_id, session_id, strength, \
                       reason, held_by, created_at",
        )
        .bind(Uuid::now_v7())
        .bind(level)
        .bind(workspace_id)
        .bind(agent_id)
        .bind(session_id)
        .bind(input.strength.as_str())
        .bind(reason)
        .bind(input.held_by.trim())
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        read(&row)
    }

    async fn release(&self, id: Uuid) -> Result<(), InhibitorError> {
        let done = sqlx::query("delete from inhibitors where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(internal)?;

        if done.rows_affected() == 0 {
            return Err(InhibitorError::NotFound);
        }
        Ok(())
    }

    async fn covering(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
        session_id: Uuid,
    ) -> Result<Vec<Inhibitor>, InhibitorError> {
        // Every level above this turn in one query. A checkpoint that asked
        // level by level could get a different answer for each, and a kill
        // switch that arrives between two of those reads is one the turn walks
        // straight past.
        let rows = sqlx::query(select_inhibitors!(
            "where level = 'platform' \
                or (level = 'workspace' and workspace_id = $1) \
                or (level = 'agent' and workspace_id = $1 and agent_id = $2) \
                or (level = 'session' and workspace_id = $1 and session_id = $3) \
             order by created_at"
        ))
        .bind(workspace_id)
        .bind(agent_id)
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        rows.iter().map(read).collect()
    }

    async fn get(&self, id: Uuid) -> Result<Inhibitor, InhibitorError> {
        let row = sqlx::query(select_inhibitors!("where id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or(InhibitorError::NotFound)?;
        read(&row)
    }

    async fn in_workspace(&self, workspace_id: Uuid) -> Result<Vec<Inhibitor>, InhibitorError> {
        let rows = sqlx::query(select_inhibitors!(
            "where workspace_id = $1 order by created_at"
        ))
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        rows.iter().map(read).collect()
    }

    async fn at(&self, scope: Scope) -> Result<Vec<Inhibitor>, InhibitorError> {
        let (level, workspace_id, agent_id, session_id) = columns(&scope);
        let rows = sqlx::query(select_inhibitors!(
            "where level = $1 \
                and workspace_id is not distinct from $2 \
                and agent_id is not distinct from $3 \
                and session_id is not distinct from $4 \
             order by created_at"
        ))
        .bind(level)
        .bind(workspace_id)
        .bind(agent_id)
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        rows.iter().map(read).collect()
    }
}
