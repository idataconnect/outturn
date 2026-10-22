#![cfg(feature = "integration-tests")]
//! Tables that only grew, and the sweeps that keep them to what still
//! matters. Each test ages rows by their own columns rather than waiting, and
//! checks that what is still needed is left. See docs/storage.md.

mod common;

use uuid::Uuid;

async fn workspace(db: &common::TestDb) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(id)
        .bind(format!("T{id}"))
        .bind(format!("t-{}", id.simple()))
        .execute(&db.pool)
        .await
        .expect("workspace");
    id
}

async fn trigger(db: &common::TestDb, ws: Uuid) -> Uuid {
    let agent = Uuid::now_v7();
    sqlx::query("insert into agents (id, workspace_id, name, slug) values ($1, $2, $3, $4)")
        .bind(agent)
        .bind(ws)
        .bind("A")
        .bind(format!("a-{}", agent.simple()))
        .execute(&db.pool)
        .await
        .expect("agent");
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into triggers (id, workspace_id, agent_id, kind, name, prompt) \
         values ($1, $2, $3, 'webhook', 'Hook', 'go')",
    )
    .bind(id)
    .bind(ws)
    .bind(agent)
    .execute(&db.pool)
    .await
    .expect("trigger");
    id
}

/// A firing is remembered thirty days from when it fired, then forgotten; a
/// firing within them still refuses its duplicate.
#[tokio::test]
async fn firings_are_forgotten_after_their_thirty_days_and_not_before() {
    let db = common::TestDb::new().await;
    let ws = workspace(&db).await;
    let trigger = trigger(&db, ws).await;
    sqlx::query(
        "insert into trigger_firings (trigger_id, external_id) values ($1, 'old'), ($1, 'new')",
    )
    .bind(trigger)
    .execute(&db.pool)
    .await
    .expect("firings");

    // The default is thirty days from firing.
    let days: f64 = sqlx::query_scalar(
        "select (extract(epoch from expires_at - created_at) / 86400)::float8 \
         from trigger_firings where trigger_id = $1 and external_id = 'new'",
    )
    .bind(trigger)
    .fetch_one(&db.pool)
    .await
    .expect("expiry");
    assert!(
        (days - 30.0).abs() < 0.01,
        "expires {days} days after firing"
    );

    sqlx::query(
        "update trigger_firings set expires_at = now() - interval '1 minute' \
         where trigger_id = $1 and external_id = 'old'",
    )
    .bind(trigger)
    .execute(&db.pool)
    .await
    .expect("age");

    let gone = outturn::api::trigger::forget_old_firings(&db.pool)
        .await
        .expect("sweep");
    assert_eq!(gone, 1);
    let left: Vec<String> =
        sqlx::query_scalar("select external_id from trigger_firings where trigger_id = $1")
            .bind(trigger)
            .fetch_all(&db.pool)
            .await
            .expect("left");
    assert_eq!(left, ["new"]);
    db.cleanup().await;
}

async fn snapshot(db: &common::TestDb, days_ago: i32) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into catalog_snapshots (id, fetched_at, source, sha256, models) \
         values ($1, now() - make_interval(days => $2), 'test', $3, '{}')",
    )
    .bind(id)
    .bind(days_ago)
    .bind(id.to_string())
    .execute(&db.pool)
    .await
    .expect("snapshot");
    id
}

/// The newest few snapshots stay, and so does an older one a model still
/// names; the rest go.
#[tokio::test]
async fn catalog_snapshots_keep_the_newest_and_the_ones_models_name() {
    let db = common::TestDb::new().await;
    // Oldest first: seven weekly fetches.
    let mut snapshots = Vec::new();
    for week in (0..7).rev() {
        snapshots.push(snapshot(&db, week * 7).await);
    }
    let named = snapshots[1];

    let account = Uuid::now_v7();
    sqlx::query(
        "insert into provider_accounts (id, workspace_id, name, protocol, base_url) \
         values ($1, null, 'Local', 'openai', 'http://llm.example.com/v1')",
    )
    .bind(account)
    .execute(&db.pool)
    .await
    .expect("account");
    sqlx::query(
        "insert into models (id, workspace_id, account_id, name, model, snapshot_id) \
         values ($1, null, $2, 'M', 'm', $3)",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .bind(named)
    .execute(&db.pool)
    .await
    .expect("model");

    let gone = outturn::api::catalog::forget_old_snapshots(&db.pool)
        .await
        .expect("sweep");
    let kept = outturn::api::catalog::SNAPSHOTS_KEPT as usize;
    assert_eq!(gone as usize, snapshots.len() - kept - 1);

    let mut left: Vec<Uuid> = sqlx::query_scalar("select id from catalog_snapshots")
        .fetch_all(&db.pool)
        .await
        .expect("left");
    left.sort();
    let mut expected: Vec<Uuid> = snapshots[snapshots.len() - kept..].to_vec();
    expected.push(named);
    expected.sort();
    assert_eq!(left, expected);

    // Nothing more to forget the second time.
    assert_eq!(
        outturn::api::catalog::forget_old_snapshots(&db.pool)
            .await
            .expect("sweep"),
        0
    );
    db.cleanup().await;
}
