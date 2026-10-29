//! Approval rules: every gated operation a workspace's agents can reach, with
//! the rule in force for each, as one table reads them. See
//! `docs/approval-rules.md`.
//!
//! A rule is said per operation, whatever stores it. Which policy speaks for
//! an operation is `choose`'s answer for each agent that reaches it, so the
//! table says what a turn would commit rather than a second reading of the
//! rows.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use uuid::Uuid;

use outturn_core::egress::amount::{self, Banding};
use outturn_core::egress::policy::{Op, Risk, Test};

use super::bands::Money;
use super::{Chosen, Declared, Policy};
use crate::router::{ApiError, ApiState, authenticate};

/// How far back the table counts what each operation asked.
const DAYS: i64 = 30;

/// The questions read for those counts, newest first. Past it the counts are
/// of the newest this many, which a workspace asking that often reads as
/// "a lot" either way.
const COUNTED: i64 = 5000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentRef {
    pub id: Uuid,
    pub name: String,
}

/// What a request through the operation carries as money.
#[derive(Debug, Serialize)]
pub struct AmountView {
    pub field: String,
    pub currency: String,
    pub minor_units: u8,
    /// The currency's own digits after the point, which every amount written
    /// to a rule is in, whatever the operation counts in.
    pub standard_minor_units: u8,
}

/// What a workspace cannot change about an operation's rule.
#[derive(Debug, Default, Serialize)]
pub struct Fixed {
    /// The skill says a person is always asked: `auto: never`.
    pub never: bool,
    /// The deployment allows no automatic approval of this operation at all,
    /// at any amount.
    pub beyond_deployment: bool,
    /// The most an amount limit may be: below it a request can be approved
    /// without a person, at or above it never. Absent where nothing bounds it.
    pub most: Option<Money>,
}

/// One rule in force on an operation, and the agents it speaks for.
#[derive(Debug, Serialize)]
pub struct InForce {
    pub policy_id: Uuid,
    /// `operation` where the rule is this operation's own; otherwise the
    /// broader policy it comes from.
    pub selector: String,
    pub requires: Option<String>,
    pub max_risk: Option<String>,
    /// `on`, or `trial`: people are still asked.
    pub state: &'static str,
    pub agents: Vec<AgentRef>,
    /// Below this, where the rule says.
    pub under: Option<Money>,
    pub per_turn: i32,
    pub per_day: i32,
    pub daily_total: Option<Money>,
    pub expires_at: DateTime<Utc>,
    pub set_by: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RuleOperation {
    pub skill_id: Uuid,
    pub skill_name: String,
    pub requires: String,
    /// `POST /charges`.
    pub matches: String,
    /// What a person calls it, from the file declaring it.
    pub name: String,
    pub agents: Vec<AgentRef>,
    pub amount: Option<AmountView>,
    pub fixed: Fixed,
    pub rules: Vec<InForce>,
    pub asked: i64,
    pub approved: i64,
    pub declined: i64,
}

#[derive(Debug, Serialize)]
pub struct RulesPage {
    pub operations: Vec<RuleOperation>,
    pub days: i64,
    pub may_write: bool,
}

/// `charge_payment_account.md` as a person says it: "Charge payment account".
fn name_of(file: &str) -> String {
    let stem = file.rsplit('/').next().unwrap_or(file);
    let stem = stem.strip_suffix(".md").unwrap_or(stem);
    let words = stem.replace(['_', '-'], " ");
    let mut chars = words.trim().chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => file.to_string(),
    }
}

/// The amount at and above which a request through a money operation is
/// never approved without a person: where its risk first passes the
/// deployment's cap, or the operator's high floor, whichever comes first.
/// `None` where neither bounds it.
pub fn most(banding: &Banding, ceiling: Risk) -> Option<i64> {
    let floor_high = banding.floor.high;
    // Risk only rises with the amount, and only changes at a band or a floor,
    // so the first of those where it passes the cap is where it does.
    let mut at: Vec<i64> = [
        Some(0),
        banding.bands.low,
        banding.bands.medium,
        banding.floor.medium,
        banding.floor.high,
    ]
    .into_iter()
    .flatten()
    .collect();
    at.sort_unstable();
    let passes = at
        .into_iter()
        .find(|a| amount::risk(banding.base, &banding.bands, &banding.floor, Some(*a)) > ceiling);
    match (passes, floor_high) {
        (Some(p), Some(f)) => Some(p.min(f)),
        (p, f) => p.or(f),
    }
}

/// The amount a policy lets a money operation through under: the tighter of
/// its test on the amount field -- `≤ 4999` is under 5000 -- and where the
/// risk the amount works out to passes its bound, which is how a policy
/// "up to low risk" limits an amount without naming one.
fn under(policy: &Policy, banding: &Banding, ceiling: Option<Risk>) -> Option<i64> {
    let by_risk = most(banding, super::bound(policy, ceiling));
    let by_test = policy
        .conditions
        .iter()
        .filter(|t| t.field == banding.field)
        .filter_map(|t| {
            let v = t.value.as_i64()?;
            match t.op {
                Op::Lt => Some(v),
                Op::Le => v.checked_add(1),
                _ => None,
            }
        })
        .min();
    match (by_test, by_risk) {
        (Some(t), Some(r)) => Some(t.min(r)),
        (t, r) => t.or(r),
    }
}

/// One operation's row, from what each agent reaching it would be committed.
pub fn row(
    declared: &Declared,
    skill_name: String,
    agents: &[AgentRef],
    policies: &[Policy],
    workspace_id: Uuid,
    ceiling: Option<Risk>,
    now: DateTime<Utc>,
) -> RuleOperation {
    let banding = declared.banding.as_ref();
    let money = |v: i64, b: &Banding| Money::of(v, &b.currency, b.minor_units);
    let fixed = Fixed {
        never: !declared.auto,
        beyond_deployment: ceiling.is_none_or(|c| declared.reach() > c),
        most: match (banding, ceiling) {
            (Some(b), Some(c)) => most(b, c).map(|v| money(v, b)),
            _ => None,
        },
    };

    let mut rules: Vec<InForce> = Vec::new();
    for agent in agents {
        let Some(chosen) = super::choose(declared, policies, workspace_id, agent.id, ceiling, now)
        else {
            continue;
        };
        let p = chosen.policy();
        if let Some(rule) = rules.iter_mut().find(|r| r.policy_id == p.id) {
            rule.agents.push(agent.clone());
            continue;
        }
        let total = banding.and_then(|b| {
            let v = *p.daily_totals.get(&b.currency)?;
            Some(Money::standard(v, &b.currency))
        });
        rules.push(InForce {
            policy_id: p.id,
            selector: p.selector.clone(),
            requires: p.requires.clone(),
            max_risk: p.max_risk.clone(),
            state: match chosen {
                Chosen::On(_) => "on",
                Chosen::Trial(_) => "trial",
            },
            agents: vec![agent.clone()],
            under: banding.and_then(|b| under(p, b, ceiling).map(|v| money(v, b))),
            per_turn: p.per_turn,
            per_day: p.per_day,
            daily_total: total,
            expires_at: p.expires_at,
            set_by: p.created_by_name.clone(),
        });
    }

    RuleOperation {
        skill_id: declared.skill_id,
        skill_name,
        requires: declared.gate.requires.clone(),
        matches: declared.operation(),
        name: name_of(&declared.file),
        agents: agents.to_vec(),
        amount: banding.map(|b| AmountView {
            field: b.field.clone(),
            currency: b.currency.clone(),
            minor_units: b.minor_units,
            standard_minor_units: amount::standard_minor_units(&b.currency),
        }),
        fixed,
        rules,
        asked: 0,
        approved: 0,
        declined: 0,
    }
}

/// One reachable operation: its declaration, its skill's name, and the
/// agents that reach it.
type Reached = (Declared, String, Vec<AgentRef>);

/// Every gated operation any of the workspace's agents can reach, once each,
/// with its skill's name and the agents that reach it.
async fn reachable(state: &ApiState, workspace_id: Uuid) -> Result<Vec<Reached>, sqlx::Error> {
    use crate::skill::SkillStore as _;
    let agents: Vec<(Uuid, String)> =
        sqlx::query_as("select id, name from agents where workspace_id = $1 order by name, id")
            .bind(workspace_id)
            .fetch_all(&state.pool)
            .await?;
    let skills = crate::skill::PostgresSkillStore::new(state.pool.clone());
    let mut seen: BTreeMap<(String, Uuid, String, String), Reached> = BTreeMap::new();
    for (id, name) in agents {
        let Ok(resolved) = skills.resolve_for_agent(workspace_id, id).await else {
            continue;
        };
        let names: BTreeMap<Uuid, String> = resolved
            .iter()
            .map(|s| (s.skill_id, s.name.clone()))
            .collect();
        for d in super::declared_in(&state.pool, workspace_id, &resolved).await? {
            let skill = names.get(&d.skill_id).cloned().unwrap_or_default();
            let key = (
                skill.clone(),
                d.skill_id,
                d.gate.requires.clone(),
                d.operation(),
            );
            let agent = AgentRef {
                id,
                name: name.clone(),
            };
            seen.entry(key)
                .or_insert_with(|| (d, skill, Vec::new()))
                .2
                .push(agent);
        }
    }
    Ok(seen.into_values().collect())
}

/// How often each operation asked a person in the last `DAYS`, and what they
/// said, matched to the operation by the gate's own matcher.
async fn count(
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    rows: &mut [(Declared, RuleOperation)],
) -> Result<(), sqlx::Error> {
    let asked = sqlx::query(
        "select kind, state, payload from action_items \
         where workspace_id = $1 and kind like 'approval.%' \
           and not (payload ? 'followup') \
           and created_at > now() - make_interval(days => $2) \
         order by id desc limit $3",
    )
    .bind(workspace_id)
    .bind(DAYS as i32)
    .bind(COUNTED)
    .fetch_all(pool)
    .await?;
    for r in &asked {
        let kind: String = r.get("kind");
        let state: String = r.get("state");
        let payload: serde_json::Value = r.get("payload");
        let requires = kind.strip_prefix("approval.").unwrap_or_default();
        let host = payload["host"].as_str().unwrap_or_default();
        let method = payload["method"].as_str().unwrap_or_default();
        let path = payload["path"].as_str().unwrap_or_default();
        let Some((_, row)) = rows.iter_mut().find(|(d, _)| {
            d.gate.requires == requires && d.gate.covers_request(host, method, path)
        }) else {
            continue;
        };
        row.asked += 1;
        match state.as_str() {
            "resolved" => row.approved += 1,
            "cancelled" => row.declined += 1,
            _ => {}
        }
    }
    Ok(())
}

/// `GET /v1/approval-rules`.
pub async fn list(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<RulesPage>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_read(&state, &claims).await?;
    let workspace_id = claims.workspace_id;
    let internal = |e: sqlx::Error| -> ApiError {
        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    };

    let reached = reachable(&state, workspace_id).await.map_err(internal)?;
    let policies = super::list(&state.pool, workspace_id)
        .await
        .map_err(internal)?;
    let ceiling = super::ceiling(&state.pool, workspace_id).await;
    let now = Utc::now();
    let mut rows: Vec<(Declared, RuleOperation)> = reached
        .into_iter()
        .map(|(d, skill, agents)| {
            let r = row(&d, skill, &agents, &policies, workspace_id, ceiling, now);
            (d, r)
        })
        .collect();
    count(&state.pool, workspace_id, &mut rows)
        .await
        .map_err(internal)?;

    let may_write = super::api::may_write(&state, &claims).await.is_ok();
    Ok(Json(RulesPage {
        operations: rows.into_iter().map(|(_, r)| r).collect(),
        days: DAYS,
        may_write,
    }))
}

fn invalid(why: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, why.into())
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// An operation as a rule names it.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct OperationRef {
    pub skill_id: Uuid,
    pub requires: String,
    /// `POST /charges`.
    pub matches: String,
}

impl OperationRef {
    fn names(&self, d: &Declared) -> bool {
        self.skill_id == d.skill_id
            && self.requires == d.gate.requires
            && super::normalize_matches(&self.matches).is_some_and(|m| m == d.operation())
    }
}

/// What a person sets on one or more operations. Amounts are in the
/// currency's standard units -- pence, cents, yen -- whatever an operation
/// counts in.
#[derive(Debug, Deserialize)]
pub struct WriteRule {
    pub operations: Vec<OperationRef>,
    /// Absent: every agent that reaches them.
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    /// Whether requests go through without a person, within what follows.
    pub automatic: bool,
    /// Below this. Required for an operation that moves money.
    #[serde(default)]
    pub under: Option<i64>,
    /// The most each operation lets out in a day.
    #[serde(default)]
    pub daily_total: Option<i64>,
    #[serde(default)]
    pub per_turn: Option<i32>,
    #[serde(default)]
    pub per_day: Option<i32>,
    #[serde(default = "trial")]
    pub mode: String,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

fn trial() -> String {
    "trial".into()
}

/// The counts a rule carries when the page sends none. The amount and the
/// day's total are what a person chooses; these only have to be above zero.
pub const DEFAULT_PER_TURN: i32 = 5;
pub const DEFAULT_PER_DAY: i32 = 50;

/// The policy one operation's rule is stored as: its own selector, a test
/// on the amount, and the day's total in its currency.
fn policy_for(
    d: &Declared,
    input: &WriteRule,
    ceiling: Option<Risk>,
    now: DateTime<Utc>,
) -> Result<super::api::Checked, ApiError> {
    let op = d.operation();
    if !d.auto {
        return Err(invalid(format!(
            "{op}: the skill says a person is always asked, and a workspace cannot change that"
        )));
    }
    let mut conditions = Vec::new();
    let mut daily_totals = BTreeMap::new();
    match (&d.banding, input.under) {
        (Some(b), Some(under)) => {
            if under <= 0 {
                return Err(invalid(
                    "the amount requests go through under is above zero",
                ));
            }
            let standard = amount::standard_minor_units(&b.currency);
            let shown = |v: i64| amount::format(v, &b.currency, standard);
            let own = amount::from_standard(under, &b.currency, b.minor_units)
                .ok_or_else(|| invalid(format!("{} cannot be counted exactly", shown(under))))?;
            if let Some(c) = ceiling
                && let Some(most) = most(b, c)
                && own > most
            {
                let most = amount::to_standard(most, &b.currency, b.minor_units).unwrap_or(most);
                return Err(invalid(format!(
                    "{op}: at most {} can go through without a person",
                    shown(most)
                )));
            }
            conditions.push(Test {
                field: b.field.clone(),
                op: Op::Lt,
                value: serde_json::json!(own),
            });
            if let Some(total) = input.daily_total {
                if total < 0 {
                    return Err(invalid("the day's total is an amount at or above zero"));
                }
                daily_totals.insert(b.currency.clone(), total);
            }
        }
        (Some(_), None) => {
            return Err(invalid(format!(
                "{op} moves money, so a rule for it says the amount it goes through under"
            )));
        }
        (None, Some(_)) | (None, None) if input.daily_total.is_some() => {
            return Err(invalid(format!("{op} moves no money to total")));
        }
        (None, Some(_)) => {
            return Err(invalid(format!("{op} moves no money to limit")));
        }
        (None, None) => {}
    }
    super::api::check(
        super::api::WritePolicy {
            agent_id: input.agent_id,
            effect: super::api::include(),
            selector: "operation".into(),
            max_risk: None,
            requires: Some(d.gate.requires.clone()),
            skill_id: Some(d.skill_id),
            matches: Some(op),
            conditions,
            per_turn: Some(input.per_turn.unwrap_or(DEFAULT_PER_TURN)),
            per_day: Some(input.per_day.unwrap_or(DEFAULT_PER_DAY)),
            sum_field: None,
            sum_per_day: None,
            daily_totals,
            mode: input.mode.clone(),
            note: None,
            expires_at: input.expires_at,
        },
        now,
    )
}

/// The exclusion that makes an operation ask whatever broader rule reaches it.
fn exclusion_for(
    d: &Declared,
    agent_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<super::api::Checked, ApiError> {
    super::api::check(
        super::api::WritePolicy {
            agent_id,
            effect: "exclude".into(),
            selector: "operation".into(),
            max_risk: None,
            requires: Some(d.gate.requires.clone()),
            skill_id: Some(d.skill_id),
            matches: Some(d.operation()),
            conditions: Vec::new(),
            per_turn: None,
            per_day: None,
            sum_field: None,
            sum_per_day: None,
            daily_totals: BTreeMap::new(),
            mode: "on".into(),
            note: None,
            // An exclusion stands as long as anything may.
            expires_at: Some(now + chrono::Duration::days(super::MAX_LIFETIME_DAYS)),
        },
        now,
    )
}

/// An operation's own rules that an edit for `agent_id` replaces: a rule for
/// every agent replaces them all, one for an agent replaces its own, and the
/// exclusion making every agent ask, which would otherwise win over it.
fn own<'a>(
    policies: &'a [Policy],
    d: &Declared,
    agent_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Vec<&'a Policy> {
    policies
        .iter()
        .filter(|p| p.revoked_at.is_none() && p.expires_at > now)
        .filter(|p| p.selector == "operation")
        .filter(|p| {
            agent_id.is_none()
                || p.agent_id == agent_id
                || (p.agent_id.is_none() && p.effect == "exclude")
        })
        .filter(|p| {
            p.skill_id == Some(d.skill_id)
                && p.requires.as_deref() == Some(d.gate.requires.as_str())
                && p.matches
                    .as_deref()
                    .and_then(super::normalize_matches)
                    .is_some_and(|m| m == d.operation())
        })
        .collect()
}

/// `PUT /v1/approval-rules`: one rule, on one operation or several, for one
/// agent or all. Everything is checked before anything is written, and the
/// operations' own rules for that scope are replaced in one transaction.
///
/// "Always asks" revokes the operation's own rule, and where a broader rule
/// would still speak for it, adds an exclusion so it asks all the same.
pub async fn put(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Json(input): Json<WriteRule>,
) -> Result<Json<RulesPage>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_write(&state, &claims).await?;
    let workspace_id = claims.workspace_id;
    if input.operations.is_empty() {
        return Err(invalid("a rule names at least one operation"));
    }

    let reached = reachable(&state, workspace_id).await.map_err(internal)?;
    let policies = super::list(&state.pool, workspace_id)
        .await
        .map_err(internal)?;
    let ceiling = super::ceiling(&state.pool, workspace_id).await;
    let now = Utc::now();

    let mut targets = Vec::new();
    for r in &input.operations {
        let (d, _, agents) = reached
            .iter()
            .find(|(d, _, _)| r.names(d))
            .ok_or_else(|| invalid(format!("no agent here reaches {}", r.matches)))?;
        if let Some(a) = input.agent_id
            && !agents.iter().any(|x| x.id == a)
        {
            return Err(invalid(format!(
                "that agent does not reach {}",
                d.operation()
            )));
        }
        if input.automatic && ceiling.is_none_or(|c| d.reach() > c) {
            return Err(invalid(format!(
                "{}: this deployment does not let it go through without a person",
                d.operation()
            )));
        }
        let write = if input.automatic {
            Some(policy_for(d, &input, ceiling, now)?)
        } else {
            None
        };
        targets.push((d, agents, write));
    }
    if let Some(first) = targets.iter().find_map(|(_, _, w)| w.as_ref()) {
        super::api::check_references(&state, workspace_id, first).await?;
    }

    let mut tx = state.pool.begin().await.map_err(internal)?;
    let mut revoked = Vec::new();
    for (d, agents, write) in &targets {
        // Lifting every agent's exclusion for one agent leaves the others
        // asking, as they were.
        let mut kept = Vec::new();
        for p in own(&policies, d, input.agent_id, now) {
            if input.agent_id.is_some() && p.agent_id.is_none() {
                for other in agents.iter().filter(|a| Some(a.id) != input.agent_id) {
                    kept.push(exclusion_for(d, Some(other.id), now)?);
                }
            }
            sqlx::query(
                "update approval_policies set revoked_at = now(), revoked_by = $3 \
                 where workspace_id = $1 and id = $2 and revoked_at is null",
            )
            .bind(workspace_id)
            .bind(p.id)
            .bind(claims.subject)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
            revoked.push(p.id);
        }
        // "Always asks" where something broader would still speak for it, for
        // any agent this rule is about, once the operation's own are gone.
        let exclusion = if write.is_none() {
            let left: Vec<Policy> = policies
                .iter()
                .filter(|p| !revoked.contains(&p.id))
                .cloned()
                .collect();
            let broader = agents
                .iter()
                .filter(|a| input.agent_id.is_none_or(|id| id == a.id))
                .any(|a| super::choose(d, &left, workspace_id, a.id, ceiling, now).is_some());
            if broader {
                Some(exclusion_for(d, input.agent_id, now)?)
            } else {
                None
            }
        } else {
            None
        };
        for c in write.iter().chain(exclusion.as_ref()).chain(&kept) {
            super::api::insert(&mut *tx, workspace_id, Uuid::now_v7(), claims.subject, c).await?;
        }
    }
    tx.commit().await.map_err(internal)?;

    tracing::info!(
        workspace_id = %workspace_id,
        operations = targets.len(),
        automatic = input.automatic,
        set_by = %claims.subject,
        "an approval rule was set"
    );
    super::waiting::reconcile(&state, workspace_id).await;
    list(State(state), headers).await
}

#[derive(Debug, Deserialize)]
pub struct PreviewQuery {
    pub skill_id: Uuid,
    pub requires: String,
    pub matches: String,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    /// In the currency's standard units, as a rule is written.
    #[serde(default)]
    pub under: Option<i64>,
    #[serde(default)]
    pub daily_total: Option<i64>,
}

/// What a rule would have done with the questions its operation raised.
#[derive(Debug, Serialize)]
pub struct Preview {
    pub days: i64,
    pub asked: usize,
    /// How many it would have let through, and what they came to.
    pub would: usize,
    pub would_total: Option<Money>,
    /// Of those, how many a person declined: the number that says whether
    /// it is safe.
    pub would_declined: usize,
    pub history: Vec<super::api::Past>,
}

/// `GET /v1/approval-rules/preview`: a rule not yet set, replayed against the
/// last `DAYS` of its operation's questions as the gateway would decide them.
pub async fn preview(
    State(state): State<Arc<ApiState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PreviewQuery>,
) -> Result<Json<Preview>, ApiError> {
    let claims = authenticate(&state, &headers)?;
    super::api::may_read(&state, &claims).await?;
    let workspace_id = claims.workspace_id;
    let r = OperationRef {
        skill_id: q.skill_id,
        requires: q.requires.clone(),
        matches: q.matches.clone(),
    };
    let reached = reachable(&state, workspace_id).await.map_err(internal)?;
    let (d, _, _) = reached
        .iter()
        .find(|(d, _, _)| r.names(d))
        .ok_or_else(|| invalid(format!("no agent here reaches {}", r.matches)))?;
    let ceiling = super::ceiling(&state.pool, workspace_id).await;
    let now = Utc::now();
    let write = WriteRule {
        operations: vec![r],
        agent_id: q.agent_id,
        automatic: true,
        under: q.under,
        daily_total: q.daily_total,
        per_turn: None,
        per_day: None,
        mode: "on".into(),
        expires_at: None,
    };
    let checked = policy_for(d, &write, ceiling, now)?;
    let draft = super::api::draft(workspace_id, &checked, now);

    let rows = sqlx::query(
        "select id, created_at, payload, state from action_items \
         where workspace_id = $1 and kind = 'approval.' || $2 \
           and not (payload ? 'followup') \
           and created_at > now() - make_interval(days => $3) \
         order by id desc limit $4",
    )
    .bind(workspace_id)
    .bind(&q.requires)
    .bind(DAYS as i32)
    .bind(COUNTED)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    let asked: Vec<super::api::Asked> = rows
        .iter()
        .map(|r| super::api::Asked {
            item_id: r.get("id"),
            at: r.get("created_at"),
            payload: r.get("payload"),
            state: r.get("state"),
        })
        .collect();
    let history = super::api::replay(
        &draft,
        &q.requires,
        &asked,
        std::slice::from_ref(d),
        ceiling,
    );
    let would: Vec<&super::api::Past> = history.iter().filter(|p| p.would).collect();
    let would_total = d.banding.as_ref().map(|b| {
        let sum: i64 = would
            .iter()
            .filter_map(|p| {
                let fields: serde_json::Map<String, serde_json::Value> = p
                    .binds
                    .as_array()?
                    .iter()
                    .filter_map(|x| Some((x["field"].as_str()?.to_string(), x["value"].clone())))
                    .collect();
                b.assess(&fields).1.and_then(|v| b.counted(v))
            })
            .sum();
        Money::standard(sum, &b.currency)
    });
    let would_declined = would.iter().filter(|p| p.answer == "declined").count();
    let would = would.len();
    let mut history = history;
    history.sort_by_key(|p| !(p.would && p.answer == "declined"));
    Ok(Json(Preview {
        days: DAYS,
        asked: history.len(),
        would,
        would_total,
        would_declined,
        history,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_approval::tests::charge_in;

    #[test]
    fn an_operation_is_named_as_a_person_says_it() {
        assert_eq!(
            name_of("charge_payment_account.md"),
            "Charge payment account"
        );
        assert_eq!(name_of("ops/cancel-booking.md"), "Cancel booking");
    }

    #[test]
    fn the_most_a_limit_may_be_is_where_risk_passes_the_cap() {
        // Low below £50, medium below £200, high from there.
        let d = charge_in("GBP");
        let b = d.banding.as_ref().unwrap();
        assert_eq!(most(b, Risk::Low), Some(5000));
        assert_eq!(most(b, Risk::Medium), Some(20000));
    }

    #[test]
    fn a_policy_up_to_low_risk_lets_through_what_is_under_the_low_band() {
        let d = charge_in("GBP");
        let b = d.banding.as_ref().unwrap();
        let mut p = crate::auto_approval::tests::policy("act");
        p.max_risk = Some("low".into());
        assert_eq!(under(&p, b, Some(Risk::High)), Some(5000));
        // A test on the amount tighter than the band is what it says.
        p.conditions = vec![outturn_core::egress::policy::Test {
            field: "amount_pence".into(),
            op: Op::Le,
            value: serde_json::json!(2999),
        }];
        assert_eq!(under(&p, b, Some(Risk::High)), Some(3000));
    }

    #[test]
    fn the_high_floor_bounds_a_limit_even_when_high_is_allowed() {
        let mut d = charge_in("GBP");
        let b = d.banding.as_mut().unwrap();
        b.floor.high = Some(500_000);
        assert_eq!(most(b, Risk::High), Some(500_000));
        b.floor.high = None;
        assert_eq!(most(b, Risk::High), None);
    }
}
