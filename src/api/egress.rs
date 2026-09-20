//! Where a workspace's egress rules are kept and read.
//!
//! Read here rather than in the runtime, because the runtime holds no
//! database: it is given a conversation and returns a reply. The rules travel
//! with the turn like the system prompt does, so the tier that owns the
//! transcript stays the only thing talking to Postgres.

use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::runtime::egress::EgressRule;

/// The hosts this workspace's agents may reach.
///
/// An empty list is the ordinary case for a new workspace and means exactly what
/// it says. Nothing is inherited from a system default: a default that reached
/// somewhere would be a decision made on a workspace's behalf about who their
/// agents may talk to.
pub async fn rules_for(pool: &PgPool, workspace_id: Uuid) -> Result<Vec<EgressRule>, sqlx::Error> {
    let rows = sqlx::query(
        "select host, header, credential_env from egress_rules \
         where workspace_id = $1 and enabled \
         order by host",
    )
    .bind(workspace_id)
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

/// A rule as a workspace sees it.
///
/// `credential_env` is the name of an environment variable, never a secret, so
/// this is safe to return, log and show in a browser. That is the point of
/// naming credentials rather than storing them: the thing describing the rule
/// cannot leak the thing that makes it work.
#[derive(Debug, serde::Serialize)]
pub struct Rule {
    pub id: Uuid,
    pub host: String,
    pub header: Option<String>,
    pub credential_env: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct CreateRule {
    /// A hostname, or anything a hostname can be recovered from -- a pasted
    /// URL is the ordinary case.
    pub host: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub credential_env: Option<String>,
}

#[derive(Debug)]
pub enum RuleError {
    /// The host is not one, or would not mean what its author thought.
    Invalid(String),
    /// This workspace already has a rule for this host.
    Duplicate(String),
    Database(String),
}

impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleError::Invalid(m) | RuleError::Duplicate(m) | RuleError::Database(m) => {
                write!(f, "{m}")
            }
        }
    }
}

pub async fn list(pool: &PgPool, workspace_id: Uuid) -> Result<Vec<Rule>, RuleError> {
    let rows = sqlx::query(
        "select id, host, header, credential_env, enabled from egress_rules \
         where workspace_id = $1 order by host",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .map_err(|e| RuleError::Database(e.to_string()))?;

    Ok(rows.iter().map(read_rule).collect())
}

pub async fn create(
    pool: &PgPool,
    workspace_id: Uuid,
    input: CreateRule,
) -> Result<Rule, RuleError> {
    let host = crate::runtime::egress::normalise_host(&input.host).map_err(RuleError::Invalid)?;

    // A header without a variable would attach nothing; a variable without a
    // header has nowhere to go. Either alone is a rule that looks configured
    // and is not, which is worse than one that plainly is not.
    match (&input.header, &input.credential_env) {
        (Some(_), None) => {
            return Err(RuleError::Invalid(
                "a header needs the name of an environment variable to take its value from".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(RuleError::Invalid(
                "a credential needs a header to travel in, such as authorization".into(),
            ));
        }
        _ => {}
    }

    if let Some(header) = &input.header {
        // Checked against the same list the guest is checked against, so a
        // rule cannot ask for something a request could never carry.
        if header.parse::<reqwest::header::HeaderName>().is_err() {
            return Err(RuleError::Invalid(format!("{header} is not a header name")));
        }
    }

    let row = sqlx::query(
        "insert into egress_rules (id, workspace_id, host, header, credential_env) \
         values ($1, $2, $3, $4, $5) \
         returning id, host, header, credential_env, enabled",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(&host)
    .bind(&input.header)
    .bind(&input.credential_env)
    .fetch_one(pool)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            RuleError::Duplicate(format!("{host} is already allowed"))
        }
        _ => RuleError::Database(e.to_string()),
    })?;

    Ok(read_rule(&row))
}

pub async fn delete(pool: &PgPool, workspace_id: Uuid, id: Uuid) -> Result<bool, RuleError> {
    // Scoped by workspace as well as id, so knowing an id from somewhere else is
    // not the same as being able to use it.
    let result = sqlx::query("delete from egress_rules where id = $1 and workspace_id = $2")
        .bind(id)
        .bind(workspace_id)
        .execute(pool)
        .await
        .map_err(|e| RuleError::Database(e.to_string()))?;
    Ok(result.rows_affected() > 0)
}

fn read_rule(row: &sqlx::postgres::PgRow) -> Rule {
    Rule {
        id: row.get("id"),
        host: row.get("host"),
        header: row.get("header"),
        credential_env: row.get("credential_env"),
        enabled: row.get("enabled"),
    }
}
