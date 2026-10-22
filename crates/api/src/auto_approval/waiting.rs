//! Saying in the queue that a broad policy has operations waiting on it.
//!
//! A policy reaching gates by a property -- a risk, an act -- covers a gate
//! published after it was accepted only in trial, so that publishing a skill is
//! never how something comes to be waved through (see "Gates published later"
//! in `docs/auto-approval.md`). Which leaves the policy quietly covering less
//! than its page suggests until somebody presses *Accept operations published
//! since*, and nothing told them to look. This raises one item per policy, for
//! whoever may set policies, naming the operations; it goes when they are
//! accepted, or the policy stops standing.
//!
//! Reconciled rather than raised from the publish: what is waiting depends on
//! every agent's skills, every policy and the deployment's cap together, and
//! each of the writes that changes one of them calls `reconcile` for the
//! workspace, which works the whole answer out again. A workspace has a handful
//! of policies and agents, so that is cheap, and it cannot drift the way a
//! raise here and a settle somewhere else would.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sqlx::Row as _;
use uuid::Uuid;

use outturn_core::auth::Authority;
use outturn_core::egress::policy::Risk;

use super::{Chosen, Declared, Policy, choose};
use crate::actions::{ActionError, NewItem, State, Target};
use crate::router::ApiState;

/// The kind of the item. Not `approval.`-prefixed: those are a request to
/// approve, and the queue offers a yes for them.
pub const KIND: &str = "policy.accept";

/// One agent's gates, as `choose` reads them.
pub struct Agent<'a> {
    pub id: Uuid,
    pub declared: &'a [Declared],
    pub skill_names: &'a BTreeMap<Uuid, String>,
}

/// For each policy that is on and has operations it would cover once accepted,
/// those operations, as a person reads them.
///
/// Only where the policy is the one `choose` picks: an operation an exclusion,
/// a narrower policy or the deployment's cap keeps from it is not waiting on
/// it, and accepting would change nothing.
pub fn waiting(
    agents: &[Agent<'_>],
    policies: &[Policy],
    workspace_id: Uuid,
    ceiling: Option<Risk>,
    now: DateTime<Utc>,
) -> BTreeMap<Uuid, BTreeSet<String>> {
    let mut out: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for agent in agents {
        for d in agent.declared {
            let Some(Chosen::Trial(p)) = choose(d, policies, workspace_id, agent.id, ceiling, now)
            else {
                continue;
            };
            if p.mode != "on" || p.accepted(d) {
                continue;
            }
            let skill = agent
                .skill_names
                .get(&d.skill_id)
                .map(String::as_str)
                .unwrap_or("a skill");
            out.entry(p.id)
                .or_default()
                .insert(format!("{} {} in {skill}", d.gate.method, d.gate.path));
        }
    }
    out
}

/// What the item calls a policy, from what selects for it.
fn describe(p: &Policy) -> String {
    let agent = p
        .agent_name
        .as_deref()
        .map(|a| format!(" for {a}"))
        .unwrap_or_default();
    match p.selector.as_str() {
        "risk" => format!(
            "The policy covering {} risk and below{agent}",
            p.max_risk.as_deref().unwrap_or("some")
        ),
        _ => format!(
            "The policy covering \"{}\"{agent}",
            p.requires.as_deref().unwrap_or("an act")
        ),
    }
}

/// Works out again what is waiting on this workspace's broad policies, and
/// brings the queue into line: an item raised for each policy with operations
/// waiting, replaced when they change, and settled when none are left.
///
/// Best effort. A failure is logged and leaves the queue as it was, which is
/// at worst a stale notice: what a turn may do is decided by the policies
/// themselves, never by this.
pub async fn reconcile(state: &ApiState, workspace_id: Uuid) {
    if let Err(e) = reconcile_inner(state, workspace_id).await {
        tracing::warn!(
            %workspace_id,
            error = %e,
            "could not say which operations wait on a policy"
        );
    }
}

/// After a skill's version is published: its owner's workspace, or, for a
/// platform skill, every workspace with a broad policy on that it might reach.
pub async fn reconcile_after_publish(state: &ApiState, owner: Uuid) {
    if owner != outturn_core::model_settings::PLATFORM_WORKSPACE {
        return reconcile(state, owner).await;
    }
    let workspaces: Vec<Uuid> = match sqlx::query_scalar(
        "select distinct workspace_id from approval_policies \
         where selector in ('risk', 'act') and mode = 'on' \
           and revoked_at is null and expires_at > now()",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "could not find the workspaces a platform skill reaches");
            return;
        }
    };
    for workspace_id in workspaces {
        reconcile(state, workspace_id).await;
    }
}

async fn reconcile_inner(state: &ApiState, workspace_id: Uuid) -> Result<(), String> {
    use crate::skill::SkillStore as _;
    let err = |e: &dyn std::fmt::Display| e.to_string();

    let now = Utc::now();
    let ceiling = super::ceiling(&state.pool, workspace_id).await;
    let policies: Vec<Policy> = super::list(&state.pool, workspace_id)
        .await
        .map_err(|e| err(&e))?
        .into_iter()
        .filter(|p| p.live(now))
        .collect();

    // Nothing is worked out for a workspace with no broad policy on: there is
    // nothing to wait on, and only items left over to settle.
    let broad = policies
        .iter()
        .any(|p| p.mode == "on" && matches!(p.selector.as_str(), "risk" | "act"));
    let mut found = BTreeMap::new();
    if broad && ceiling.is_some() {
        let agents: Vec<Uuid> =
            sqlx::query_scalar("select id from agents where workspace_id = $1 order by id")
                .bind(workspace_id)
                .fetch_all(&state.pool)
                .await
                .map_err(|e| err(&e))?;
        let skills = crate::skill::PostgresSkillStore::new(state.pool.clone());
        let mut read = Vec::new();
        for agent in agents {
            let resolved = skills
                .resolve_for_agent(workspace_id, agent)
                .await
                .map_err(|e| err(&e))?;
            let names: BTreeMap<Uuid, String> = resolved
                .iter()
                .map(|s| (s.skill_id, s.name.clone()))
                .collect();
            let declared = super::declared_for(&state.pool, &resolved)
                .await
                .map_err(|e| err(&e))?;
            read.push((agent, declared, names));
        }
        let agents: Vec<Agent<'_>> = read
            .iter()
            .map(|(id, declared, names)| Agent {
                id: *id,
                declared,
                skill_names: names,
            })
            .collect();
        found = waiting(&agents, &policies, workspace_id, ceiling, now);
    }

    // What is open now. The kind written out, not bound, so the partial index
    // on these items (migration 0059) is the one used.
    let open = sqlx::query(
        "select id, payload from action_items \
         where workspace_id = $1 and kind = 'policy.accept' and state = 'pending'",
    )
    .bind(workspace_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| err(&e))?;

    for row in open {
        let id: Uuid = row.get("id");
        let payload: serde_json::Value = row.get("payload");
        let policy_id = payload["policy_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok());
        let listed: BTreeSet<String> = payload["operations"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        match policy_id.and_then(|p| found.get(&p).map(|ops| (p, ops))) {
            // Still true as it stands: left alone, so its age is how long the
            // operations have waited.
            Some((p, ops)) if *ops == listed => {
                found.remove(&p);
            }
            // Different operations now: this one goes and a new one is raised
            // below, naming what is waiting today.
            Some(_) => settle(state, workspace_id, id, State::Cancelled).await,
            // Nothing waiting: accepted, if the policy still stands; otherwise
            // it is not anybody's to accept any more.
            None => {
                let standing = policy_id
                    .is_some_and(|p| policies.iter().any(|x| x.id == p && x.state(now) == "on"));
                let to = if standing {
                    State::Resolved
                } else {
                    State::Cancelled
                };
                settle(state, workspace_id, id, to).await;
            }
        }
    }

    if found.is_empty() {
        return Ok(());
    }
    // Whoever may set a policy, which is what accepting is. Answering alone is
    // not enough to accept, so it is not enough to be asked.
    let targets: Vec<Target> = state
        .roles
        .roles_with(workspace_id, Authority::ApprovalsDelegate)
        .await
        .map_err(|e| err(&e))?
        .into_iter()
        .map(Target::Role)
        .collect();
    if targets.is_empty() {
        return Ok(());
    }
    for (policy_id, operations) in found {
        let Some(p) = policies.iter().find(|p| p.id == policy_id) else {
            continue;
        };
        let count = operations.len();
        let summary = format!(
            "{} does not cover {} published since it was accepted, until it accepts {}",
            describe(p),
            if count == 1 {
                "1 operation".to_string()
            } else {
                format!("{count} operations")
            },
            if count == 1 { "it" } else { "them" },
        );
        let raised = state
            .actions
            .raise(
                workspace_id,
                NewItem {
                    kind: KIND.into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({
                        "policy_id": policy_id,
                        "summary": summary,
                        "operations": operations,
                    }),
                    targets: targets.clone(),
                    expires_at: None,
                },
            )
            .await;
        // A reconcile racing this one raised it first: the unique index is what
        // keeps it to one, and losing to it is the same outcome.
        if let Err(e) = raised {
            tracing::debug!(%policy_id, error = %e, "a policy's waiting operations were not raised");
        }
    }
    Ok(())
}

async fn settle(state: &ApiState, workspace_id: Uuid, item: Uuid, to: State) {
    match state.actions.settle(workspace_id, item, to, None).await {
        Ok(()) | Err(ActionError::NotFound) | Err(ActionError::NotPending(_)) => {}
        Err(e) => {
            tracing::warn!(%item, error = %e, "could not settle a policy's waiting operations")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{agent, declared, policy, workspace};
    use super::*;

    fn names() -> BTreeMap<Uuid, String> {
        BTreeMap::from([(declared("charge", Risk::Low).skill_id, "Charging".into())])
    }

    #[test]
    fn a_broad_policy_on_lists_what_was_published_after_it_was_accepted() {
        let mut later = declared("charge", Risk::Low);
        later.published_at = Utc::now() + chrono::Duration::seconds(5);
        let earlier = declared("charge", Risk::Low);
        let p = policy("act");
        let names = names();
        let found = waiting(
            &[
                Agent {
                    id: agent(),
                    declared: std::slice::from_ref(&later),
                    skill_names: &names,
                },
                Agent {
                    id: Uuid::now_v7(),
                    declared: std::slice::from_ref(&earlier),
                    skill_names: &names,
                },
            ],
            std::slice::from_ref(&p),
            workspace(),
            Some(Risk::High),
            Utc::now(),
        );
        assert_eq!(
            found
                .get(&p.id)
                .map(|o| o.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["POST /charges in Charging".to_string()])
        );
    }

    #[test]
    fn nothing_waits_on_a_policy_in_trial_one_naming_the_operation_or_one_kept_out() {
        let mut later = declared("charge", Risk::Low);
        later.published_at = Utc::now() + chrono::Duration::seconds(5);
        let names = names();
        let agents = [Agent {
            id: agent(),
            declared: std::slice::from_ref(&later),
            skill_names: &names,
        }];
        let run = |policies: &[Policy], ceiling| {
            waiting(&agents, policies, workspace(), ceiling, Utc::now())
        };

        let mut trial = policy("act");
        trial.mode = "trial".into();
        assert!(run(&[trial], Some(Risk::High)).is_empty());
        assert!(run(&[policy("operation")], Some(Risk::High)).is_empty());
        // Above what the deployment lets any policy cover.
        assert!(run(&[policy("act")], None).is_empty());
        // An exclusion that applies: accepting would change nothing.
        let mut exclude = policy("risk");
        exclude.effect = "exclude".into();
        assert!(run(&[policy("act"), exclude], Some(Risk::High)).is_empty());
    }
}
