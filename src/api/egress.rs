//! Where a tenant's egress rules are kept and read.
//!
//! Read here rather than in the runtime, because the runtime holds no
//! database: it is given a conversation and returns a reply. The rules travel
//! with the turn like the system prompt does, so the tier that owns the
//! transcript stays the only thing talking to Postgres.

use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::runtime::egress::EgressRule;

/// The hosts this tenant's agents may reach.
///
/// An empty list is the ordinary case for a new tenant and means exactly what
/// it says. Nothing is inherited from a system default: a default that reached
/// somewhere would be a decision made on a tenant's behalf about who their
/// agents may talk to.
pub async fn rules_for(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<EgressRule>, sqlx::Error> {
    let rows = sqlx::query(
        "select host, header, credential_env from egress_rules \
         where tenant_id = $1 and enabled \
         order by host",
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| EgressRule {
            host: r.get("host"),
            header: r.get("header"),
            credential_env: r.get("credential_env"),
        })
        .collect())
}
