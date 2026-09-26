//! The action queue against a real Postgres.
//!
//! Every behaviour here is in SQL -- partial unique indexes, a composite
//! foreign key, `count(distinct)`, and a conditional update that decides who
//! wins a race. None of it is reachable from a unit test, and an in-memory
//! implementation of the trait would only test the in-memory implementation.
//!
//! Built only under the integration-tests feature; needs TEST_DATABASE_URL
//! naming a test database. See tests/api.rs.
//!
//! The delivery tests hold a `PgListener` of their own, and every one of them
//! drops it before `cleanup`. A listener checks a connection out of the test's
//! pool and never returns it, and `cleanup` begins by closing that pool --
//! which waits for every connection to come back. Left to the end of scope the
//! listener is still holding one, and the test hangs in `cleanup` rather than
//! failing. The suites that go through `EventBus` do not hit this because the
//! bus owns its listener on a task of its own.

use outturn::api::actions::{
    ActionError, ActionStore, Delivery, NewItem, PostgresActionStore, State, Target,
};
use sqlx::postgres::PgPool;
use uuid::Uuid;

mod common;

/// A workspace, and the store under test.
async fn setup() -> (common::TestDb, Uuid, PostgresActionStore) {
    let db = common::TestDb::new().await;
    let workspace_id = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(workspace_id)
        .bind(format!("T{workspace_id}"))
        .bind(format!("t-{}", workspace_id.simple()))
        .execute(&db.pool)
        .await
        .expect("workspace");

    let store = PostgresActionStore::new(db.pool.clone());
    (db, workspace_id, store)
}

async fn make_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    // Just the account: an email address lives on `user_identities`, and
    // nothing here signs in.
    sqlx::query("insert into users (id, display_name) values ($1, $2)")
        .bind(id)
        .bind("Test User")
        .execute(pool)
        .await
        .expect("user");
    id
}

async fn make_role(pool: &PgPool, workspace_id: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into roles (workspace_id, id, name) values ($1, $2, $3)")
        .bind(workspace_id)
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .expect("role");
    id
}

async fn grant(pool: &PgPool, user_id: Uuid, workspace_id: Uuid, role_id: Uuid) {
    sqlx::query(
        "insert into user_workspace_roles (user_id, workspace_id, role_id) values ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(workspace_id)
    .bind(role_id)
    .execute(pool)
    .await
    .expect("grant");
}

async fn revoke(pool: &PgPool, user_id: Uuid, workspace_id: Uuid, role_id: Uuid) {
    sqlx::query(
        "delete from user_workspace_roles \
         where user_id = $1 and workspace_id = $2 and role_id = $3",
    )
    .bind(user_id)
    .bind(workspace_id)
    .bind(role_id)
    .execute(pool)
    .await
    .expect("revoke");
}

fn item(kind: &str, targets: Vec<Target>) -> NewItem {
    NewItem {
        kind: kind.into(),
        event_id: None,
        payload: serde_json::json!({"question": "approve?"}),
        targets,
        expires_at: None,
    }
}

// --- raising --------------------------------------------------------------

#[tokio::test]
async fn a_raised_item_reaches_the_targeted_user() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");

    let queue = store.queue_for_user(ws, user).await.expect("queue");
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].id, id);
    assert_eq!(queue[0].kind, "hitl.approval");
    assert_eq!(queue[0].state, State::Pending);

    db.cleanup().await;
}

#[tokio::test]
async fn a_raised_item_reaches_everyone_holding_the_targeted_role() {
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let alice = make_user(&db.pool).await;
    let bob = make_user(&db.pool).await;
    grant(&db.pool, alice, ws, role).await;
    grant(&db.pool, bob, ws, role).await;

    store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");

    assert_eq!(store.count_for_user(ws, alice).await.unwrap(), 1);
    assert_eq!(store.count_for_user(ws, bob).await.unwrap(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn an_item_does_not_reach_someone_outside_the_role() {
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let outsider = make_user(&db.pool).await;

    store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");

    assert_eq!(store.count_for_user(ws, outsider).await.unwrap(), 0);
    assert!(store.queue_for_user(ws, outsider).await.unwrap().is_empty());

    db.cleanup().await;
}

#[tokio::test]
async fn an_item_with_no_targets_is_refused_before_it_is_written() {
    let (db, ws, store) = setup().await;

    let err = store
        .raise(ws, item("hitl.approval", vec![]))
        .await
        .unwrap_err();
    assert!(matches!(err, ActionError::Invalid(_)));

    // The transaction must not have left the item behind.
    let count: i64 =
        sqlx::query_scalar("select count(*) from action_items where workspace_id = $1")
            .bind(ws)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);

    db.cleanup().await;
}

// --- the premise: role membership is resolved at read time ----------------

#[tokio::test]
async fn joining_a_role_shows_items_raised_before_you_joined() {
    // The whole reason targets are stored as roles rather than expanded to
    // members when the item is raised.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let latecomer = make_user(&db.pool).await;

    store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");
    assert_eq!(store.count_for_user(ws, latecomer).await.unwrap(), 0);

    grant(&db.pool, latecomer, ws, role).await;

    // No queue row was written by the grant.
    assert_eq!(store.count_for_user(ws, latecomer).await.unwrap(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn leaving_a_role_takes_its_items_out_of_your_queue() {
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let leaver = make_user(&db.pool).await;
    grant(&db.pool, leaver, ws, role).await;

    store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");
    assert_eq!(store.count_for_user(ws, leaver).await.unwrap(), 1);

    revoke(&db.pool, leaver, ws, role).await;
    assert_eq!(store.count_for_user(ws, leaver).await.unwrap(), 0);

    db.cleanup().await;
}

#[tokio::test]
async fn an_item_raised_into_an_empty_role_is_not_lost() {
    // Write-time expansion would resolve this to nobody and drop it.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");

    let queue = store.queue_for_role(ws, role).await.expect("role queue");
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].id, id);

    // And it arrives when somebody finally joins.
    let joiner = make_user(&db.pool).await;
    grant(&db.pool, joiner, ws, role).await;
    assert_eq!(store.count_for_user(ws, joiner).await.unwrap(), 1);

    db.cleanup().await;
}

// --- counting -------------------------------------------------------------

#[tokio::test]
async fn an_item_targeted_at_you_and_at_your_role_counts_once() {
    // The `count(distinct i.id)`: the join matches twice.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let user = make_user(&db.pool).await;
    grant(&db.pool, user, ws, role).await;

    store
        .raise(
            ws,
            item(
                "hitl.approval",
                vec![Target::Role(role), Target::User(user)],
            ),
        )
        .await
        .expect("raise");

    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 1);
    assert_eq!(store.queue_for_user(ws, user).await.unwrap().len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn settled_items_leave_the_count() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 1);

    store
        .settle(ws, id, State::Resolved, Some(user))
        .await
        .expect("settle");
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 0);

    db.cleanup().await;
}

#[tokio::test]
async fn a_queue_counts_only_its_own_workspace() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let other = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(other)
        .bind(format!("T{other}"))
        .bind(format!("t-{}", other.simple()))
        .execute(&db.pool)
        .await
        .expect("other workspace");

    store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");
    store
        .raise(other, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise elsewhere");

    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 1);
    assert_eq!(store.count_for_user(other, user).await.unwrap(), 1);

    db.cleanup().await;
}

// --- settling -------------------------------------------------------------

#[tokio::test]
async fn one_answer_wins_and_the_other_is_told_it_lost() {
    // Two people answering the same request. The conditional update is what
    // makes this one winner rather than a silently overwritten decision.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let alice = make_user(&db.pool).await;
    let bob = make_user(&db.pool).await;
    grant(&db.pool, alice, ws, role).await;
    grant(&db.pool, bob, ws, role).await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");

    store
        .settle(ws, id, State::Resolved, Some(alice))
        .await
        .expect("alice settles");

    let err = store
        .settle(ws, id, State::Resolved, Some(bob))
        .await
        .unwrap_err();
    assert!(matches!(err, ActionError::NotPending("resolved")));

    // Alice's answer stands.
    let who: Option<Uuid> = sqlx::query_scalar(
        "select resolved_by from action_items where workspace_id = $1 and id = $2",
    )
    .bind(ws)
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(who, Some(alice));

    db.cleanup().await;
}

#[tokio::test]
async fn settling_something_that_is_not_there_says_so() {
    let (db, ws, store) = setup().await;
    let err = store
        .settle(ws, Uuid::now_v7(), State::Resolved, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ActionError::NotFound));
    db.cleanup().await;
}

#[tokio::test]
async fn an_item_cannot_be_settled_as_pending() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;
    let id = store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");

    let err = store
        .settle(ws, id, State::Pending, None)
        .await
        .unwrap_err();
    assert!(matches!(err, ActionError::Invalid(_)));

    db.cleanup().await;
}

#[tokio::test]
async fn settling_leaves_an_item_readable_with_its_outcome() {
    // Resolution is recorded rather than deleted, so a queue that emptied can
    // still explain why.
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;
    let id = store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");

    store
        .settle(ws, id, State::Cancelled, Some(user))
        .await
        .expect("settle");

    let state: String =
        sqlx::query_scalar("select state from action_items where workspace_id = $1 and id = $2")
            .bind(ws)
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(state, "cancelled");

    db.cleanup().await;
}

// --- targeting ------------------------------------------------------------

#[tokio::test]
async fn adding_a_target_escalates_an_open_item() {
    let (db, ws, store) = setup().await;
    let first = make_role(&db.pool, ws, "approvers").await;
    let second = make_role(&db.pool, ws, "managers").await;
    let manager = make_user(&db.pool).await;
    grant(&db.pool, manager, ws, second).await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::Role(first)]))
        .await
        .expect("raise");
    assert_eq!(store.count_for_user(ws, manager).await.unwrap(), 0);

    store
        .add_targets(ws, id, &[Target::Role(second)])
        .await
        .expect("escalate");
    assert_eq!(store.count_for_user(ws, manager).await.unwrap(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn adding_a_target_twice_is_not_an_error() {
    // `on conflict do nothing` against the partial unique indexes: an
    // escalation re-adding a role should not have to know whether it did.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let user = make_user(&db.pool).await;
    grant(&db.pool, user, ws, role).await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");

    store
        .add_targets(ws, id, &[Target::Role(role)])
        .await
        .expect("idempotent add");

    let targets: i64 = sqlx::query_scalar(
        "select count(*) from action_targets where workspace_id = $1 and item_id = $2",
    )
    .bind(ws)
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(targets, 1);
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn adding_a_target_to_a_missing_item_says_not_found() {
    // Rather than surfacing a foreign key violation.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let err = store
        .add_targets(ws, Uuid::now_v7(), &[Target::Role(role)])
        .await
        .unwrap_err();
    assert!(matches!(err, ActionError::NotFound));
    db.cleanup().await;
}

#[tokio::test]
async fn removing_a_target_leaves_the_item_for_everyone_else() {
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let holder = make_user(&db.pool).await;
    let named = make_user(&db.pool).await;
    grant(&db.pool, holder, ws, role).await;

    let id = store
        .raise(
            ws,
            item(
                "hitl.approval",
                vec![Target::Role(role), Target::User(named)],
            ),
        )
        .await
        .expect("raise");

    store
        .remove_targets(ws, id, &[Target::User(named)])
        .await
        .expect("remove");

    assert_eq!(store.count_for_user(ws, named).await.unwrap(), 0);
    assert_eq!(store.count_for_user(ws, holder).await.unwrap(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn a_role_target_and_a_user_target_with_the_same_id_are_distinct() {
    // Nothing stops a role id and a user id colliding; the two partial unique
    // indexes are what keep them separate rows.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;
    let user = make_user(&db.pool).await;

    let id = store
        .raise(
            ws,
            item(
                "hitl.approval",
                vec![Target::Role(role), Target::User(user)],
            ),
        )
        .await
        .expect("raise");

    // Removing the user target must not take the role target with it.
    store
        .remove_targets(ws, id, &[Target::User(user)])
        .await
        .expect("remove");

    let remaining: i64 = sqlx::query_scalar(
        "select count(*) from action_targets \
         where workspace_id = $1 and item_id = $2 and role_id is not null",
    )
    .bind(ws)
    .bind(id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(remaining, 1);

    db.cleanup().await;
}

// --- orphaning ------------------------------------------------------------

#[tokio::test]
async fn removing_the_last_target_orphans_the_item_rather_than_hiding_it() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");
    store
        .remove_targets(ws, id, &[Target::User(user)])
        .await
        .expect("remove");

    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 0);

    let orphans = store.orphaned(ws, 10).await.expect("orphaned");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, id);

    db.cleanup().await;
}

#[tokio::test]
async fn deleting_a_role_orphans_the_items_it_was_the_only_target_of() {
    // The composite foreign key cascades the targeting away; the item, which
    // is a decision somebody still owes, survives to be found.
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");

    sqlx::query("delete from roles where workspace_id = $1 and id = $2")
        .bind(ws)
        .bind(role)
        .execute(&db.pool)
        .await
        .expect("delete role");

    let orphans = store.orphaned(ws, 10).await.expect("orphaned");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, id);

    db.cleanup().await;
}

#[tokio::test]
async fn a_settled_item_with_no_targets_is_not_an_orphan() {
    // Orphaning is about open items nobody is waiting on.
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let id = store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");
    store
        .settle(ws, id, State::Resolved, Some(user))
        .await
        .expect("settle");
    store
        .remove_targets(ws, id, &[Target::User(user)])
        .await
        .expect("remove");

    assert!(store.orphaned(ws, 10).await.unwrap().is_empty());

    db.cleanup().await;
}

// --- expiry ---------------------------------------------------------------

#[tokio::test]
async fn an_expired_item_is_swept_out_of_the_queue() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let mut expiring = item("hitl.approval", vec![Target::User(user)]);
    expiring.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    store.raise(ws, expiring).await.expect("raise");

    // Still counted until the sweep runs: the count filters on state, not on
    // expiry, which is what lets it use the partial index.
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 1);

    let swept = store.sweep_expired(100).await.expect("sweep");
    assert_eq!(swept, 1);
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 0);

    db.cleanup().await;
}

#[tokio::test]
async fn the_sweep_leaves_items_that_have_not_expired() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let mut later = item("hitl.approval", vec![Target::User(user)]);
    later.expires_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
    store.raise(ws, later).await.expect("raise");
    // And one with no expiry at all.
    store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("raise");

    assert_eq!(store.sweep_expired(100).await.expect("sweep"), 0);
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 2);

    db.cleanup().await;
}

#[tokio::test]
async fn the_sweep_is_bounded_by_its_limit() {
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    for _ in 0..3 {
        let mut expiring = item("hitl.approval", vec![Target::User(user)]);
        expiring.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        store.raise(ws, expiring).await.expect("raise");
    }

    assert_eq!(store.sweep_expired(2).await.expect("sweep"), 2);
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 1);
    assert_eq!(store.sweep_expired(2).await.expect("sweep"), 1);
    assert_eq!(store.count_for_user(ws, user).await.unwrap(), 0);

    db.cleanup().await;
}

// --- delivery -------------------------------------------------------------

#[tokio::test]
async fn a_delivery_naming_nothing_does_not_announce() {
    // A no-op that reached the database would wake every client showing a
    // queue to refetch an unchanged answer.
    let (db, ws, store) = setup().await;

    let mut listener = sqlx::postgres::PgListener::connect_with(&db.pool)
        .await
        .expect("listener");
    listener
        .listen(outturn::api::actions::CHANNEL)
        .await
        .expect("listen");

    store
        .deliver(ws, Delivery::Invalidate { targets: vec![] })
        .await
        .expect("empty invalidate");
    store
        .deliver(
            ws,
            Delivery::Targeted {
                item_ids: vec![],
                targets: vec![Target::User(Uuid::now_v7())],
            },
        )
        .await
        .expect("empty targeted");

    let quiet = tokio::time::timeout(std::time::Duration::from_millis(300), listener.recv()).await;
    assert!(quiet.is_err(), "an empty delivery announced anyway");

    drop(listener);
    db.cleanup().await;
}

#[tokio::test]
async fn raising_an_item_announces_it_to_its_targets() {
    let (db, ws, store) = setup().await;
    let role = make_role(&db.pool, ws, "approvers").await;

    let mut listener = sqlx::postgres::PgListener::connect_with(&db.pool)
        .await
        .expect("listener");
    listener
        .listen(outturn::api::actions::CHANNEL)
        .await
        .expect("listen");

    store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("raise");

    let note = tokio::time::timeout(std::time::Duration::from_secs(5), listener.recv())
        .await
        .expect("announcement arrived")
        .expect("recv");
    let payload: serde_json::Value = serde_json::from_str(note.payload()).expect("json");

    assert_eq!(payload["workspace_id"], serde_json::json!(ws));
    assert_eq!(payload["roles"], serde_json::json!([role]));
    assert_eq!(payload["invalidate"], serde_json::json!(false));

    drop(listener);
    db.cleanup().await;
}

#[tokio::test]
async fn an_invalidation_says_that_it_is_one() {
    // So the path that is supposed to be rare can be found in the log.
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;

    let mut listener = sqlx::postgres::PgListener::connect_with(&db.pool)
        .await
        .expect("listener");
    listener
        .listen(outturn::api::actions::CHANNEL)
        .await
        .expect("listen");

    store
        .deliver(
            ws,
            Delivery::Invalidate {
                targets: vec![Target::User(user)],
            },
        )
        .await
        .expect("invalidate");

    let note = tokio::time::timeout(std::time::Duration::from_secs(5), listener.recv())
        .await
        .expect("announcement arrived")
        .expect("recv");
    let payload: serde_json::Value = serde_json::from_str(note.payload()).expect("json");

    assert_eq!(payload["invalidate"], serde_json::json!(true));
    assert_eq!(payload["users"], serde_json::json!([user]));

    drop(listener);
    db.cleanup().await;
}

#[tokio::test]
async fn a_rolled_back_raise_announces_nothing() {
    // pg_notify is transactional, so nobody is woken for an item that does
    // not exist. Exercised through a validation failure, which returns before
    // the transaction commits.
    let (db, ws, store) = setup().await;

    let mut listener = sqlx::postgres::PgListener::connect_with(&db.pool)
        .await
        .expect("listener");
    listener
        .listen(outturn::api::actions::CHANNEL)
        .await
        .expect("listen");

    let _ = store.raise(ws, item("hitl.approval", vec![])).await;

    let quiet = tokio::time::timeout(std::time::Duration::from_millis(300), listener.recv()).await;
    assert!(quiet.is_err(), "a refused raise announced anyway");

    drop(listener);
    db.cleanup().await;
}

// --- cross-workspace reads ------------------------------------------------
//
// The global badge is the one query in this design with no `workspace_id`
// predicate, so it is the one place a missing filter leaks another tenant's
// work into somebody's notification centre. The workspace set is derived from
// `user_workspace_roles` rather than supplied by the caller, and these tests
// are what hold that: each seeds a workspace the reader has no role in and
// asserts it contributes nothing.

#[tokio::test]
async fn a_global_count_spans_only_the_workspaces_you_hold_a_role_in() {
    let (db, mine, store) = setup().await;
    let user = make_user(&db.pool).await;
    let my_role = make_role(&db.pool, mine, "approvers").await;
    grant(&db.pool, user, mine, my_role).await;

    // A second workspace, with an item waiting on a role this user does not
    // hold. Nothing about it may reach them.
    let theirs = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(theirs)
        .bind(format!("T{theirs}"))
        .bind(format!("t-{}", theirs.simple()))
        .execute(&db.pool)
        .await
        .expect("their workspace");
    let their_role = make_role(&db.pool, theirs, "approvers").await;

    store
        .raise(mine, item("hitl.approval", vec![Target::Role(my_role)]))
        .await
        .expect("mine");
    store
        .raise(
            theirs,
            item("hitl.approval", vec![Target::Role(their_role)]),
        )
        .await
        .expect("theirs");

    let total = store
        .count_for_user_everywhere(user, 500)
        .await
        .expect("global count");
    assert_eq!(
        total, 1,
        "a workspace the reader has no role in was counted"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn a_global_read_never_returns_another_tenants_row() {
    let (db, mine, store) = setup().await;
    let user = make_user(&db.pool).await;
    let my_role = make_role(&db.pool, mine, "approvers").await;
    grant(&db.pool, user, mine, my_role).await;

    let theirs = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(theirs)
        .bind(format!("T{theirs}"))
        .bind(format!("t-{}", theirs.simple()))
        .execute(&db.pool)
        .await
        .expect("their workspace");
    let their_role = make_role(&db.pool, theirs, "approvers").await;

    // Targeted at the *same person* in a workspace they hold no role in. A
    // direct user target is the case a role-membership join alone would miss:
    // the row names them, and only the workspace set says it is not theirs to
    // see.
    store
        .raise(theirs, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("direct target elsewhere");
    // And one addressed to a role over there, which is the case the workspace
    // set alone would miss if the role join were dropped.
    store
        .raise(
            theirs,
            item("hitl.approval", vec![Target::Role(their_role)]),
        )
        .await
        .expect("role target elsewhere");
    let ours = store
        .raise(mine, item("hitl.approval", vec![Target::Role(my_role)]))
        .await
        .expect("mine");

    let rows = store
        .queue_for_user_everywhere(user, 50)
        .await
        .expect("global queue");
    let ids: Vec<Uuid> = rows.iter().map(|i| i.id).collect();
    assert_eq!(ids, vec![ours], "a row from another tenant was returned");
    assert!(
        rows.iter().all(|i| i.workspace_id == mine),
        "a row carried another workspace's id"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn losing_your_last_role_in_a_workspace_removes_it_from_the_global_count() {
    // Membership is what admits a workspace, so losing it must withdraw the
    // whole workspace rather than only the items targeted at that role.
    let (db, ws, store) = setup().await;
    let user = make_user(&db.pool).await;
    let role = make_role(&db.pool, ws, "approvers").await;
    grant(&db.pool, user, ws, role).await;

    // One targeted at the role, one at the person directly.
    store
        .raise(ws, item("hitl.approval", vec![Target::Role(role)]))
        .await
        .expect("role item");
    store
        .raise(ws, item("hitl.approval", vec![Target::User(user)]))
        .await
        .expect("direct item");
    assert_eq!(store.count_for_user_everywhere(user, 500).await.unwrap(), 2);

    revoke(&db.pool, user, ws, role).await;

    assert_eq!(
        store.count_for_user_everywhere(user, 500).await.unwrap(),
        0,
        "a workspace the reader no longer belongs to still counted"
    );

    db.cleanup().await;
}
