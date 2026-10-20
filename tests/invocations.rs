#![cfg(feature = "integration-tests")]
//! The record of declared writes: a write sent twice is one write.
//! See docs/idempotency.md.

mod common;

use outturn::api::invocations::{Claimed, claim_in, settle_in};
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

#[tokio::test]
async fn an_answered_write_is_answered_again_without_going_out() {
    let db = common::TestDb::new().await;
    let ws = workspace(&db).await;

    assert_eq!(claim_in(&db.pool, ws, "k1").await.unwrap(), Claimed::New);
    let answer = serde_json::json!({"status": 201, "body": "{\"id\":\"bk_1\"}"});
    settle_in(&db.pool, ws, "k1", Some(answer.clone()))
        .await
        .unwrap();

    match claim_in(&db.pool, ws, "k1").await.unwrap() {
        Claimed::Completed { response, .. } => assert_eq!(response, answer),
        other => panic!("expected the first answer, got {other:?}"),
    }
}

#[tokio::test]
async fn a_write_whose_answer_was_never_seen_is_not_sent_again() {
    let db = common::TestDb::new().await;
    let ws = workspace(&db).await;
    assert_eq!(claim_in(&db.pool, ws, "k2").await.unwrap(), Claimed::New);
    assert!(matches!(
        claim_in(&db.pool, ws, "k2").await.unwrap(),
        Claimed::Attempted { .. }
    ));
}

#[tokio::test]
async fn a_refused_write_may_be_tried_again() {
    // A refusal changed nothing, so its claim is released: the retry a 400
    // invites is not mistaken for a duplicate.
    let db = common::TestDb::new().await;
    let ws = workspace(&db).await;
    assert_eq!(claim_in(&db.pool, ws, "k3").await.unwrap(), Claimed::New);
    settle_in(&db.pool, ws, "k3", None).await.unwrap();
    assert_eq!(claim_in(&db.pool, ws, "k3").await.unwrap(), Claimed::New);
}

#[tokio::test]
async fn one_workspaces_write_is_not_anothers() {
    let db = common::TestDb::new().await;
    let (a, b) = (workspace(&db).await, workspace(&db).await);
    assert_eq!(claim_in(&db.pool, a, "same").await.unwrap(), Claimed::New);
    assert_eq!(claim_in(&db.pool, b, "same").await.unwrap(), Claimed::New);
}

#[tokio::test]
async fn an_expired_record_is_claimed_over() {
    let db = common::TestDb::new().await;
    let ws = workspace(&db).await;
    assert_eq!(claim_in(&db.pool, ws, "k4").await.unwrap(), Claimed::New);
    sqlx::query("update tool_invocations set expires_at = now() - interval '1 second'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(claim_in(&db.pool, ws, "k4").await.unwrap(), Claimed::New);
}
