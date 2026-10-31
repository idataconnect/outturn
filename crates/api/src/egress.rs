//! Where a workspace's egress rules are kept and read.
//!
//! Read here rather than in the runtime, because the runtime holds no
//! database: it is given a conversation and returns a reply. The rules travel
//! with the turn like the system prompt does, so the tier that owns the
//! transcript stays the only thing talking to Postgres.

use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use outturn_core::egress::rule::{ClientAuth, ClientCredentials, EgressRule};

/// The columns a rule is read from, everywhere one is read. A macro because the
/// queries are static strings, which is what keeps them out of reach of input.
macro_rules! rule_columns {
    () => {
        "host, header, credential_env, credential_id, \
     token_url, scope, client_id_env, client_secret_env, client_auth, owner"
    };
}

/// The hosts this workspace's agents may reach.
///
/// An empty list is the ordinary case for a new workspace and means exactly what
/// it says. Nothing is inherited from a system default: a default that reached
/// somewhere would be a decision made on a workspace's behalf about who their
/// agents may talk to.
pub async fn rules_for(pool: &PgPool, workspace_id: Uuid) -> Result<Vec<EgressRule>, sqlx::Error> {
    rules_for_turn(pool, workspace_id, None).await
}

/// The hosts whose connection is each person's own: what a held request is
/// checked against before it is sent on somebody's turn.
pub async fn asker_hosts(pool: &PgPool, workspace_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "select host from egress_rules where workspace_id = $1 and owner = 'asker' and enabled",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

/// The rules a turn `asker` sent may use, with each asker's rule resolved to
/// the credential that person connected for it. See
/// docs/user-credentials.md, *Whose turn it is*.
///
/// Decided here, once, by the tier that mints the turn: the rule the runtime
/// carries names the credential and the person, the commitment covers both,
/// and the gateway never works out whose key to use from anything the
/// runtime says.
///
/// An asker's rule on a turn of theirs names their live credential, or none
/// when they have connected nothing -- which the gateway refuses with a
/// reason rather than sending bare. On a turn nobody sent it is the
/// workspace's credential the rule names for that case, as an agent's rule;
/// naming none, the rule is left out, and the turn cannot reach the host.
pub async fn rules_for_turn(
    pool: &PgPool,
    workspace_id: Uuid,
    asker: Option<Uuid>,
) -> Result<Vec<EgressRule>, sqlx::Error> {
    let rows = sqlx::query(concat!(
        "select id, ",
        rule_columns!(),
        ", (select c.id from credentials c \
            where c.rule_id = egress_rules.id and c.user_id = $2 \
              and c.revoked_at is null) as own_credential \
         from egress_rules \
         where workspace_id = $1 and enabled \
         order by host"
    ))
    .bind(workspace_id)
    .bind(asker)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .filter_map(|r| {
            let rule = EgressRule {
                host: r.get("host"),
                header: r.get("header"),
                credential_env: r.get("credential_env"),
                client: read_client(r),
                credential: r.get("credential_id"),
                asker: None,
            };
            match (Owner::read(r), asker) {
                (Owner::Agent, _) => Some(rule),
                (Owner::Asker, Some(person)) => Some(EgressRule {
                    credential: r.get("own_credential"),
                    asker: Some(person),
                    ..rule
                }),
                (Owner::Asker, None) => rule.credential.is_some().then_some(rule),
            }
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
    // it, and `normalize_host` permits a wildcard over a domain. Declaring
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
/// Whose credential a rule's requests carry. See docs/user-credentials.md.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    /// The one the rule names, whoever asked: as every rule was before.
    #[default]
    Agent,
    /// The one the person who asked connected for this rule. The credential
    /// a rule names is then used only on a turn nobody sent.
    Asker,
}

impl Owner {
    fn read(row: &sqlx::postgres::PgRow) -> Self {
        match row.get::<&str, _>("owner") {
            "asker" => Owner::Asker,
            _ => Owner::Agent,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Owner::Agent => "agent",
            Owner::Asker => "asker",
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct Rule {
    pub id: Uuid,
    pub host: String,
    pub owner: Owner,
    pub header: Option<String>,
    pub credential_env: Option<String>,
    /// Names and a URL, never a secret or a token, for the same reason.
    pub client: Option<ClientCredentials>,
    /// A sealed credential, by id: safe for the same reason, since what the
    /// row holds is a ciphertext only the gateway can open.
    pub credential: Option<Uuid>,
    pub enabled: bool,
    /// Its credential is not bound to this workspace and host, so the gateway
    /// will refuse every request it matches. Said here because a rule written
    /// before bindings existed looks configured and is not.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unbound: bool,
    /// The header the skill that opened this host says its key travels in,
    /// for a form connecting one to start from. Without it the form offered
    /// `authorization` to an API that takes `api_key`, and a key connected
    /// as offered went in a header the API never reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_header: Option<String>,
}

impl Rule {
    fn as_egress_rule(&self) -> outturn_core::egress::rule::EgressRule {
        outturn_core::egress::rule::EgressRule {
            host: self.host.clone(),
            header: self.header.clone(),
            credential_env: self.credential_env.clone(),
            client: self.client.clone(),
            credential: self.credential,
            asker: None,
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
    /// A sealed credential sent in `header`, instead of `credential_env`.
    #[serde(default)]
    pub credential: Option<Uuid>,
    /// Whose credential the rule's requests carry. For an asker's rule,
    /// `credential` is the workspace's, used only on a turn nobody sent.
    #[serde(default)]
    pub owner: Owner,
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
    bindings: &outturn_core::egress::bindings::Bindings,
    workspace_id: Uuid,
    after: Option<Uuid>,
    limit: i64,
) -> Result<Vec<Rule>, RuleError> {
    let rows = sqlx::query(concat!(
        "select id, ",
        rule_columns!(),
        ", enabled, \
         (select lower(s.auth_header) from skill_sources s \
           where s.skill_id = egress_rules.from_skill_id) as suggested_header \
         from egress_rules \
         where workspace_id = $1 and ($2::uuid is null or id > $2) order by id limit $3"
    ))
    .bind(workspace_id)
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| RuleError::Database(e.to_string()))?;

    let mut rules: Vec<Rule> = rows
        .iter()
        .map(|row| Rule {
            suggested_header: row.get("suggested_header"),
            ..read_rule(row)
        })
        .collect();
    for rule in &mut rules {
        rule.unbound = bindings
            .check_rule(workspace_id, &rule.as_egress_rule())
            .is_err();
    }
    Ok(rules)
}

pub async fn create(
    pool: &PgPool,
    bindings: &outturn_core::egress::bindings::Bindings,
    workspace_id: Uuid,
    input: CreateRule,
) -> Result<Rule, RuleError> {
    let host =
        outturn_core::egress::rule::normalize_host(&input.host).map_err(RuleError::Invalid)?;

    // An asker's rule carries each person's own key, in a header the rule
    // names, to the one host that key is sealed for. Never an environment
    // variable, which is the operator's and nobody's own, and never an
    // exchange, which is the platform authenticating as itself.
    let asker = input.owner == Owner::Asker;
    if asker {
        if input.header.is_none() {
            return Err(RuleError::Invalid(
                "a connection each person makes needs a header for their key to travel in, \
                 such as authorization"
                    .into(),
            ));
        }
        if input.credential_env.is_some() || input.client.is_some() {
            return Err(RuleError::Invalid(
                "a connection each person makes carries their own key, not one from the \
                 gateway's environment or an exchange"
                    .into(),
            ));
        }
        if host.contains('*') {
            return Err(RuleError::Invalid(
                "a connection each person makes names one host, not a wildcard: their key is \
                 bound to a name, not a pattern"
                    .into(),
            ));
        }
    }

    // A header without a variable would attach nothing; a variable without a
    // header has nowhere to go. Either alone is a rule that looks configured
    // and is not, which is worse than one that plainly is not.
    match (&input.header, &input.credential_env, input.credential) {
        (Some(_), None, None) if !asker => {
            return Err(RuleError::Invalid(
                "a header needs a credential, or the name of an environment variable, to take \
                 its value from"
                    .into(),
            ));
        }
        (_, Some(_), Some(_)) => {
            return Err(RuleError::Invalid(
                "a rule takes a sealed credential or an environment variable, not both".into(),
            ));
        }
        (None, _, Some(_)) => {
            return Err(RuleError::Invalid(
                "a credential needs a header to travel in, such as authorization".into(),
            ));
        }
        (None, Some(_), None) => {
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
        if header.parse::<reqwest::header::HeaderName>().is_err() {
            return Err(RuleError::Invalid(format!("{header} is not a header name")));
        }
        outturn_core::egress::rule::check_rule_header(header)
            .map_err(|e| RuleError::Invalid(e.to_string()))?;
    }

    if let Some(variable) = &input.credential_env {
        outturn_core::egress::rule::check_credential_variable(variable)
            .map_err(RuleError::Invalid)?;
    }

    // A sealed credential is checked against what it was sealed to go to: this
    // workspace, this exact host, this header. Said now, while somebody is
    // looking; the gateway asks the same of every request, of the seal itself.
    if let Some(id) = input.credential {
        if host.contains('*') {
            return Err(RuleError::Invalid(
                "a rule sending a sealed credential names one host, not a wildcard: a \
                 credential is bound to names, not patterns"
                    .into(),
            ));
        }
        let binding = super::credentials::binding_of(pool, workspace_id, id)
            .await
            .map_err(|e| RuleError::Database(e.to_string()))?
            .ok_or_else(|| {
                RuleError::Invalid("no such credential in this workspace, or it is revoked".into())
            })?;
        binding
            .attaches(
                workspace_id,
                &host,
                input.header.as_deref(),
                binding.kind.on_a_request(),
                // A rule written here is the agent's; a person's own key
                // is never one.
                None,
            )
            .map_err(RuleError::Invalid)?;
    }

    // Said now, while somebody is looking. Not the enforcement -- the gateway
    // asks again of every request, from bindings this tier cannot write.
    bindings
        .check_rule(
            workspace_id,
            &outturn_core::egress::rule::EgressRule {
                host: host.clone(),
                header: input.header.clone(),
                credential_env: input.credential_env.clone(),
                client: input.client.clone(),
                credential: input.credential,
                asker: None,
            },
        )
        .map_err(RuleError::Invalid)?;

    let client = input.client.as_ref();
    let row = sqlx::query(concat!(
        "insert into egress_rules (id, workspace_id, host, header, credential_env, \
             token_url, scope, client_id_env, client_secret_env, client_auth, credential_id, \
             owner) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
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
    .bind(input.credential)
    .bind(input.owner.as_str())
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

/// Puts a sealed credential on a rule that already exists, or takes it off.
///
/// Approving a skill's hosts writes their rules with no credential, and a
/// workspace connecting its key afterwards should not have to delete the rule
/// and lose the record of which skill opened it. Checked as `create` checks a
/// rule naming a credential: this workspace's, live, and sealed for this exact
/// host and header.
pub async fn set_credential(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
    header: Option<String>,
    credential: Option<Uuid>,
) -> Result<Rule, RuleError> {
    let row = sqlx::query(
        "select host, header, credential_env, token_url, owner from egress_rules \
         where id = $1 and workspace_id = $2",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| RuleError::Database(e.to_string()))?
    .ok_or_else(|| RuleError::Invalid("no such rule".into()))?;
    let host: String = row.get("host");
    if row.get::<Option<String>, _>("credential_env").is_some()
        || row.get::<Option<String>, _>("token_url").is_some()
    {
        return Err(RuleError::Invalid(
            "this rule's credential comes from the gateway's environment; remove it and add \
             the host again to use a sealed credential"
                .into(),
        ));
    }
    // An asker's rule keeps the header each person's key travels in: their
    // keys are sealed to it, and a new one would strand every one of them.
    // What may change is the workspace's credential for a turn nobody sent.
    let asker = Owner::read(&row) == Owner::Asker;
    if asker && header != row.get::<Option<String>, _>("header") {
        return Err(RuleError::Invalid(
            "a connection each person makes keeps the header their keys are sealed to".into(),
        ));
    }
    match (&header, credential) {
        (Some(_), None) if asker => {}
        (Some(header), Some(credential)) => {
            if host.contains('*') {
                return Err(RuleError::Invalid(
                    "a sealed credential is sent to one host, not a wildcard".into(),
                ));
            }
            if header.parse::<reqwest::header::HeaderName>().is_err() {
                return Err(RuleError::Invalid(format!("{header} is not a header name")));
            }
            outturn_core::egress::rule::check_rule_header(header)
                .map_err(|e| RuleError::Invalid(e.to_string()))?;
            let binding = super::credentials::binding_of(pool, workspace_id, credential)
                .await
                .map_err(|e| RuleError::Database(e.to_string()))?
                .ok_or_else(|| {
                    RuleError::Invalid(
                        "no such credential in this workspace, or it is revoked".into(),
                    )
                })?;
            binding
                .attaches(
                    workspace_id,
                    &host,
                    Some(header),
                    binding.kind.on_a_request(),
                    // A rule written here is the agent's; a person's own key
                    // is never one.
                    None,
                )
                .map_err(RuleError::Invalid)?;
        }
        (None, None) => {}
        _ => {
            return Err(RuleError::Invalid(
                "a credential and the header it travels in go together".into(),
            ));
        }
    }
    let row = sqlx::query(concat!(
        "update egress_rules set header = $3, credential_id = $4 \
         where id = $1 and workspace_id = $2 returning id, ",
        rule_columns!(),
        ", enabled"
    ))
    .bind(id)
    .bind(workspace_id)
    .bind(&header)
    .bind(credential)
    .fetch_one(pool)
    .await
    .map_err(|e| RuleError::Database(e.to_string()))?;
    Ok(read_rule(&row))
}

/// Makes a rule's connection each person's own, or one for everyone.
///
/// To the asker: the rule keeps or gains a header for people's keys to travel
/// in, and whatever credential it names becomes the one used on a turn nobody
/// sent. To the agent: the keys people connected for it are revoked, since
/// nothing would use them again, and a rule left naming no credential stops
/// naming a header too, as an agent's rule with no key does.
pub async fn set_owner(
    pool: &PgPool,
    workspace_id: Uuid,
    id: Uuid,
    owner: Owner,
    header: Option<String>,
) -> Result<Rule, RuleError> {
    let row = sqlx::query(
        "select host, header, credential_env, token_url, credential_id from egress_rules \
         where id = $1 and workspace_id = $2",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| RuleError::Database(e.to_string()))?
    .ok_or_else(|| RuleError::Invalid("no such rule".into()))?;
    let host: String = row.get("host");
    let credential: Option<Uuid> = row.get("credential_id");
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| RuleError::Database(e.to_string()))?;
    let updated = match owner {
        Owner::Asker => {
            if row.get::<Option<String>, _>("credential_env").is_some()
                || row.get::<Option<String>, _>("token_url").is_some()
            {
                return Err(RuleError::Invalid(
                    "this rule's key comes from the gateway's environment or an exchange, which \
                     is nobody's own; remove it and add the host again"
                        .into(),
                ));
            }
            if host.contains('*') {
                return Err(RuleError::Invalid(
                    "a connection each person makes names one host, not a wildcard".into(),
                ));
            }
            let header = header.or_else(|| row.get("header")).ok_or_else(|| {
                RuleError::Invalid(
                    "a connection each person makes needs a header for their key to travel \
                         in, such as authorization"
                        .into(),
                )
            })?;
            if header.parse::<reqwest::header::HeaderName>().is_err() {
                return Err(RuleError::Invalid(format!("{header} is not a header name")));
            }
            outturn_core::egress::rule::check_rule_header(&header)
                .map_err(|e| RuleError::Invalid(e.to_string()))?;
            // The workspace's key stays, for turns nobody sent, only if it
            // travels in the header people's keys will.
            if let Some(credential) = credential {
                let binding = super::credentials::binding_of(pool, workspace_id, credential)
                    .await
                    .map_err(|e| RuleError::Database(e.to_string()))?;
                if let Some(binding) = binding {
                    binding
                        .attaches(
                            workspace_id,
                            &host,
                            Some(&header),
                            binding.kind.on_a_request(),
                            None,
                        )
                        .map_err(RuleError::Invalid)?;
                }
            }
            sqlx::query(concat!(
                "update egress_rules set owner = 'asker', header = $3 \
                 where id = $1 and workspace_id = $2 returning id, ",
                rule_columns!(),
                ", enabled"
            ))
            .bind(id)
            .bind(workspace_id)
            .bind(&header)
            .fetch_one(&mut *tx)
            .await
        }
        Owner::Agent => {
            sqlx::query(
                "update credentials set sealed = null, revoked_at = now(), \
                     generation = generation + 1, updated_at = now() \
                 where rule_id = $1 and workspace_id = $2 and revoked_at is null",
            )
            .bind(id)
            .bind(workspace_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| RuleError::Database(e.to_string()))?;
            sqlx::query(concat!(
                "update egress_rules set owner = 'agent', \
                     header = case when credential_id is null then null else header end \
                 where id = $1 and workspace_id = $2 returning id, ",
                rule_columns!(),
                ", enabled"
            ))
            .bind(id)
            .bind(workspace_id)
            .fetch_one(&mut *tx)
            .await
        }
    }
    .map_err(|e| RuleError::Database(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| RuleError::Database(e.to_string()))?;
    Ok(read_rule(&updated))
}

#[derive(Debug, serde::Deserialize)]
pub struct SetOwner {
    pub owner: Owner,
    /// The header people's keys travel in, when the rule names none yet.
    #[serde(default)]
    pub header: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SetCredential {
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub credential: Option<Uuid>,
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
        owner: Owner::read(row),
        header: row.get("header"),
        credential_env: row.get("credential_env"),
        credential: row.get("credential_id"),
        client: read_client(row),
        enabled: row.get("enabled"),
        unbound: false,
        suggested_header: None,
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
    let opened = outturn_core::egress::internal::Internal::shared().allows(host, port);
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
        outturn_core::egress::rule::check_credential_variable(variable)
            .map_err(RuleError::Invalid)?;
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
