//! The auto-approval record against a real Postgres.
//!
//! The record is the check (docs/auto-approval.md): the gateway's insert
//! succeeds only while the policy is on, unrevoked, unexpired, at the version a
//! turn committed to, set by somebody still in the workspace, and within its
//! caps. All of that is one statement's `where`, so it is tested here rather
//! than in a unit test that could only test a copy of it.
//!
//! Built only under the integration-tests feature; needs TEST_DATABASE_URL.

use outturn::egress::policy::{Admitted, PolicyGrant, Record, record};
use sqlx::postgres::PgPool;
use uuid::Uuid;

mod common;

struct World {
    db: common::TestDb,
    workspace: Uuid,
    setter: Uuid,
}

async fn world() -> World {
    let db = common::TestDb::new().await;
    let workspace = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(workspace)
        .bind(format!("T{workspace}"))
        .bind(format!("t-{}", workspace.simple()))
        .execute(&db.pool)
        .await
        .expect("workspace");
    let setter = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, 'Alice')")
        .bind(setter)
        .execute(&db.pool)
        .await
        .expect("user");
    let role = Uuid::now_v7();
    sqlx::query("insert into roles (workspace_id, id, name) values ($1, $2, 'admin')")
        .bind(workspace)
        .bind(role)
        .execute(&db.pool)
        .await
        .expect("role");
    sqlx::query(
        "insert into user_workspace_roles (user_id, workspace_id, role_id) values ($1, $2, $3)",
    )
    .bind(setter)
    .bind(workspace)
    .bind(role)
    .execute(&db.pool)
    .await
    .expect("member");
    World {
        db,
        workspace,
        setter,
    }
}

async fn policy(pool: &PgPool, w: &World, mode: &str, per_turn: i32, per_day: i32) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into approval_policies \
             (workspace_id, id, selector, requires, per_turn, per_day, sum_field, sum_per_day, \
              mode, created_by, expires_at) \
         values ($1, $2, 'act', 'charge', $3, $4, 'amount_pence', 100, $5, $6, \
                 now() + interval '30 days')",
    )
    .bind(w.workspace)
    .bind(id)
    .bind(per_turn)
    .bind(per_day)
    .bind(mode)
    .bind(w.setter)
    .execute(pool)
    .await
    .expect("policy");
    id
}

fn grant(policy_id: Uuid, job_id: Uuid) -> PolicyGrant {
    PolicyGrant {
        policy_id,
        version: 1,
        job_id,
        gate: "g".into(),
        requires: "charge".into(),
        tests: vec![],
        fields: vec!["amount_pence".into()],
        per_turn: 100,
        per_day: 100,
        sum_field: Some("amount_pence".into()),
        sum_per_day: Some(100),
        banding: None,
        max_risk: None,
        currency_per_day: None,
    }
}

fn admitted(amount: i64) -> Admitted {
    Admitted {
        bound: serde_json::json!({ "amount_pence": amount }),
        amount: Some(amount),
        money: None,
        body: format!(r#"{{"amount_pence":{amount}}}"#),
    }
}

async fn try_record(w: &World, workspace: Uuid, g: &PolicyGrant, amount: i64) -> bool {
    let a = admitted(amount);
    record(
        &w.db.pool,
        &[Record {
            workspace_id: workspace,
            session_id: Uuid::now_v7(),
            grant: g,
            method: "POST",
            host: "api.example.com",
            path: "/charges",
            admitted: &a,
        }],
    )
    .await
    .expect("record")
}

async fn count(w: &World) -> i64 {
    sqlx::query_scalar("select count(*) from auto_approvals where workspace_id = $1")
        .bind(w.workspace)
        .fetch_one(&w.db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_policy_that_is_on_is_recorded_until_a_cap_says_otherwise() {
    let w = world().await;
    let id = policy(&w.db.pool, &w, "on", 2, 10).await;
    let job = Uuid::now_v7();
    let g = grant(id, job);
    assert!(try_record(&w, w.workspace, &g, 10).await);
    assert!(try_record(&w, w.workspace, &g, 10).await);
    // A third in the same turn is over the per-turn cap.
    assert!(!try_record(&w, w.workspace, &g, 10).await);
    // Another turn has room by count, and not by the day's sum.
    let other = grant(id, Uuid::now_v7());
    assert!(!try_record(&w, w.workspace, &other, 90).await);
    assert!(try_record(&w, w.workspace, &other, 80).await);
    assert_eq!(count(&w).await, 3);
    w.db.cleanup().await;
}

#[tokio::test]
async fn trial_revoked_expired_edited_or_lapsed_policies_record_nothing() {
    let w = world().await;
    let job = Uuid::now_v7();

    let trial = policy(&w.db.pool, &w, "trial", 5, 5).await;
    assert!(!try_record(&w, w.workspace, &grant(trial, job), 1).await);

    let revoked = policy(&w.db.pool, &w, "on", 5, 5).await;
    sqlx::query("update approval_policies set revoked_at = now() where id = $1")
        .bind(revoked)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert!(!try_record(&w, w.workspace, &grant(revoked, job), 1).await);

    let expired = policy(&w.db.pool, &w, "on", 5, 5).await;
    sqlx::query("update approval_policies set expires_at = now() where id = $1")
        .bind(expired)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert!(!try_record(&w, w.workspace, &grant(expired, job), 1).await);

    // Edited after the turn committed to version 1.
    let edited = policy(&w.db.pool, &w, "on", 5, 5).await;
    sqlx::query("update approval_policies set version = 2 where id = $1")
        .bind(edited)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert!(!try_record(&w, w.workspace, &grant(edited, job), 1).await);

    // Its setter left the workspace.
    let lapsed = policy(&w.db.pool, &w, "on", 5, 5).await;
    sqlx::query("delete from user_workspace_roles where user_id = $1")
        .bind(w.setter)
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert!(!try_record(&w, w.workspace, &grant(lapsed, job), 1).await);

    assert_eq!(count(&w).await, 0);
    w.db.cleanup().await;
}

#[tokio::test]
async fn a_policy_records_nothing_in_another_workspace() {
    let w = world().await;
    let id = policy(&w.db.pool, &w, "on", 5, 5).await;
    let elsewhere = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(elsewhere)
        .bind(format!("T{elsewhere}"))
        .bind(format!("t-{}", elsewhere.simple()))
        .execute(&w.db.pool)
        .await
        .unwrap();
    assert!(!try_record(&w, elsewhere, &grant(id, Uuid::now_v7()), 1).await);
    w.db.cleanup().await;
}

/// A daily total in one currency counts that currency's amounts across
/// everything the policy covers, and nothing in another: four charges of £49
/// against £150 a day let three through.
#[tokio::test]
async fn a_daily_total_counts_one_currency_across_the_policy() {
    let w = world().await;
    let id = policy(&w.db.pool, &w, "on", 100, 100).await;
    sqlx::query(
        "update approval_policies set sum_field = null, sum_per_day = null, \
                daily_totals = '{\"GBP\": 15000}' where id = $1",
    )
    .bind(id)
    .execute(&w.db.pool)
    .await
    .unwrap();
    let mut g = grant(id, Uuid::now_v7());
    g.sum_field = None;
    g.sum_per_day = None;
    g.currency_per_day = Some(15000);

    let write = |currency: &str, pence: i64| {
        let a = Admitted {
            bound: serde_json::json!({ "amount_pence": pence }),
            amount: None,
            money: Some((currency.to_string(), pence)),
            body: format!(r#"{{"amount_pence":{pence}}}"#),
        };
        let g = g.clone();
        let pool = w.db.pool.clone();
        let workspace = w.workspace;
        async move {
            record(
                &pool,
                &[Record {
                    workspace_id: workspace,
                    session_id: Uuid::now_v7(),
                    grant: &g,
                    method: "POST",
                    host: "api.example.com",
                    path: "/charges",
                    admitted: &a,
                }],
            )
            .await
            .expect("record")
        }
    };
    for _ in 0..3 {
        assert!(write("GBP", 4900).await);
    }
    assert!(!write("GBP", 4900).await, "over £150 in a day");
    // Dollars are not pounds, and this policy totals none of them.
    assert!(write("USD", 4900).await);
    let recorded: (i64, i64) = sqlx::query_as(
        "select count(*), sum(currency_amount)::bigint from auto_approvals \
         where workspace_id = $1 and currency = 'GBP'",
    )
    .bind(w.workspace)
    .fetch_one(&w.db.pool)
    .await
    .unwrap();
    assert_eq!(recorded, (3, 14700));
    w.db.cleanup().await;
}

/// The row's own total holds where the committed one is looser -- the tighter
/// of the two, as with every other cap.
#[tokio::test]
async fn the_rows_daily_total_holds_over_a_looser_committed_one() {
    let w = world().await;
    let id = policy(&w.db.pool, &w, "on", 100, 100).await;
    sqlx::query(
        "update approval_policies set sum_field = null, sum_per_day = null, \
                daily_totals = '{\"GBP\": 100}' where id = $1",
    )
    .bind(id)
    .execute(&w.db.pool)
    .await
    .unwrap();
    let mut g = grant(id, Uuid::now_v7());
    g.sum_field = None;
    g.sum_per_day = None;
    g.currency_per_day = Some(1_000_000);
    let a = Admitted {
        bound: serde_json::json!({ "amount_pence": 500 }),
        amount: None,
        money: Some(("GBP".to_string(), 500)),
        body: r#"{"amount_pence":500}"#.into(),
    };
    let written = record(
        &w.db.pool,
        &[Record {
            workspace_id: w.workspace,
            session_id: Uuid::now_v7(),
            grant: &g,
            method: "POST",
            host: "api.example.com",
            path: "/charges",
            admitted: &a,
        }],
    )
    .await
    .expect("record");
    assert!(!written);
    w.db.cleanup().await;
}
