//! Where a workspace's egress rules are kept and read.
//!
//! Read here rather than in the runtime, because the runtime holds no
//! database: it is given a conversation and returns a reply. The rules travel
//! with the turn like the system prompt does, so the tier that owns the
//! transcript stays the only thing talking to Postgres.

use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::runtime::egress::{ClientAuth, ClientCredentials, EgressRule};

/// The columns a rule is read from, everywhere one is read. A macro because the
/// queries are static strings, which is what keeps them out of reach of input.
macro_rules! rule_columns {
    () => {
        "host, header, credential_env, \
     token_url, scope, client_id_env, client_secret_env, client_auth"
    };
}

/// The hosts this workspace's agents may reach.
///
/// An empty list is the ordinary case for a new workspace and means exactly what
/// it says. Nothing is inherited from a system default: a default that reached
/// somewhere would be a decision made on a workspace's behalf about who their
/// agents may talk to.
pub async fn rules_for(pool: &PgPool, workspace_id: Uuid) -> Result<Vec<EgressRule>, sqlx::Error> {
    let rows = sqlx::query(concat!(
        "select ",
        rule_columns!(),
        " from egress_rules \
         where workspace_id = $1 and enabled \
         order by host"
    ))
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| EgressRule {
            host: r.get("host"),
            header: r.get("header"),
            credential_env: r.get("credential_env"),
            client: read_client(r),
        })
        .collect())
}

/// The hosts a skill's own declaration opened, for this workspace.
///
/// What `approve_new_hosts` exempts. A host that arrived because somebody
/// installed a skill declaring it has been consented to once already, in an act
/// that named the skill and the host together -- `POST /v1/skills/{id}/hosts/approve`
/// is that act, and `from_skill_id` is what it records. Asking again, per
/// conversation, would be asking the same question a second time in a worse place.
///
/// A host somebody added by hand through `/v1/egress-rules` carries no skill, and
/// is the case the setting is for: it says this workspace's agents may reach it,
/// not that any particular use of it was reviewed.
pub async fn hosts_from_skills(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let hosts: Vec<String> = sqlx::query_scalar(
        "select host from egress_rules \
         where workspace_id = $1 and enabled and from_skill_id is not null \
         order by host",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;

    // A wildcard is never exempt, however it got here.
    //
    // Approving a skill's hosts takes `settings:update`, which is also what sets
    // `approve_new_hosts` -- so whoever turns the ceiling on can exempt a host from
    // it, and `normalise_host` permits a wildcard over a domain. Declaring
    // `*.example.com` in a skill and approving it would exempt every host under it
    // from every turn: not an escalation across an authority boundary, but a wider
    // door than the setting reads as having. (`*.com` is refused already, since
    // `com` has no domain of its own, so the class is bounded -- not small.)
    //
    // So the exemption is for a host somebody named. A wildcard names a class, and
    // consenting to a class is the permission-dialog hazard `docs/approvals.md`
    // spends its `covers` section on: approving the instance you were shown is not
    // approving the class it belongs to. A workspace wanting the class exempt can
    // say so host by host.
    Ok(hosts
        .into_iter()
        .filter(|host| !host.contains('*'))
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
    /// Names and a URL, never a secret or a token, for the same reason.
    pub client: Option<ClientCredentials>,
    pub enabled: bool,
    /// Its credential is not bound to this workspace and host, so the gateway
    /// will refuse every request it matches. Said here because a rule written
    /// before bindings existed looks configured and is not.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unbound: bool,
}

impl Rule {
    fn as_egress_rule(&self) -> crate::runtime::egress::EgressRule {
        crate::runtime::egress::EgressRule {
            host: self.host.clone(),
            header: self.header.clone(),
            credential_env: self.credential_env.clone(),
            client: self.client.clone(),
        }
    }
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
    /// An OAuth 2 client-credentials exchange, instead of `header` and
    /// `credential_env`.
    #[serde(default)]
    pub client: Option<ClientCredentials>,
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

pub async fn list(
    pool: &PgPool,
    bindings: &crate::egress::bindings::Bindings,
    workspace_id: Uuid,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Rule>, RuleError> {
    let rows = sqlx::query(concat!(
        "select id, ",
        rule_columns!(),
        ", enabled from egress_rules \
         where workspace_id = $1 and ($2::uuid is null or id > $2) order by id limit $3"
    ))
    .bind(workspace_id)
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| RuleError::Database(e.to_string()))?;

    let mut rules: Vec<Rule> = rows.iter().map(read_rule).collect();
    for rule in &mut rules {
        rule.unbound = bindings
            .check_rule(workspace_id, &rule.as_egress_rule())
            .is_err();
    }
    Ok(rules)
}

pub async fn create(
    pool: &PgPool,
    bindings: &crate::egress::bindings::Bindings,
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

    if let Some(client) = &input.client {
        if input.header.is_some() {
            return Err(RuleError::Invalid(
                "a rule takes a header and a variable, or a client-credentials exchange, \
                 not both: the API would read one of them and nobody could say which"
                    .into(),
            ));
        }
        check_client(client)?;
    }

    if let Some(header) = &input.header {
        // Checked against the same list the guest is checked against, so a
        // rule cannot ask for something a request could never carry.
        if header.parse::<reqwest::header::HeaderName>().is_err() {
            return Err(RuleError::Invalid(format!("{header} is not a header name")));
        }
    }

    if let Some(variable) = &input.credential_env {
        crate::runtime::egress::check_credential_variable(variable).map_err(RuleError::Invalid)?;
    }

    // Said now, while somebody is looking. Not the enforcement -- the gateway
    // asks again of every request, from bindings this tier cannot write.
    bindings
        .check_rule(
            workspace_id,
            &crate::runtime::egress::EgressRule {
                host: host.clone(),
                header: input.header.clone(),
                credential_env: input.credential_env.clone(),
                client: input.client.clone(),
            },
        )
        .map_err(RuleError::Invalid)?;

    let client = input.client.as_ref();
    let row = sqlx::query(concat!(
        "insert into egress_rules (id, workspace_id, host, header, credential_env, \
             token_url, scope, client_id_env, client_secret_env, client_auth) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         returning id, ",
        rule_columns!(),
        ", enabled"
    ))
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(&host)
    .bind(&input.header)
    .bind(&input.credential_env)
    .bind(client.map(|c| &c.token_url))
    .bind(client.and_then(|c| c.scope.as_ref()))
    .bind(client.map(|c| &c.client_id_env))
    .bind(client.map(|c| &c.client_secret_env))
    .bind(client.map(|c| c.client_auth.as_str()))
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
        client: read_client(row),
        enabled: row.get("enabled"),
        unbound: false,
    }
}

fn read_client(row: &sqlx::postgres::PgRow) -> Option<ClientCredentials> {
    Some(ClientCredentials {
        token_url: row.get::<Option<String>, _>("token_url")?,
        scope: row.get("scope"),
        client_id_env: row.get("client_id_env"),
        client_secret_env: row.get("client_secret_env"),
        // The column's own check admits only these two.
        client_auth: match row.get::<Option<&str>, _>("client_auth") {
            Some("post") => ClientAuth::Post,
            _ => ClientAuth::Basic,
        },
    })
}

/// Refuses an exchange that could not work, or would send its secret somewhere
/// it should not go.
///
/// The gateway vets the token URL again when it exchanges -- resolving it, and
/// refusing it if the answer is private -- because what a name resolves to
/// today is not what it resolved to when the rule was written. This is the part
/// that can be said now, while somebody is looking.
fn check_client(client: &ClientCredentials) -> Result<(), RuleError> {
    let url = reqwest::Url::parse(&client.token_url)
        .map_err(|_| RuleError::Invalid(format!("{} is not a URL", client.token_url)))?;
    let host = url
        .host_str()
        .ok_or_else(|| RuleError::Invalid("the token URL names no host".into()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(RuleError::Invalid(
            "the token URL carries credentials of its own; name them as variables instead".into(),
        ));
    }
    let port = url.port_or_known_default().unwrap_or(0);
    let opened = crate::gateway::egress::internal::Internal::shared().allows(host, port);
    match url.scheme() {
        "https" => {}
        "http" if opened => {}
        _ => {
            return Err(RuleError::Invalid(
                "the token URL has to be https: the client secret is sent to it".into(),
            ));
        }
    }

    for variable in [&client.client_id_env, &client.client_secret_env] {
        crate::runtime::egress::check_credential_variable(variable).map_err(RuleError::Invalid)?;
    }

    if let Some(scope) = &client.scope {
        // RFC 6749 section 3.3: tokens of printable ASCII, minus `"` and `\`,
        // separated by single spaces.
        let valid = !scope.is_empty()
            && scope.split(' ').all(|token| {
                !token.is_empty()
                    && token
                        .chars()
                        .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
            });
        if !valid {
            return Err(RuleError::Invalid(
                "a scope is one or more tokens separated by single spaces".into(),
            ));
        }
    }

    Ok(())
}
