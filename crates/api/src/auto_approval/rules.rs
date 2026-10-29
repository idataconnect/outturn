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
use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row as _;
use uuid::Uuid;

use outturn_core::egress::amount::{self, Banding};
use outturn_core::egress::policy::{Op, Risk};

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
