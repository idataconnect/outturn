//! Auto-approval policies: which gated requests a workspace has stopped needing
//! to be asked about.
//!
//! See `docs/auto-approval.md`. This half is the API's: the rows, who may write
//! them, and the resolution of which policy applies to each of a turn's gates --
//! intersected with the gate's own ceiling (`auto: never`, its `risk`) and the
//! deployment's (`auto_approve_max_risk`) -- into the `PolicyGrant`s committed
//! into the turn token. The gateway's half, deciding a live request and writing
//! the record, is `outturn_core::egress::policy`.
//!
//! Resolution never widens. A policy reaches a gate only where every one of
//! these says yes: the operator's cap, the gate's declaration, the policy's
//! selector and agent, no exclusion, and the policy's tests fitting what the gate
//! binds. Anything unclear -- two equally specific inclusions, a gate published
//! after a broad policy was accepted -- resolves to a person being asked.

pub mod api;
pub mod bands;
pub mod routes;
pub mod waiting;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row as _;
use uuid::Uuid;

use outturn_core::egress::amount::{Banding, Declaration};
use outturn_core::egress::gate::Gate;
use outturn_core::egress::policy::{self, PolicyGrant, Risk, Test};

/// How long a policy stands when nobody says. Nothing stands for ever.
pub const DEFAULT_LIFETIME_DAYS: i64 = 90;

/// The furthest out a policy may be set to expire. Renewing is an edit, made
/// with the evidence in view.
pub const MAX_LIFETIME_DAYS: i64 = 365;

/// A gate as its declaration describes it to a policy.
#[derive(Debug, Clone)]
pub struct Declared {
    pub gate: Gate,
    pub skill_id: Uuid,
    pub risk: Risk,
    /// `auto: allowed`.
    pub auto: bool,
    pub numbers: Vec<String>,
    pub free: Vec<String>,
    /// When the version declaring it was published, which is what tells a gate
    /// a broad policy was accepted with from one that arrived later.
    pub published_at: DateTime<Utc>,
    /// What the operator declared about money, where the operation moves any.
    pub amount: Option<Declaration>,
    /// The bands it runs under in this workspace: the operator's defaults from
    /// `declared_for`, the workspace's own once `bands::apply` has been.
    pub banding: Option<Banding>,
}

impl Declared {
    /// The lowest risk a request through this gate can work out to, which is
    /// what a selector and the deployment's cap are compared with before any
    /// request arrives. The gateway then holds each request to the policy's
    /// bound by the risk its own amount works out to.
    pub fn reach(&self) -> Risk {
        self.banding.as_ref().map_or(self.risk, Banding::lowest)
    }

    /// `METHOD /path`, as a policy or a band names an operation.
    pub fn operation(&self) -> String {
        format!("{} {}", self.gate.method, self.gate.path)
    }
}

/// One policy row, as the page and the resolution read it.
#[derive(Debug, Clone, Serialize)]
pub struct Policy {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub agent_name: Option<String>,
    pub effect: String,
    pub selector: String,
    pub max_risk: Option<String>,
    pub requires: Option<String>,
    pub skill_id: Option<Uuid>,
    pub skill_name: Option<String>,
    pub matches: Option<String>,
    pub conditions: Vec<Test>,
    pub per_turn: i32,
    pub per_day: i32,
    pub sum_field: Option<String>,
    pub sum_per_day: Option<i64>,
    /// The most of each currency it lets out in a day, in minor units.
    pub daily_totals: BTreeMap<String, i64>,
    pub mode: String,
    pub version: i32,
    pub note: Option<String>,
    pub created_by: Option<Uuid>,
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_by: Option<Uuid>,
    pub updated_by_name: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub accepted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_by: Option<Uuid>,
    pub revoked_by_name: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// Whether whoever set it is still in the workspace. A policy whose setter
    /// left lapses rather than standing on nobody's word.
    pub setter_present: bool,
    /// How many requests it has let out, ever.
    pub auto_approved: i64,
}

impl Policy {
    /// What the page says it is: `on`, `trial`, or why it is neither.
    pub fn state(&self, now: DateTime<Utc>) -> &'static str {
        if self.revoked_at.is_some() {
            "revoked"
        } else if self.expires_at <= now {
            "expired"
        } else if !self.setter_present {
            "lapsed"
        } else if self.mode == "on" {
            "on"
        } else {
            "trial"
        }
    }

    fn live(&self, now: DateTime<Utc>) -> bool {
        matches!(self.state(now), "on" | "trial")
    }

    /// Whether this policy's selector reaches a gate, before anything about its
    /// tests is asked.
    fn selects(&self, declared: &Declared) -> bool {
        // A bound on risk, where one is written, is a bound on the lowest risk
        // a request through the gate can have; the gateway holds each request
        // to it by its own amount.
        let bound = || match self.max_risk.as_deref() {
            None => self.selector != "risk",
            Some(raw) => Risk::parse(raw).is_some_and(|max| declared.reach() <= max),
        };
        if !bound() {
            return false;
        }
        match self.selector.as_str() {
            "risk" => true,
            "act" => self.requires.as_deref() == Some(declared.gate.requires.as_str()),
            "operation" => {
                self.skill_id == Some(declared.skill_id)
                    && self.requires.as_deref() == Some(declared.gate.requires.as_str())
                    && self
                        .matches
                        .as_deref()
                        .and_then(normalize_matches)
                        .is_some_and(|m| m == declared.operation())
            }
            _ => false,
        }
    }

    /// Operation over act over risk, then a policy naming an agent over one that
    /// does not. Only chooses among inclusions; an exclusion that applies wins
    /// whatever this says.
    fn specificity(&self) -> u8 {
        let selector = match self.selector.as_str() {
            "operation" => 3,
            "act" => 2,
            _ => 1,
        };
        selector * 2 + u8::from(self.agent_id.is_some())
    }

    /// Whether this policy applies to gates published after it was accepted
    /// only in trial. Only a selector that reaches gates by a property can reach
    /// one nobody looked at; one naming an operation names it.
    fn accepted(&self, declared: &Declared) -> bool {
        self.selector == "operation" || declared.published_at <= self.accepted_at
    }
}

/// `POST /charges` as a gate stores it: the method upper case, the path as
/// written.
pub fn normalize_matches(raw: &str) -> Option<String> {
    let (method, path) = raw.trim().split_once(' ')?;
    let path = path.trim();
    if method.is_empty() || !path.starts_with('/') {
        return None;
    }
    Some(format!("{} {}", method.trim().to_ascii_uppercase(), path))
}

/// Which policy, if any, speaks for a gate, and on what terms.
#[derive(Debug)]
pub enum Chosen<'a> {
    /// Committed into the token: the gateway may let a request through.
    On(&'a Policy),
    /// Asked about as before, and the question says this policy would have said
    /// yes -- a policy in trial, or an accepted-later gate.
    Trial(&'a Policy),
}

impl<'a> Chosen<'a> {
    pub fn policy(&self) -> &'a Policy {
        match self {
            Chosen::On(p) | Chosen::Trial(p) => p,
        }
    }
}

/// The policy that speaks for one gate, for one agent, under the deployment's
/// cap -- or none, which is a person being asked.
///
/// `ceiling` is `auto_approve_max_risk` as the workspace sees it: `None` for
/// `none`.
pub fn choose<'a>(
    declared: &Declared,
    policies: &'a [Policy],
    workspace_id: Uuid,
    agent_id: Uuid,
    ceiling: Option<Risk>,
    now: DateTime<Utc>,
) -> Option<Chosen<'a>> {
    // The gate's own ceiling and the deployment's, before any policy is read.
    // A host gate binds nothing and is never covered: see "Host gates".
    if !declared.auto || declared.gate.binds.is_empty() {
        return None;
    }
    if ceiling.is_none_or(|max| declared.reach() > max) {
        return None;
    }

    let applicable = policies.iter().filter(|p| {
        p.workspace_id == workspace_id
            && p.live(now)
            && p.agent_id.is_none_or(|a| a == agent_id)
            && p.selects(declared)
    });

    // Deny overrides: any exclusion that applies wins, however broad.
    let mut inclusions = Vec::new();
    for p in applicable {
        if p.effect == "exclude" {
            return None;
        }
        // A policy covers a gate only if the gate binds every field it tests,
        // and a policy with daily totals only a money operation in a currency
        // it totals, or one that moves no money at all: "up to £500 a day"
        // says nothing about dollars.
        let totalled = p.daily_totals.is_empty()
            || declared
                .banding
                .as_ref()
                .is_none_or(|b| p.daily_totals.contains_key(&b.currency));
        if totalled
            && policy::fits(
                &p.conditions,
                p.sum_field.as_deref(),
                &declared.gate.binds,
                &declared.numbers,
            )
        {
            inclusions.push(p);
        }
    }

    let best = inclusions.iter().map(|p| p.specificity()).max()?;
    let mut top = inclusions.into_iter().filter(|p| p.specificity() == best);
    let chosen = top.next()?;
    // Two equally specific policies could say different things about the same
    // request. Neither is chosen over the other by an order nobody set: a person
    // is asked, and the page says the two overlap.
    if top.next().is_some() {
        return None;
    }
    if chosen.mode == "on" && chosen.accepted(declared) {
        Some(Chosen::On(chosen))
    } else {
        Some(Chosen::Trial(chosen))
    }
}

/// What goes into the token for one gate a policy is on for.
///
/// `ceiling` is the deployment's cap, as `choose` was given it. Where the gate
/// declares an amount, the grant carries its bands and the tighter of the cap
/// and the policy's own bound, for the gateway to hold each request to by the
/// risk its amount works out to.
pub fn grant(
    workspace_id: Uuid,
    job_id: Uuid,
    declared: &Declared,
    policy: &Policy,
    ceiling: Option<Risk>,
) -> PolicyGrant {
    let banding = declared.banding.clone();
    let max_risk = banding.as_ref().map(|_| {
        let own = policy.max_risk.as_deref().and_then(Risk::parse);
        match (ceiling, own) {
            (Some(c), Some(o)) => c.min(o),
            (Some(c), None) => c,
            (None, Some(o)) => o,
            // No cap is `none`, which `choose` never covers; low is what is
            // left if it ever did.
            (None, None) => Risk::Low,
        }
    });
    let currency_per_day = banding
        .as_ref()
        .and_then(|b| policy.daily_totals.get(&b.currency).copied());
    let mut fields: Vec<String> = declared
        .gate
        .binds
        .iter()
        .chain(&declared.free)
        .cloned()
        .collect();
    fields.sort();
    fields.dedup();
    PolicyGrant {
        policy_id: policy.id,
        version: policy.version,
        job_id,
        gate: declared.gate.key(workspace_id),
        requires: declared.gate.requires.clone(),
        tests: policy.conditions.clone(),
        fields,
        per_turn: i64::from(policy.per_turn),
        per_day: i64::from(policy.per_day),
        sum_field: policy.sum_field.clone(),
        sum_per_day: policy.sum_per_day,
        banding,
        max_risk,
        currency_per_day,
    }
}

/// The deployment's cap as one workspace sees it. `None` is `none`, and so is
/// anything unreadable: a cap that cannot be read is no permission.
pub async fn ceiling(pool: &sqlx::PgPool, workspace_id: Uuid) -> Option<Risk> {
    use super::settings::SettingsStore as _;
    let view = super::settings::PostgresSettingsStore::new(pool.clone())
        .view(super::settings::Level::Workspace(workspace_id), None)
        .await
        .ok()?;
    view.into_iter()
        .find(|e| e.setting.key == "auto_approve_max_risk")
        .and_then(|e| e.value.as_str().and_then(Risk::parse))
}

/// What a turn's skills declare, read the way `skill::gates_for_turn` reads the
/// gates themselves, with what a policy needs beside each.
///
/// The gate built here is the same value the turn commits to -- the same
/// columns, aggregated the same way -- so its key is the one the gateway looks a
/// policy up by. Were they ever to differ, the policy would find no gate and a
/// person would be asked, which is the direction a mismatch has to fail in.
pub async fn declared_for(
    pool: &sqlx::PgPool,
    skills: &[super::skill::ResolvedSkill],
) -> Result<Vec<Declared>, sqlx::Error> {
    if skills.is_empty() {
        return Ok(Vec::new());
    }
    let versions: Vec<Uuid> = skills.iter().map(|s| s.version_id).collect();
    let rows = sqlx::query(
        "select g.requires, g.host, g.method, g.path_pattern, g.identified_by, \
                g.risk, g.auto, g.numbers, g.free, g.amount, v.skill_id, v.created_at, \
                coalesce( \
                    array_agg(b.field order by b.position) \
                        filter (where b.field is not null), \
                    '{}' \
                ) as binds \
         from skill_version_gates g \
         join skill_versions v on v.id = g.version_id \
         left join skill_version_gate_binds b \
                on b.version_id = g.version_id \
               and b.path = g.path \
               and b.requires = g.requires \
         where g.version_id = any($1) \
         group by g.version_id, g.path, g.requires, g.host, g.method, \
                  g.path_pattern, g.identified_by, g.risk, g.auto, g.numbers, g.free, \
                  g.amount, v.skill_id, v.created_at",
    )
    .bind(&versions)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            let risk = Risk::parse(r.get::<String, _>("risk").as_str()).unwrap_or(Risk::High);
            // A declaration this cannot read is an operation with no amount,
            // whose risk is its base risk: the conservative reading.
            let amount: Option<Declaration> = r
                .get::<Option<serde_json::Value>, _>("amount")
                .and_then(|v| serde_json::from_value(v).ok());
            let banding = amount.as_ref().map(|a| Banding {
                field: a.amount.field.clone(),
                currency: a.amount.currency.clone(),
                base: risk,
                bands: a.bands.clone(),
                floor: a.floor.clone(),
            });
            Declared {
                gate: Gate {
                    requires: r.get("requires"),
                    host: r.get("host"),
                    method: r.get("method"),
                    path: r.get("path_pattern"),
                    identified_by: r.try_get("identified_by").ok().flatten(),
                    binds: r.get("binds"),
                },
                skill_id: r.get("skill_id"),
                // A value the check constraint allows and this cannot read is the
                // most guarded one.
                risk,
                auto: r.get::<String, _>("auto") == "allowed",
                numbers: r.get("numbers"),
                free: r.get("free"),
                published_at: r.get("created_at"),
                amount,
                banding,
            }
        })
        .filter(|d| !d.gate.binds.is_empty())
        .collect())
}

/// What a turn's skills declare, with the bands each money operation runs
/// under in this workspace: `declared_for`, then `bands::apply`.
pub async fn declared_in(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    skills: &[super::skill::ResolvedSkill],
) -> Result<Vec<Declared>, sqlx::Error> {
    let mut declared = declared_for(pool, skills).await?;
    if declared.iter().any(|d| d.amount.is_some()) {
        let rows = bands::of(pool, workspace_id).await?;
        bands::apply(&mut declared, &rows);
    }
    Ok(declared)
}

/// The policy columns the page and the resolution read, as a macro so each
/// query is one literal.
macro_rules! select_policies {
    () => {
        "select p.id, p.workspace_id, p.agent_id, a.name as agent_name, p.effect, \
            p.selector, p.max_risk, p.requires, p.skill_id, s.name as skill_name, p.matches, \
            p.conditions, p.per_turn, p.per_day, p.sum_field, p.sum_per_day, p.daily_totals, \
            p.mode, \
            p.version, p.note, p.created_by, cu.display_name as created_by_name, p.created_at, \
            p.updated_by, uu.display_name as updated_by_name, p.updated_at, p.accepted_at, \
            p.expires_at, p.revoked_by, ru.display_name as revoked_by_name, p.revoked_at, \
            exists (select 1 from user_workspace_roles m \
                    where m.user_id = p.created_by and m.workspace_id = p.workspace_id) \
                as setter_present, \
            (select count(*) from auto_approvals x \
             where x.workspace_id = p.workspace_id and x.policy_id = p.id) as auto_approved \
     from approval_policies p \
     left join agents a on a.id = p.agent_id \
     left join skills s on s.id = p.skill_id \
     left join users cu on cu.id = p.created_by \
     left join users uu on uu.id = p.updated_by \
     left join users ru on ru.id = p.revoked_by "
    };
}

fn read(r: &sqlx::postgres::PgRow) -> Policy {
    Policy {
        id: r.get("id"),
        workspace_id: r.get("workspace_id"),
        agent_id: r.get("agent_id"),
        agent_name: r.get("agent_name"),
        effect: r.get("effect"),
        selector: r.get("selector"),
        max_risk: r.get("max_risk"),
        requires: r.get("requires"),
        skill_id: r.get("skill_id"),
        skill_name: r.get("skill_name"),
        matches: r.get("matches"),
        // A row this cannot read admits nothing: tests that do not parse are a
        // policy whose meaning is unknown, so it is read as one covering nothing.
        conditions: serde_json::from_value(r.get("conditions")).unwrap_or_else(|_| {
            vec![Test {
                field: String::new(),
                op: policy::Op::Eq,
                value: serde_json::Value::Null,
            }]
        }),
        per_turn: r.get("per_turn"),
        per_day: r.get("per_day"),
        sum_field: r.get("sum_field"),
        sum_per_day: r.get("sum_per_day"),
        // Unreadable totals are a policy whose meaning is unknown, read as one
        // with a total of nothing in a currency nobody uses: covering no money
        // operation at all, since none is in that currency.
        daily_totals: serde_json::from_value(r.get("daily_totals"))
            .unwrap_or_else(|_| BTreeMap::from([("???".to_string(), 0)])),
        mode: r.get("mode"),
        version: r.get("version"),
        note: r.get("note"),
        created_by: r.get("created_by"),
        created_by_name: r.get("created_by_name"),
        created_at: r.get("created_at"),
        updated_by: r.get("updated_by"),
        updated_by_name: r.get("updated_by_name"),
        updated_at: r.get("updated_at"),
        accepted_at: r.get("accepted_at"),
        expires_at: r.get("expires_at"),
        revoked_by: r.get("revoked_by"),
        revoked_by_name: r.get("revoked_by_name"),
        revoked_at: r.get("revoked_at"),
        setter_present: r.get("setter_present"),
        auto_approved: r.get("auto_approved"),
    }
}

/// Every policy a workspace has, newest first, revoked ones included.
pub async fn list(pool: &sqlx::PgPool, workspace_id: Uuid) -> Result<Vec<Policy>, sqlx::Error> {
    let rows = sqlx::query(concat!(
        select_policies!(),
        "where p.workspace_id = $1 order by p.id desc"
    ))
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(read).collect())
}

pub async fn get(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    id: Uuid,
) -> Result<Option<Policy>, sqlx::Error> {
    let row = sqlx::query(concat!(
        select_policies!(),
        "where p.workspace_id = $1 and p.id = $2"
    ))
    .bind(workspace_id)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(read))
}

/// The policies that could speak for this agent's turns now.
async fn live_for(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
) -> Result<Vec<Policy>, sqlx::Error> {
    let rows = sqlx::query(concat!(
        select_policies!(),
        "where p.workspace_id = $1 and p.revoked_at is null \
           and p.expires_at > now() and (p.agent_id is null or p.agent_id = $2)"
    ))
    .bind(workspace_id)
    .bind(agent_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(read).collect())
}

/// What a turn's token carries about its policies.
///
/// Best effort in the safe direction: anything that fails here leaves the turn
/// with no policies, so every gated request it makes asks a person, as it would
/// have before policies existed.
pub async fn for_turn(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
    job_id: Uuid,
    skills: &[super::skill::ResolvedSkill],
) -> Vec<PolicyGrant> {
    let Some(ceiling) = ceiling(pool, workspace_id).await else {
        return Vec::new();
    };
    let resolved = async {
        let policies = live_for(pool, workspace_id, agent_id).await?;
        if policies.is_empty() {
            return Ok::<_, sqlx::Error>(Vec::new());
        }
        let declared = declared_in(pool, workspace_id, skills).await?;
        let now = Utc::now();
        Ok(declared
            .iter()
            .filter_map(|d| {
                match choose(d, &policies, workspace_id, agent_id, Some(ceiling), now)? {
                    Chosen::On(p) => Some(grant(workspace_id, job_id, d, p, Some(ceiling))),
                    Chosen::Trial(_) => None,
                }
            })
            .collect())
    }
    .await;
    match resolved {
        Ok(grants) => grants,
        Err(e) => {
            tracing::warn!(
                %workspace_id,
                error = %e,
                "auto-approval policies could not be resolved, so this turn asks a person"
            );
            Vec::new()
        }
    }
}

/// Which of a running turn's committed policies still stand, for its token to
/// be reissued with those alone.
///
/// A grant stands while its policy is on, at the version it was committed at:
/// revoked, expired, lapsed, put back in trial or edited, and it is dropped. An
/// edited policy is not committed again on its new terms here -- that would be
/// a reissue adding what the turn did not start with -- so it covers the gate
/// again from the next turn. Anything that fails drops every grant, which asks
/// a person.
pub async fn still_standing(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
    committed: &[PolicyGrant],
) -> Vec<PolicyGrant> {
    if committed.is_empty() {
        return Vec::new();
    }
    let policies = match live_for(pool, workspace_id, agent_id).await {
        Ok(policies) => policies,
        Err(e) => {
            tracing::warn!(
                %workspace_id,
                error = %e,
                "a turn's policies could not be checked, so its reissued token carries none"
            );
            return Vec::new();
        }
    };
    standing(committed, &policies, Utc::now())
}

fn standing(
    committed: &[PolicyGrant],
    policies: &[Policy],
    now: DateTime<Utc>,
) -> Vec<PolicyGrant> {
    committed
        .iter()
        .filter(|g| {
            policies
                .iter()
                .any(|p| p.id == g.policy_id && p.version == g.version && p.state(now) == "on")
        })
        .cloned()
        .collect()
}

/// The policy that would have said yes to a request a person is about to be
/// asked about, for the question to say so.
///
/// A policy in trial, or one that is on and was not committed for this gate
/// (accepted before the gate was published). Never one that is on and was
/// committed: that request was refused for another reason -- a cap, a field
/// nobody called harmless -- and saying it "would have" been approved would be
/// wrong.
pub async fn would_approve(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    agent_id: Uuid,
    skills: &[super::skill::ResolvedSkill],
    gate: &Gate,
    query: Option<&str>,
    body: Option<&str>,
) -> Option<Uuid> {
    let ceiling = ceiling(pool, workspace_id).await?;
    let policies = live_for(pool, workspace_id, agent_id).await.ok()?;
    if policies.is_empty() {
        return None;
    }
    let declared = declared_in(pool, workspace_id, skills).await.ok()?;
    let d = declared.iter().find(|d| d.gate == *gate)?;
    let Chosen::Trial(p) = choose(
        d,
        &policies,
        workspace_id,
        agent_id,
        Some(ceiling),
        Utc::now(),
    )?
    else {
        return None;
    };
    // The same test the gateway applies, against a grant built the same way, so
    // "would have" means what "does" would.
    grant(workspace_id, Uuid::nil(), d, p, Some(ceiling))
        .admits(workspace_id, gate, query, body)
        .map(|_| p.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outturn_core::egress::policy::Op;
    use serde_json::json;

    pub(super) fn workspace() -> Uuid {
        Uuid::from_bytes([1; 16])
    }

    pub(super) fn agent() -> Uuid {
        Uuid::from_bytes([2; 16])
    }

    fn skill() -> Uuid {
        Uuid::from_bytes([4; 16])
    }

    pub(super) fn declared(requires: &str, risk: Risk) -> Declared {
        Declared {
            gate: Gate {
                requires: requires.into(),
                host: "api.example.com".into(),
                method: "POST".into(),
                path: "/charges".into(),
                identified_by: None,
                binds: vec!["booking_id".into(), "amount_pence".into()],
            },
            skill_id: skill(),
            risk,
            auto: true,
            numbers: vec!["amount_pence".into()],
            free: vec!["description".into()],
            published_at: Utc::now() - chrono::Duration::days(30),
            amount: None,
            banding: None,
        }
    }

    pub(super) fn policy(selector: &str) -> Policy {
        let now = Utc::now();
        Policy {
            id: Uuid::now_v7(),
            workspace_id: workspace(),
            agent_id: None,
            agent_name: None,
            effect: "include".into(),
            selector: selector.into(),
            max_risk: (selector == "risk").then(|| "medium".into()),
            requires: (selector != "risk").then(|| "charge".into()),
            skill_id: (selector == "operation").then(skill),
            skill_name: None,
            matches: (selector == "operation").then(|| "post /charges".into()),
            conditions: vec![Test {
                field: "amount_pence".into(),
                op: Op::Le,
                value: json!(5000),
            }],
            per_turn: 3,
            per_day: 20,
            sum_field: None,
            sum_per_day: None,
            daily_totals: BTreeMap::new(),
            mode: "on".into(),
            version: 1,
            note: None,
            created_by: Some(Uuid::now_v7()),
            created_by_name: None,
            created_at: now,
            updated_by: None,
            updated_by_name: None,
            updated_at: now,
            accepted_at: now,
            expires_at: now + chrono::Duration::days(30),
            revoked_by: None,
            revoked_by_name: None,
            revoked_at: None,
            setter_present: true,
            auto_approved: 0,
        }
    }

    fn pick<'a>(d: &Declared, policies: &'a [Policy], ceiling: Option<Risk>) -> Option<Chosen<'a>> {
        choose(d, policies, workspace(), agent(), ceiling, Utc::now())
    }

    #[test]
    fn nothing_is_covered_until_the_deployment_opts_in() {
        let d = declared("charge", Risk::Low);
        let policies = vec![policy("act")];
        assert!(pick(&d, &policies, None).is_none());
        assert!(matches!(
            pick(&d, &policies, Some(Risk::Low)),
            Some(Chosen::On(_))
        ));
    }

    #[test]
    fn a_policy_cannot_exceed_the_ceiling_its_gate_declares() {
        let policies = vec![policy("act")];
        // Above the deployment's cap.
        assert!(
            pick(
                &declared("charge", Risk::High),
                &policies,
                Some(Risk::Medium)
            )
            .is_none()
        );
        // `auto: never`.
        let mut never = declared("charge", Risk::Low);
        never.auto = false;
        assert!(pick(&never, &policies, Some(Risk::High)).is_none());
    }

    #[test]
    fn a_risk_selector_reaches_only_what_is_at_or_below_it() {
        let policies = vec![policy("risk")];
        assert!(
            pick(
                &declared("charge", Risk::Medium),
                &policies,
                Some(Risk::High)
            )
            .is_some()
        );
        assert!(pick(&declared("charge", Risk::High), &policies, Some(Risk::High)).is_none());
    }

    #[test]
    fn an_exclusion_wins_however_broad() {
        let include = policy("operation");
        let mut exclude = policy("risk");
        exclude.effect = "exclude".into();
        exclude.max_risk = Some("high".into());
        let policies = vec![include, exclude];
        assert!(pick(&declared("charge", Risk::Low), &policies, Some(Risk::High)).is_none());
    }

    #[test]
    fn the_most_specific_inclusion_is_chosen() {
        let broad = policy("risk");
        let narrow = policy("operation");
        let narrow_id = narrow.id;
        let policies = vec![broad, narrow];
        let chosen = pick(&declared("charge", Risk::Low), &policies, Some(Risk::High)).unwrap();
        assert_eq!(chosen.policy().id, narrow_id);

        // And one naming the agent over one that does not.
        let every = policy("act");
        let mut this = policy("act");
        this.agent_id = Some(agent());
        let this_id = this.id;
        let policies = vec![every, this];
        let chosen = pick(&declared("charge", Risk::Low), &policies, Some(Risk::High)).unwrap();
        assert_eq!(chosen.policy().id, this_id);
    }

    #[test]
    fn two_equally_specific_inclusions_ask_a_person() {
        let policies = vec![policy("act"), policy("act")];
        assert!(pick(&declared("charge", Risk::Low), &policies, Some(Risk::High)).is_none());
    }

    #[test]
    fn a_policy_is_scoped_to_its_workspace_and_agent() {
        let mut elsewhere = policy("act");
        elsewhere.workspace_id = Uuid::now_v7();
        assert!(
            pick(
                &declared("charge", Risk::Low),
                &[elsewhere],
                Some(Risk::High)
            )
            .is_none()
        );

        let mut another_agent = policy("act");
        another_agent.agent_id = Some(Uuid::now_v7());
        assert!(
            pick(
                &declared("charge", Risk::Low),
                &[another_agent],
                Some(Risk::High)
            )
            .is_none()
        );
    }

    #[test]
    fn an_expired_revoked_or_lapsed_policy_covers_nothing() {
        let d = declared("charge", Risk::Low);
        let mut expired = policy("act");
        expired.expires_at = Utc::now() - chrono::Duration::seconds(1);
        let mut revoked = policy("act");
        revoked.revoked_at = Some(Utc::now());
        let mut lapsed = policy("act");
        lapsed.setter_present = false;
        for p in [expired, revoked, lapsed] {
            assert!(pick(&d, std::slice::from_ref(&p), Some(Risk::High)).is_none());
        }
    }

    #[test]
    fn a_policy_testing_what_a_gate_does_not_bind_does_not_cover_it() {
        let mut d = declared("charge", Risk::Low);
        d.numbers.clear();
        // The policy compares `amount_pence`, which this gate does not call a
        // number: not covered, rather than covered with the test skipped.
        assert!(pick(&d, &[policy("act")], Some(Risk::High)).is_none());
    }

    #[test]
    fn trial_and_gates_published_after_acceptance_still_ask() {
        let d = declared("charge", Risk::Low);
        let mut trial = policy("act");
        trial.mode = "trial".into();
        assert!(matches!(
            pick(&d, &[trial], Some(Risk::High)),
            Some(Chosen::Trial(_))
        ));

        let mut later = declared("charge", Risk::Low);
        later.published_at = Utc::now() + chrono::Duration::seconds(5);
        assert!(matches!(
            pick(&later, &[policy("act")], Some(Risk::High)),
            Some(Chosen::Trial(_))
        ));
        // One naming the operation named it, so publishing it again changes
        // nothing.
        assert!(matches!(
            pick(&later, &[policy("operation")], Some(Risk::High)),
            Some(Chosen::On(_))
        ));
    }

    #[test]
    fn a_host_gate_is_never_covered() {
        let mut d = declared("reach", Risk::Low);
        d.gate.binds.clear();
        let mut p = policy("act");
        p.requires = Some("reach".into());
        p.conditions.clear();
        assert!(pick(&d, &[p], Some(Risk::High)).is_none());
    }

    #[test]
    fn a_reissued_token_keeps_only_the_policies_still_on_at_their_version() {
        let d = declared("charge", Risk::Low);
        let kept = policy("act");
        let mut revoked = policy("operation");
        revoked.revoked_at = Some(Utc::now());
        let mut edited = policy("risk");
        let grants: Vec<PolicyGrant> = [&kept, &revoked, &edited]
            .iter()
            .map(|p| grant(workspace(), Uuid::nil(), &d, p, None))
            .collect();
        edited.version += 1;
        let mut trial = policy("act");
        trial.mode = "trial".into();
        let mut grants = grants;
        grants.push(grant(workspace(), Uuid::nil(), &d, &trial, None));

        let left = standing(&grants, &[kept.clone(), revoked, edited, trial], Utc::now());
        assert_eq!(
            left.iter().map(|g| g.policy_id).collect::<Vec<_>>(),
            vec![kept.id]
        );
        // A policy gone from the live set altogether -- expired -- goes too.
        assert!(standing(&grants, &[], Utc::now()).is_empty());
    }

    /// A charge whose base risk is high, with bands from the operator: low
    /// below £50, medium below £200.
    pub(super) fn charge_in(currency: &str) -> Declared {
        use outturn_core::egress::amount::{Amount, Bands, Floor};
        let mut d = declared("charge", Risk::High);
        let declaration = Declaration {
            amount: Amount {
                field: "amount_pence".into(),
                currency: currency.into(),
                minor_units: 2,
            },
            bands: Bands {
                low: Some(5000),
                medium: Some(20000),
            },
            floor: Floor::default(),
        };
        d.banding = Some(Banding {
            field: "amount_pence".into(),
            currency: currency.into(),
            base: Risk::High,
            bands: declaration.bands.clone(),
            floor: declaration.floor.clone(),
        });
        d.amount = Some(declaration);
        d
    }

    #[test]
    fn a_risk_selector_reaches_a_money_operation_by_its_lowest_band() {
        let mut low = policy("risk");
        low.max_risk = Some("low".into());
        low.conditions.clear();
        let d = charge_in("GBP");
        // High by declaration, and still reached: some charges are low.
        let chosen = pick(&d, std::slice::from_ref(&low), Some(Risk::Medium)).unwrap();
        let g = grant(
            workspace(),
            Uuid::nil(),
            &d,
            chosen.policy(),
            Some(Risk::Medium),
        );
        assert_eq!(
            g.max_risk,
            Some(Risk::Low),
            "the tighter of the policy and the cap"
        );
        assert_eq!(g.banding, d.banding);

        // A cap of none reaches nothing, bands or not.
        assert!(pick(&d, std::slice::from_ref(&low), None).is_none());
        // And with no band below high, nothing a low policy can reach.
        let mut no_bands = charge_in("GBP");
        no_bands.banding.as_mut().unwrap().bands = Default::default();
        assert!(pick(&no_bands, &[low], Some(Risk::High)).is_none());
    }

    #[test]
    fn an_act_selector_may_bound_risk_too() {
        let mut charges = policy("act");
        charges.max_risk = Some("low".into());
        charges.conditions.clear();
        let d = charge_in("GBP");
        let chosen = pick(&d, std::slice::from_ref(&charges), Some(Risk::High)).unwrap();
        let g = grant(
            workspace(),
            Uuid::nil(),
            &d,
            chosen.policy(),
            Some(Risk::High),
        );
        assert_eq!(g.max_risk, Some(Risk::Low));
        // Without bands the bound is on the base risk, which is high.
        let plain = declared("charge", Risk::High);
        assert!(pick(&plain, &[charges], Some(Risk::High)).is_none());
    }

    #[test]
    fn a_daily_total_covers_only_its_own_currency() {
        let mut p = policy("risk");
        p.max_risk = Some("low".into());
        p.conditions.clear();
        p.daily_totals = BTreeMap::from([("GBP".to_string(), 50000)]);
        let pounds = charge_in("GBP");
        let chosen = pick(&pounds, std::slice::from_ref(&p), Some(Risk::High)).unwrap();
        let g = grant(
            workspace(),
            Uuid::nil(),
            &pounds,
            chosen.policy(),
            Some(Risk::High),
        );
        assert_eq!(g.currency_per_day, Some(50000));
        assert!(
            pick(
                &charge_in("USD"),
                std::slice::from_ref(&p),
                Some(Risk::High)
            )
            .is_none()
        );
        // An operation moving no money is not one a total is about.
        assert!(pick(&declared("charge", Risk::Low), &[p], Some(Risk::High)).is_some());
    }

    #[test]
    fn the_grant_carries_the_gates_fields_and_the_policys_caps() {
        let d = declared("charge", Risk::Low);
        let p = policy("act");
        let g = grant(workspace(), Uuid::nil(), &d, &p, None);
        assert_eq!(g.fields, vec!["amount_pence", "booking_id", "description"]);
        assert_eq!(g.per_turn, 3);
        assert_eq!(g.gate, d.gate.key(workspace()));
    }
}
