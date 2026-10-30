//! The notification center.

use super::*;

// --- the notification center ----------------------------------------------

#[tokio::test]
async fn the_queue_returns_what_is_waiting_on_the_reader() {
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let cookie = h
        .login_as("queue@test.invalid", None, Some((workspace, "admin")))
        .await;

    // Raised through the store, since nothing posts these yet.
    let store = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");
    {
        use outturn::api::actions::ActionStore as _;
        store
            .raise(
                workspace,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({"question": "approve?"}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise");
    }

    let (status, body) = get_with_cookie(&h, "/v1/action-items", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["items"].as_array().expect("items").len(), 1);
    assert_eq!(v["items"][0]["kind"], "approval.charge");

    let (status, body) = get_with_cookie(&h, "/v1/action-items/count", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["count"], 1);
    assert_eq!(v["capped"], false);

    h.db.cleanup().await;
}

#[tokio::test]
async fn the_queue_refuses_a_caller_with_no_session() {
    // Authenticated but not authorized is the design; unauthenticated is still
    // refused, and this is what says the first did not cost the second.
    let h = harness().await;

    let (status, _) = h.get("/v1/action-items", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = h.get("/v1/action-items/count", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    h.db.cleanup().await;
}

#[tokio::test]
async fn the_queue_shows_nothing_from_a_workspace_the_reader_left() {
    let h = harness().await;
    let mine = h.make_workspace("Mine", "mine").await;
    let theirs = h.make_workspace("Theirs", "theirs").await;
    let cookie = h
        .login_as("reader@test.invalid", None, Some((mine, "admin")))
        .await;
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");

    let store = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    {
        use outturn::api::actions::ActionStore as _;
        // Addressed to this very person, in a workspace they hold no role in.
        store
            .raise(
                theirs,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise elsewhere");
    }

    let (status, body) = get_with_cookie(&h, "/v1/action-items/count", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        v["count"], 0,
        "an item from a workspace the reader has no role in reached them"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_waiting_queue_read_wakes_when_something_is_raised() {
    // The long poll, and the reason the bus exists. Parks, then something is
    // raised for this person, and the parked read returns it well inside the
    // 25-second timeout -- so it woke on the notification rather than timing
    // out.
    let h = harness_parking_for(std::time::Duration::from_secs(3600)).await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let cookie = h
        .login_as("waiter@test.invalid", None, Some((workspace, "admin")))
        .await;
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");

    // Raised once the read is parked and the bus can hear it, waited for
    // rather than slept through. The read subscribes before it parks, so a
    // change made once it is counted is one it will hear.
    let pool = h.db.pool.clone();
    let state = Arc::clone(&h.state);
    let raiser = tokio::spawn(async move {
        state.action_bus().listening().await;
        while state.action_bus().waiters() == 0 {
            tokio::task::yield_now().await;
        }
        let store = outturn::api::actions::PostgresActionStore::new(pool);
        use outturn::api::actions::ActionStore as _;
        store
            .raise(
                workspace,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise");
    });

    // The park is one nothing reaches, so answering at all means the change
    // woke it: it answers with the item on timeout too, and only the clock
    // told the two apart before, which a loaded machine could fool.
    let (status, body) = get_with_cookie(&h, "/v1/action-items?wait=true", &cookie).await;
    raiser.await.expect("raiser");

    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["items"].as_array().expect("items").len(), 1);

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_waiting_read_returns_the_current_queue_when_nothing_happens() {
    // A timeout answers with what is there rather than empty: the queue is a
    // set, so "nothing changed" still has a current value.
    // A short park, since showing it answers on timeout means reaching one.
    let h = harness_parking_for(std::time::Duration::from_millis(200)).await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let cookie = h
        .login_as("quiet@test.invalid", None, Some((workspace, "admin")))
        .await;
    let user: Uuid = sqlx::query_scalar("select id from users where display_name = 'Test User'")
        .fetch_one(&h.db.pool)
        .await
        .expect("user");

    let store = outturn::api::actions::PostgresActionStore::new(h.db.pool.clone());
    {
        use outturn::api::actions::ActionStore as _;
        store
            .raise(
                workspace,
                outturn::api::actions::NewItem {
                    kind: "approval.charge".into(),
                    event_id: None,
                    inhibitor_id: None,
                    payload: serde_json::json!({}),
                    targets: vec![outturn::api::actions::Target::User(user)],
                    expires_at: None,
                },
            )
            .await
            .expect("raise");
    }

    // Raised before the read parks, so no notification arrives while it waits.
    // It should still answer with the item rather than an empty page.
    let (status, body) = get_with_cookie(&h, "/v1/action-items?wait=true", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["items"].as_array().expect("items").len(), 1);

    h.db.cleanup().await;
}
