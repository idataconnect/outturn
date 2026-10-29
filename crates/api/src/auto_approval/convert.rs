//! Moving a workspace's auto-approval onto approval rules
//! (`docs/approval-rules.md`, *Migration*): run once at startup, and again
//! harmlessly, since each pass leaves nothing for the next to do.
//!
//! Two things go. A policy over every operation needing one act is replaced by
//! a rule on each operation it reaches today, with the same caps, agent, mode
//! and expiry. And a workspace's own bands, which moved where an amount fell
//! between low, medium and high, are folded into the amount each rule goes
//! through under, after which only the operator's bands are left.
//!
//! Nothing is widened. Each rule is given the amount its policy let through
//! under the bands as they were; where the operator's bands alone would let
//! through more, the rule says the smaller figure. Where a choice is unclear,
//! the narrower reading is the one written. An operation an act policy did not
//! reach before -- because its risk was over the policy's bound, or nothing
//! reaches it -- gets no rule.
//!
//! Here rather than in a migration because what it needs to know is which
//! operations each workspace's agents reach and what each amount works out to,
//! which is skill resolution and the bands arithmetic, not SQL.

use chrono::Utc;
use sqlx::Row as _;
use uuid::Uuid;

use outturn_core::egress::amount::Bands;
use outturn_core::egress::policy::{Op, Test};

use super::{Declared, Policy};
use crate::router::ApiState;

/// Held for the whole pass, so two replicas starting together do not both
/// convert the same workspace. One that does not get it skips the pass: the
/// other is doing it.
const LOCK: i64 = 0x6f75_7474_7275_6c65;

/// A workspace's own band, as the table held it.
struct Band {
    currency: String,
    skill_id: Option<Uuid>,
    requires: Option<String>,
    matches: Option<String>,
    low: Option<i64>,
    medium: Option<i64>,
}

impl Band {
    fn names(&self, d: &Declared) -> bool {
        self.skill_id == Some(d.skill_id)
            && self.requires.as_deref() == Some(d.gate.requires.as_str())
            && self
                .matches
                .as_deref()
                .and_then(super::normalize_matches)
                .is_some_and(|m| m == d.operation())
    }
}

/// The bands an operation ran under with the workspace's rows applied: its
/// own row, else its currency's, else the operator's. A currency's row is in
/// the currency's standard units, an operation's in its own.
fn as_it_was(d: &Declared, rows: &[Band]) -> Declared {
    let Some(banding) = d.banding.as_ref() else {
        return d.clone();
    };
    let in_currency = || rows.iter().filter(|r| r.currency == banding.currency);
    let bands = |r: &Band| Bands {
        low: r.low,
        medium: r.medium,
    };
    let chosen = match in_currency().find(|r| r.names(d)) {
        Some(own) => Some(bands(own)),
        None => in_currency()
            .find(|r| r.skill_id.is_none())
            .map(|all| bands(all).from_standard(&banding.currency, banding.minor_units)),
    };
    let mut d = d.clone();
    if let (Some(chosen), Some(banding)) = (chosen, d.banding.as_mut()) {
        banding.bands = chosen;
    }
    d
}

/// The policy one operation's rule is stored as, made from the policy it
/// replaces or narrows.
fn narrowed(p: &Policy, d: &Declared, under: Option<i64>) -> Policy {
    let mut q = p.clone();
    q.id = Uuid::now_v7();
    q.selector = "operation".into();
    q.skill_id = Some(d.skill_id);
    q.requires = Some(d.gate.requires.clone());
    q.matches = Some(d.operation());
    if let (Some(banding), Some(under)) = (&d.banding, under) {
        // The amount is what limits a money operation now, so its bound on
        // risk goes; on anything else the bound stays, since it is what
        // decided the operation was reached at all.
        q.max_risk = None;
        q.conditions.retain(|t| t.field != banding.field);
        q.conditions.push(Test {
            field: banding.field.clone(),
            op: Op::Lt,
            value: serde_json::json!(under),
        });
    }
    q
}

async fn insert(tx: &mut sqlx::PgConnection, p: &Policy, note: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into approval_policies \
             (workspace_id, id, agent_id, effect, selector, max_risk, requires, skill_id, \
              matches, conditions, per_turn, per_day, sum_field, sum_per_day, mode, note, \
              created_by, updated_by, accepted_by, accepted_at, expires_at, daily_totals) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $17, $17, $18, $19, $20)",
    )
    .bind(p.workspace_id)
    .bind(p.id)
    .bind(p.agent_id)
    .bind(&p.effect)
    .bind(&p.selector)
    .bind(&p.max_risk)
    .bind(&p.requires)
    .bind(p.skill_id)
    .bind(&p.matches)
    .bind(serde_json::to_value(&p.conditions).unwrap_or_default())
    .bind(p.per_turn)
    .bind(p.per_day)
    .bind(&p.sum_field)
    .bind(p.sum_per_day)
    .bind(&p.mode)
    .bind(note)
    // Still on the word of whoever set the policy it came from, so it lapses
    // when they leave as that one would have.
    .bind(p.created_by)
    .bind(p.accepted_at)
    .bind(p.expires_at)
    .bind(serde_json::to_value(&p.daily_totals).unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// One workspace, in one transaction.
async fn workspace(state: &ApiState, tx: &mut sqlx::PgConnection, ws: Uuid) -> anyhow::Result<()> {
    let rows: Vec<Band> = sqlx::query(
        "select currency, skill_id, requires, matches, low, medium \
         from approval_bands where workspace_id = $1",
    )
    .bind(ws)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| Band {
        currency: r.get("currency"),
        skill_id: r.get("skill_id"),
        requires: r.get("requires"),
        matches: r.get("matches"),
        low: r.get("low"),
        medium: r.get("medium"),
    })
    .collect();
    let reached = super::rules::reachable(state, ws).await?;
    let ceiling = super::ceiling(&state.pool, ws).await;
    let now = Utc::now();
    let live: Vec<Policy> = super::list(&state.pool, ws)
        .await?
        .into_iter()
        .filter(|p| p.effect == "include" && p.revoked_at.is_none() && p.expires_at > now)
        .collect();

    let mut written = 0;
    let mut retired: Vec<Uuid> = Vec::new();
    for p in &live {
        for (after, _, agents) in &reached {
            let before = as_it_was(after, &rows);
            if !p.selects(&before) {
                continue;
            }
            let under_before = before
                .banding
                .as_ref()
                .and_then(|b| super::rules::under(p, b, ceiling));
            let under_after = after
                .banding
                .as_ref()
                .and_then(|b| super::rules::under(p, b, ceiling));
            if p.selector == "act" {
                // Only where it was the policy deciding: an agent another rule
                // spoke for keeps that rule, rather than gaining this one.
                let scope: Vec<_> = agents
                    .iter()
                    .filter(|a| p.agent_id.is_none_or(|id| id == a.id))
                    .collect();
                let decided: Vec<Uuid> = scope
                    .iter()
                    .filter(|a| {
                        super::choose(&before, &live, ws, a.id, ceiling, now)
                            .is_some_and(|c| c.policy().id == p.id)
                    })
                    .map(|a| a.id)
                    .collect();
                let note = format!("from the rule for every “{}”", after.gate.requires);
                let mut rule = narrowed(p, after, under_before);
                if !decided.is_empty() && decided.len() == scope.len() {
                    insert(&mut *tx, &rule, &note).await?;
                    written += 1;
                } else {
                    for agent in decided {
                        rule.id = Uuid::now_v7();
                        rule.agent_id = Some(agent);
                        insert(&mut *tx, &rule, &note).await?;
                        written += 1;
                    }
                }
                continue;
            }
            // A risk or operation policy over a money operation whose band was
            // the workspace's: held to what it let through, where the
            // operator's band alone would let through more.
            let wider = match (under_before, under_after) {
                (Some(b), Some(a)) => a > b,
                (Some(_), None) => true,
                _ => false,
            };
            if after.banding.is_some() && wider {
                // An operation's own is replaced; a broad one stays, and the
                // narrower rule beside it speaks for this operation, being the
                // more specific.
                let rule = narrowed(p, after, under_before);
                insert(&mut *tx, &rule, "held to the workspace's band it had").await?;
                if p.selector == "operation" {
                    retired.push(p.id);
                }
                written += 1;
            }
        }
        if p.selector == "act" {
            retired.push(p.id);
        }
    }

    // What the act policies and narrowed operation policies were is kept, as
    // revoked, so their records still say what let a request through.
    sqlx::query(
        "update approval_policies set revoked_at = now() \
         where workspace_id = $1 and id = any($2) and revoked_at is null",
    )
    .bind(ws)
    .bind(&retired)
    .execute(&mut *tx)
    .await?;
    sqlx::query("delete from approval_bands where workspace_id = $1")
        .bind(ws)
        .execute(&mut *tx)
        .await?;
    if written > 0 || !retired.is_empty() || !rows.is_empty() {
        tracing::info!(
            workspace_id = %ws,
            rules_written = written,
            policies_retired = retired.len(),
            bands_removed = rows.len(),
            "moved a workspace's auto-approval onto approval rules"
        );
    }
    Ok(())
}

/// Every workspace with an act policy still standing or a band of its own.
pub async fn run(state: &ApiState) {
    let result: anyhow::Result<()> = async {
        let mut tx = state.pool.begin().await?;
        let locked: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock($1)")
            .bind(LOCK)
            .fetch_one(&mut *tx)
            .await?;
        if !locked {
            return Ok(());
        }
        let workspaces: Vec<Uuid> = sqlx::query_scalar(
            "select workspace_id from approval_policies \
             where selector = 'act' and revoked_at is null \
             union select workspace_id from approval_bands",
        )
        .fetch_all(&mut *tx)
        .await?;
        for ws in workspaces {
            workspace(state, &mut tx, ws).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        // Nothing was written: the pass is one transaction. The act policies
        // stop speaking for anything either way, which asks a person, and the
        // next start tries again.
        tracing::error!(error = %e, "could not move auto-approval onto approval rules");
    }
}
