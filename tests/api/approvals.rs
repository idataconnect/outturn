//! Approvals: holding a conversation, answering, and gated hosts.

use super::*;

// --- approvals ------------------------------------------------------------
//
// The whole path: a conversation is held, somebody is asked, they answer, and
// the parked turn is given back. See docs/approvals.md.

#[tokio::test]
async fn an_approval_holds_the_conversation_and_asks_somebody() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("admin role");

    let (status, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session,
            "requires": "charge",
            "reason": "£180 to payment account PA-4471",
            "roles": [role],
            "payload": { "amount_pence": 18000, "booking_id": "b-8812" }
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let raised: Value = serde_json::from_str(&body).expect("json");

    // A suspended hold, which nothing in this codebase took before.
    let strength: String = sqlx::query_scalar("select strength from inhibitors where id = $1")
        .bind(
            raised["inhibitor_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .fetch_one(&h.db.pool)
        .await
        .expect("hold");
    assert_eq!(strength, "suspended");

    // And it is in the asked role's queue, with what they need to decide.
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let queue: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(queue["items"].as_array().unwrap().len(), 1);
    assert_eq!(queue["items"][0]["kind"], "approval.charge");
    assert_eq!(queue["items"][0]["payload"]["amount_pence"], 18000);

    h.db.cleanup().await;
}

#[tokio::test]
async fn an_approval_addressed_to_nobody_is_refused() {
    // It would park the conversation for ever, and the orphan is only findable
    // by somebody who thinks to look.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    let (status, _) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session,
            "requires": "charge",
            "reason": "£180",
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Nothing was held.
    let holds: i64 = sqlx::query_scalar("select count(*) from inhibitors")
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(holds, 0, "a refused approval left a hold behind");

    h.db.cleanup().await;
}

#[tokio::test]
async fn an_approval_needs_a_reason_somebody_can_act_on() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (status, _) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "  ", "roles": [role]
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_conversation_in_another_workspace_cannot_be_held() {
    // Otherwise a caller could park somebody else's conversation by id.
    let h = harness().await;
    let mine = h.make_workspace("Mine", "mine").await;
    let theirs = h.make_workspace("Theirs", "theirs").await;
    let their_admin = h
        .login_as("them@test.invalid", None, Some((theirs, "admin")))
        .await;
    let (_, their_session) = a_held_conversation(&h, theirs, &their_admin).await;

    let my_admin = h
        .login_as("me@test.invalid", None, Some((mine, "admin")))
        .await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(mine)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (status, _) = post_with_cookie(
        &h,
        "/v1/approvals",
        &my_admin,
        &serde_json::json!({
            "session_id": their_session,
            "requires": "charge",
            "reason": "£180",
            "roles": [role]
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    h.db.cleanup().await;
}

#[tokio::test]
async fn approving_releases_the_hold_and_gives_the_turn_back() {
    // The end of the path, and the reason parking exists.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (agent, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180", "roles": [role]
        })
        .to_string(),
    )
    .await;
    let raised: Value = serde_json::from_str(&body).expect("json");
    let item = raised["id"].as_str().unwrap();
    let hold: Uuid = raised["inhibitor_id"].as_str().unwrap().parse().unwrap();

    // A turn parked under this conversation, as the worker would have left one.
    let job = outturn::jobs::enqueue(
        &h.db.pool,
        workspace,
        "chat.turn",
        serde_json::json!({
            "workspace_id": workspace, "session_id": session,
            "agent_id": agent, "message_id": Uuid::now_v7(),
        }),
        None,
        Some(&format!("session:{session}")),
        outturn::jobs::PRIORITY_REALTIME,
    )
    .await
    .expect("enqueue");
    let claimed = outturn::jobs::claim(
        &h.db.pool,
        &["chat.turn"],
        1,
        std::time::Duration::from_secs(45),
    )
    .await
    .expect("claim");
    outturn::jobs::park(&h.db.pool, job, claimed[0].job.lease_token)
        .await
        .expect("park");

    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let answered: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(answered["resumed"], 1, "the parked turn was not given back");

    // The hold is gone and the turn is claimable again.
    let still: i64 = sqlx::query_scalar("select count(*) from inhibitors where id = $1")
        .bind(hold)
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(still, 0, "approving left the hold on");
    let state: String = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(job)
        .fetch_one(&h.db.pool)
        .await
        .expect("state");
    assert_eq!(state, "pending");

    // And it has left the queue.
    let (_, body) = get_with_cookie(&h, "/v1/action-items/count", &admin).await;
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["count"], 0);

    h.db.cleanup().await;
}

#[tokio::test]
async fn declining_lets_the_conversation_carry_on() {
    // This used to assert the opposite, on the reasoning that "nothing has
    // changed about whether the work may proceed". True of the *work*, and the
    // grant is what enforces it -- a decline mints none, so the gate refuses the
    // same call again. But the hold is session-scoped, so leaving it up
    // suspended every later turn in the conversation, and the item was
    // `Cancelled` so nobody could answer it again and nothing else moves a job
    // out of `parked`. Declining one charge ended the conversation, recoverable
    // only by an operator releasing the hold by id.
    //
    // What stops the agent asking again immediately is `DECLINED_GUIDANCE` in
    // the transcript, not the hold.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£4000", "roles": [role]
        })
        .to_string(),
    )
    .await;
    let raised: Value = serde_json::from_str(&body).expect("json");
    let item = raised["id"].as_str().unwrap();
    let hold: Uuid = raised["inhibitor_id"].as_str().unwrap().parse().unwrap();

    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":false,"note":"too much for a first-time guest"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Zero parked turns to give back: this fixture holds a conversation without
    // one. What matters here is the hold, checked below -- `resumed` is
    // exercised against a real parked job in `tests/action_queue.rs`.
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["resumed"], 0);

    let still: i64 = sqlx::query_scalar("select count(*) from inhibitors where id = $1")
        .bind(hold)
        .fetch_one(&h.db.pool)
        .await
        .expect("count");
    assert_eq!(
        still, 0,
        "a declined approval must not leave the session suspended"
    );

    // And nothing was granted, which is what keeps the work from happening.
    let grants: i64 =
        sqlx::query_scalar("select count(*) from approval_grants where workspace_id = $1")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("grants");
    assert_eq!(grants, 0, "a decline grants nothing");

    h.db.cleanup().await;
}

#[tokio::test]
async fn answering_twice_is_refused() {
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;
    let role: Uuid =
        sqlx::query_scalar("select id from roles where workspace_id = $1 and name = 'admin'")
            .bind(workspace)
            .fetch_one(&h.db.pool)
            .await
            .expect("role");

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180", "roles": [role]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (first, _) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(first, StatusCode::OK);

    // Answered once already: the second is told it lost rather than silently
    // overwriting the first decision.
    let (second, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &admin,
        r#"{"approved":false}"#,
    )
    .await;
    assert_eq!(second, StatusCode::CONFLICT, "{body}");

    h.db.cleanup().await;
}

#[tokio::test]
async fn somebody_without_the_authority_cannot_answer() {
    // The two-user case: one may approve, the other may not, and it is an
    // authority rather than a prop.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("manager@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    // A role with everything an operator needs and no approvals:answer.
    let (status, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &admin,
        r#"{"name":"desk","authorities":["sessions:read","sessions:create","agents:read"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let desk_role: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180",
            "roles": [desk_role]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let clerk = h
        .login_as("clerk@test.invalid", None, Some((workspace, "desk")))
        .await;

    // It is in their queue -- they are who was asked --
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &clerk).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "the asked role did not see it"
    );

    // -- and they still may not answer it.
    let (status, _) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &clerk,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    h.db.cleanup().await;
}

#[tokio::test]
async fn an_approval_somebody_was_not_asked_about_is_not_theirs_to_answer() {
    // Targeting would be decorative if any holder of approvals:answer could
    // answer anything.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("manager@test.invalid", None, Some((workspace, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, workspace, &admin).await;

    // Addressed to a role nobody in this test holds.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &admin,
        r#"{"name":"finance","authorities":["approvals:answer"]}"#,
    )
    .await;
    let finance: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£180", "roles": [finance]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Another person who holds approvals:answer through a different role, and
    // who has an approval of their own waiting -- so their queue is not empty.
    // Without that, a lookup that ignored the id entirely would still refuse
    // this, and the test would pass while proving nothing.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &admin,
        r#"{"name":"ops","authorities":["approvals:answer","sessions:read"]}"#,
    )
    .await;
    let ops_role: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let other = h
        .login_as("ops@test.invalid", None, Some((workspace, "ops")))
        .await;

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &admin,
        &serde_json::json!({
            "session_id": session, "requires": "refund", "reason": "£20",
            "roles": [ops_role]
        })
        .to_string(),
    )
    .await;
    let theirs = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(theirs, item, "the two approvals must be different items");
    // Left unanswered on purpose. Their queue has to be non-empty *at the
    // moment of the refusal below*, or a lookup that ignored the id and took
    // whatever was first would find nothing and refuse for the wrong reason --
    // which is exactly what an earlier version of this test did.
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &other).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let queue = serde_json::from_str::<Value>(&body).unwrap();
    assert_eq!(
        queue["items"].as_array().unwrap().len(),
        1,
        "the second reader should have exactly their own waiting: {body}"
    );
    assert_eq!(queue["items"][0]["id"], theirs);

    let (status, _) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &other,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "somebody answered an approval they were not asked about"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn answering_needs_the_authority_in_the_items_own_workspace() {
    // The queue read is global by design, so an item can come from a workspace
    // the token was not minted for -- and the authority has to be resolved
    // there rather than against whichever workspace the caller happens to be
    // signed into. Otherwise holding approvals:answer in one workspace
    // authorizes payments in every other one the person belongs to.
    let h = harness().await;
    let armed = h.make_workspace("Armed", "armed").await;
    let unarmed = h.make_workspace("Unarmed", "unarmed").await;

    // Each workspace's own admin, since a session is created in the workspace
    // the token names.
    let armed_admin = h
        .login_as("armed@test.invalid", None, Some((armed, "admin")))
        .await;
    let unarmed_admin = h
        .login_as("unarmed@test.invalid", None, Some((unarmed, "admin")))
        .await;
    let (_, session) = a_held_conversation(&h, unarmed, &unarmed_admin).await;

    // The role carrying approvals:answer lives in `armed`.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &armed_admin,
        r#"{"name":"finance","authorities":["approvals:answer","sessions:read"]}"#,
    )
    .await;
    assert!(body.contains("finance"), "{body}");

    // And the targeted role in `unarmed` carries no such authority.
    let (_, body) = post_with_cookie(
        &h,
        "/v1/roles",
        &unarmed_admin,
        r#"{"name":"desk","authorities":["sessions:read"]}"#,
    )
    .await;
    let desk: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (_, body) = post_with_cookie(
        &h,
        "/v1/approvals",
        &unarmed_admin,
        &serde_json::json!({
            "session_id": session, "requires": "charge", "reason": "£4000",
            "roles": [desk]
        })
        .to_string(),
    )
    .await;
    let item = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // One person in both workspaces: finance in `armed`, desk in `unarmed`.
    let split = h
        .login_as("split@test.invalid", None, Some((armed, "finance")))
        .await;
    let split_id: Uuid = sqlx::query_scalar(
        "select u.id from users u join user_identities i on i.user_id = u.id \
         where i.provider_subject = 'split@test.invalid'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("the split user");
    h.users
        .grant_workspace_role(split_id, unarmed, "desk")
        .await
        .expect("grant desk in the other workspace");

    // It is in their queue -- the read is global and they hold the targeted
    // role there.
    let (status, body) = get_with_cookie(&h, "/v1/action-items", &split).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "the global read should have reached the other workspace: {body}"
    );

    // And they must not be able to answer it: their approvals:answer is in
    // `armed`, and this item is `unarmed`'s.
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/approvals/{item}/answer"),
        &split,
        r#"{"approved":true}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an authority held in another workspace authorized this: {body}"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_gate_declaration_with_no_host_is_refused_at_publish() {
    // The file would say an operation is gated and nothing could gate it: the
    // gate rows are written per declared host, so a version naming none stores
    // nothing, `gates_for_turn` returns the empty set, and the charge goes out.
    // That is the quiet failure the whole mechanism is arranged against, and the
    // fix is one line in the publish -- so it is refused rather than accepted.
    let h = harness().await;
    let workspace = h.make_workspace("Hollowbrook", "hollowbrook").await;
    let admin = h
        .login_as("desk@test.invalid", None, Some((workspace, "admin")))
        .await;

    let body = serde_json::json!({
        "slug": "charging",
        "name": "Charging",
        "body": "One operation. Read charge.md before using it.",
        "files": [{
            "path": "charge.md",
            "content": "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n",
        }],
    })
    .to_string();

    let (status, message) = post_with_cookie(&h, "/v1/skills", &admin, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{message}");
    assert!(
        message.contains("names no host"),
        "the refusal should say what is missing: {message}"
    );

    // And the same skill with a host publishes.
    let with_host = serde_json::json!({
        "slug": "charging",
        "name": "Charging",
        "body": "One operation. Read charge.md before using it.",
        // A public name, since a cluster-internal one needs the operator allowlist
        // and this test is about gates rather than about egress.
        "hosts": ["api.hollowbrook.test"],
        "files": [{
            "path": "charge.md",
            "content": "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n",
        }],
    })
    .to_string();
    let (status, message) = post_with_cookie(&h, "/v1/skills", &admin, &with_host).await;
    assert_eq!(status, StatusCode::CREATED, "{message}");

    // Stored as the egress matcher spells it, which is what the gateway compares
    // against -- `normalize_host` strips a port and lower-cases, so a gate written
    // from the host as typed would never match the request.
    let host: String = sqlx::query_scalar(
        "select host from skill_version_gates \
         where path = 'charge.md' and requires = 'charge'",
    )
    .fetch_one(&h.db.pool)
    .await
    .expect("a gate row");
    assert_eq!(host, "api.hollowbrook.test");

    h.db.cleanup().await;
}

#[tokio::test]
async fn approve_new_hosts_gates_a_hand_added_host_and_not_a_skills_own() {
    // The setting, end to end: what it turns into, and what it exempts. A host a
    // skill's declaration opened was consented to when the skill was installed --
    // in an act that named the skill and the host together -- so asking again per
    // conversation would be asking the same question somewhere worse. A host
    // somebody added by hand says agents *may* reach it, not that any particular
    // use of it was reviewed, and that is the case the setting is for.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@test.invalid", None, Some((workspace, "admin")))
        .await;

    // One host from a skill, one added by hand.
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "brought",
            "name": "Brought By A Skill",
            "body": "Reaches api.brought.test.",
            "hosts": ["api.brought.test"],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/skills/{skill}/hosts/approve"),
        &admin,
        "{}",
    )
    .await;
    assert!(
        status.is_success(),
        "approving the skill's host: {status} {body}"
    );

    let (status, body) = post_with_cookie(
        &h,
        "/v1/egress-rules",
        &admin,
        &serde_json::json!({ "host": "api.byhand.test" }).to_string(),
    )
    .await;
    assert!(
        status.is_success(),
        "adding a rule by hand: {status} {body}"
    );

    // Off by default: nothing is gated.
    let rules = outturn::api::egress::rules_for(&h.db.pool, workspace)
        .await
        .expect("rules");
    let hosts: Vec<String> = rules.iter().map(|r| r.host.clone()).collect();
    assert!(hosts.contains(&"api.byhand.test".to_string()), "{hosts:?}");
    assert!(hosts.contains(&"api.brought.test".to_string()), "{hosts:?}");

    let exempt = outturn::api::egress::hosts_from_skills(&h.db.pool, workspace)
        .await
        .expect("skill hosts");
    assert_eq!(
        exempt,
        vec!["api.brought.test".to_string()],
        "only the skill's host should be exempt"
    );

    // What the setting turns into.
    let gates = outturn::egress::gate::Gates::of(outturn::egress::gate::for_unreviewed_hosts(
        &hosts, &exempt,
    ));
    assert!(
        gates
            .covering("api.byhand.test", "GET", "/anything")
            .is_some(),
        "the hand-added host should need approving"
    );
    assert!(
        gates
            .covering("api.brought.test", "GET", "/anything")
            .is_none(),
        "the skill's own host was already consented to"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_skill_with_two_hosts_gates_both_of_them() {
    // The gate row was keyed on `(version_id, path, requires)` with no host,
    // while the write loops over the skill's hosts with `on conflict do
    // nothing` -- so the second host's gate was silently dropped and requests
    // to it went out unapproved. A gate failing open, with nothing saying so.
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let charge_file = "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n";
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "two-hosts",
            "name": "Two hosts",
            "body": "One operation, two places it can go.",
            "hosts": ["api.one.test", "api.two.test"],
            "files": [{ "path": "charge.md", "content": charge_file }],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let hosts: Vec<String> = sqlx::query_scalar(
        "select g.host from skill_version_gates g \
           join skill_versions v on v.id = g.version_id \
           join skills s on s.id = v.skill_id \
          where s.slug = 'two-hosts' order by g.host",
    )
    .fetch_all(&h.db.pool)
    .await
    .expect("gates");

    assert_eq!(
        hosts,
        vec!["api.one.test".to_string(), "api.two.test".to_string()],
        "both hosts must be gated; a dropped one is a request nobody gates"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_body_only_edit_keeps_the_gates_its_files_declare() {
    // The quiet failure the whole mechanism is arranged against, found by review.
    //
    // Files carry forward when a version brings none of its own -- that is how
    // somebody fixes a typo in a skill's prose. The gates have to carry with them,
    // because the declaration lives *in* those files. They did not: the carry
    // branch keyed on `based_on`, which is the latest version of the *base* skill
    // and is null for every ordinary skill, so neither branch ran and the new
    // version had no gates at all. The file still said the operation needed a
    // charge, `gates_for_turn` returned the empty set, and every turn after the
    // edit was ungated with nothing logged and nothing refused.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@test.invalid", None, Some((workspace, "admin")))
        .await;

    let charge_file = "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [amount_pence]\n---\n\n# charge\n";
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "charging",
            "name": "Charging",
            "body": "One operation. Read charge.md.",
            "hosts": ["api.hollowbrook.test"],
            "files": [{ "path": "charge.md", "content": charge_file }],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let gates_for = |version: Uuid| {
        let pool = h.db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "select count(*) from skill_version_gates where version_id = $1",
            )
            .bind(version)
            .fetch_one(&pool)
            .await
            .expect("count")
        }
    };

    let first: Uuid = sqlx::query_scalar(
        "select id from skill_versions where skill_id = $1 order by ordinal desc limit 1",
    )
    .bind(skill)
    .fetch_one(&h.db.pool)
    .await
    .expect("the first version");
    assert_eq!(
        gates_for(first).await,
        1,
        "the first version should be gated"
    );

    // A reworded body and no files: the ordinary way a typo is fixed.
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/skills/{skill}/versions"),
        &admin,
        &serde_json::json!({
            "body": "One operation. Read charge.md before charging anybody.",
            "hosts": ["api.hollowbrook.test"],
        })
        .to_string(),
    )
    .await;
    assert!(status.is_success(), "{status} {body}");

    let second: Uuid = sqlx::query_scalar(
        "select id from skill_versions where skill_id = $1 order by ordinal desc limit 1",
    )
    .bind(skill)
    .fetch_one(&h.db.pool)
    .await
    .expect("the second version");
    assert_ne!(second, first, "a version should have been appended");

    // The files carried forward, so the declaration did too.
    let files: i64 =
        sqlx::query_scalar("select count(*) from skill_version_files where version_id = $1")
            .bind(second)
            .fetch_one(&h.db.pool)
            .await
            .expect("files");
    assert_eq!(files, 1, "the file should have carried forward");
    assert_eq!(
        gates_for(second).await,
        1,
        "the gate its file declares was dropped, so every later turn is ungated"
    );

    // And what the gate binds, which is the half a count of gates cannot see.
    // A gate whose binds did not travel is not ungated -- it still refuses -- but
    // every grant taken out under it is keyed on the path alone, so one approved
    // charge covers a charge for any amount. Counting gates alone passed while
    // that was true.
    let binds: Vec<String> = sqlx::query_scalar(
        "select field from skill_version_gate_binds where version_id = $1 order by position",
    )
    .bind(second)
    .fetch_all(&h.db.pool)
    .await
    .expect("binds");
    assert_eq!(
        binds,
        vec!["amount_pence".to_string()],
        "the bound fields did not carry forward, so later grants bind nothing"
    );

    h.db.cleanup().await;
}

#[tokio::test]
async fn a_wildcard_host_is_never_exempt_from_the_ceiling() {
    // Approving a skill's hosts takes `settings:update`, which is also what turns
    // the ceiling on -- so whoever sets it can exempt a host from it, and
    // `normalize_host` permits a wildcard over a domain. Declaring `*.example.com`
    // in a skill and approving it would exempt every host under it from every turn.
    // (`*.com` is refused outright, since `com` has no domain of its own.)
    //
    // Not an escalation across an authority boundary, but a wider door than the
    // setting reads as having, and the same hazard the `covers` section of
    // docs/approvals.md is about: approving the instance you were shown is not
    // approving the class it belongs to.
    let h = harness().await;
    let workspace = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@test.invalid", None, Some((workspace, "admin")))
        .await;

    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "wide",
            "name": "Wide Open",
            "body": "Reaches anything.",
            // `*.com` is already refused -- `com` has no domain -- so the widest
            // form actually reachable is a wildcard over a real domain.
            "hosts": ["*.example.com", "api.named.test"],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill: Uuid = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (status, body) = post_with_cookie(
        &h,
        &format!("/v1/skills/{skill}/hosts/approve"),
        &admin,
        "{}",
    )
    .await;
    assert!(status.is_success(), "{status} {body}");

    let exempt = outturn::api::egress::hosts_from_skills(&h.db.pool, workspace)
        .await
        .expect("skill hosts");
    assert!(
        exempt.contains(&"api.named.test".to_string()),
        "a named host a skill brought should be exempt: {exempt:?}"
    );
    assert!(
        !exempt.iter().any(|h| h.contains('*')),
        "a wildcard was exempted: {exempt:?}"
    );

    h.db.cleanup().await;
}

/// An approval says what is being approved: each field the grant will be keyed
/// on, with the value this request carries. A person used to be asked to
/// approve a charge and shown only the path it was posted to -- never its
/// amount -- so saying yes was saying yes to a number they had not seen.
///
/// Also the first test through `/v1/work/gated` itself, the way a runtime
/// reports a refusal.
#[tokio::test]
async fn an_approval_shows_the_values_it_approves() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let charge_file = "---\napproval:\n  requires: charge\n  matches: POST /charges\n  binds: [booking_id, amount_pence]\n---\n\n# charge\n";
    let (status, body) = post_with_cookie(
        &h,
        "/v1/skills",
        &admin,
        &serde_json::json!({
            "slug": "charging",
            "name": "Charging",
            "body": "Read charge.md before charging anybody.",
            "hosts": ["api.hollowbrook.test"],
            "files": [{ "path": "charge.md", "content": charge_file }],
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let skill = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) =
        post_with_cookie(&h, &format!("/v1/skills/{skill}/hosts/approve"), &admin, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (agent, session) = a_held_conversation(&h, acme, &admin).await;
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/agents/{agent}/skills"))
        .header("cookie", format!("outturn_session={admin}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"[{{"skill_id":"{skill}"}}]"#)))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The runtime reports the refusal, as it does when the gateway turns the
    // charge away.
    let (job, lease) = a_running_turn(&h, acme, agent, session).await;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/work/gated")
        .header("authorization", format!("Bearer {TEST_RUNTIME_KEY}"))
        .header(outturn::api::work::LEASE_HEADER, lease.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "session_id": session,
                "job_id": job,
                "method": "POST",
                "host": "api.hollowbrook.test",
                "path": "/charges",
                "body": r#"{"amount_pence":78000,"booking_id":"bk_1","note":"not bound"}"#,
            })
            .to_string(),
        ))
        .unwrap();
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let expected = serde_json::json!([
        { "field": "booking_id", "value": "bk_1" },
        { "field": "amount_pence", "value": 78000 },
    ]);

    // In the queue, where the inbox reads it.
    let (_, body) = get_with_cookie(&h, "/v1/action-items", &admin).await;
    let queue = items(&body);
    assert_eq!(queue.len(), 1, "{body}");
    assert_eq!(queue[0]["payload"]["binds"], expected, "{body}");

    // And with the conversation, where the banner reads it.
    let (_, body) = get_with_cookie(
        &h,
        &format!("/v1/agent-sessions/{session}/messages"),
        &admin,
    )
    .await;
    let history: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(history["awaiting"]["binds"], expected, "{body}");

    h.db.cleanup().await;
}
