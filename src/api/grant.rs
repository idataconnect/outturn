//! What a yes is worth once somebody has given it.
//!
//! See `docs/approvals.md`. Releasing the hold lets a parked turn run again,
//! but nothing about the request it was parked for has changed -- so without
//! this the resumed turn reaches the same gate and is refused a second time.
//! The hold answers "may this turn proceed"; a grant answers "may this request
//! go out". Two questions, and answering the first is not answering the second.
//!
//! A grant is narrow on purpose. Both extents die with the turn, so nothing
//! accumulates and nothing granted at nine reaches a call at five. Wider
//! extents -- this session, until revoked -- are deliberately absent: they are
//! authorities rather than approvals, and the place to add one is the roles
//! model, where a list of who holds what already exists.
//!
//! This is the store. What a grant *is*, and how the tier making a request
//! checks one, is in `egress::grant` -- beside the gates, because both tiers
//! read it and only this one writes it.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::egress::grant::{Extent, Granted, digest, unit_from_body};

/// What `gated::raise` records on the queue item so answering it can mint a
/// grant without re-deriving anything.
///
/// One definition rather than a set of `payload->>'...'` reads on each side. The
/// two used to be written and parsed independently, so renaming a key degraded
/// silently into the "raised by hand, nothing to grant" path -- which is a
/// success path, and indistinguishable from the typo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateRecord {
    pub requires: String,
    /// What a `call` grant will be keyed on: the shape that was refused.
    pub shape: String,
    pub job_id: Uuid,
    pub session_id: Uuid,
    /// The wider extent the gate offered, where it declared one. An offer, never
    /// a grant: the approver ticks it.
    #[serde(default)]
    pub covers: Option<Covers>,
}

/// The unit a `covers` offer is about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Covers {
    /// The field the declaration named, shown so a person can see what they are
    /// widening to.
    pub field: String,
    pub unit: String,
}

/// A grant as it is written.
#[derive(Debug, Clone)]
pub struct NewGrant {
    pub workspace_id: Uuid,
    pub session_id: Uuid,
    pub job_id: Uuid,
    pub requires: String,
    pub extent: Extent,
    pub keyed_on: String,
    pub granted_by: Option<Uuid>,
}

/// Writes one.
pub async fn write(pool: &sqlx::PgPool, grant: NewGrant) -> Result<Uuid, sqlx::Error> {
    write_on(pool, grant).await
}

/// The same, inside somebody else's transaction.
///
/// Which is where a grant belongs: it has to commit with the settle that earned
/// it and the resume that acts on it, or the turn runs before its grant exists.
pub async fn write_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    grant: NewGrant,
) -> Result<Uuid, sqlx::Error> {
    write_on(&mut **tx, grant).await
}

async fn write_on<'e, E>(executor: E, grant: NewGrant) -> Result<Uuid, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into approval_grants \
         (workspace_id, id, session_id, job_id, requires, extent, keyed_on, granted_by) \
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(grant.workspace_id)
    .bind(id)
    .bind(grant.session_id)
    .bind(grant.job_id)
    .bind(&grant.requires)
    .bind(grant.extent.as_str())
    .bind(&grant.keyed_on)
    .bind(grant.granted_by)
    .execute(executor)
    .await?;
    Ok(id)
}

/// The grants this turn still holds, for the token it travels with.
///
/// Whole grants rather than the acts they name. Returning bare `requires` was
/// the bug: the caller dropped every gate declaring that act, so one approved
/// GET disabled `approve_new_hosts` for every host and method the turn could
/// reach, and a unit grant for one booking covered every other booking too.
/// What a grant permits is a shape or a unit, and the extent is the half that
/// says which.
pub async fn live_for(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    job_id: Uuid,
) -> Result<Vec<Granted>, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "select requires, extent, keyed_on from approval_grants \
         where workspace_id = $1 and job_id = $2 and spent_at is null \
         order by granted_at",
    )
    .bind(workspace_id)
    .bind(job_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|(requires, extent, keyed_on)| {
            // Dropped rather than guessed at: a grant nobody can interpret must
            // not become one that permits everything. Said out loud because the
            // only way here is a pod older than the row that wrote it, and a
            // silently narrower grant looks to the person who gave it like an
            // approval that did not take.
            let Some(extent) = Extent::parse(&extent) else {
                tracing::warn!(
                    extent = %extent,
                    "a grant was left out because this build does not know its extent"
                );
                return None;
            };
            Some(Granted {
                requires,
                extent,
                keyed_on,
            })
        })
        .collect())
}
