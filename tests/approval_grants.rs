//! What an approval is worth once somebody has given it, against a real
//! Postgres.
//!
//! The loop these cover is the one `docs/approvals.md` recorded as missing: a
//! turn refused for want of an approval, held, answered, and then resumed
//! *without being refused a second time*. That second refusal is what happens
//! when a yes releases the hold and nothing else, and it is invisible from any
//! single tier -- the hold looks lifted, the turn looks resumed, and the request
//! is refused exactly as before.
//!
//! What a grant *permits* is not tested here: that is pure, it is in
//! `egress::grant`, and it is unit-tested beside the digest it compares. What
//! needs a database is the half this file is about -- that a grant is written
//! with the settle that earned it, that it is scoped to the turn and the
//! workspace, and that a resumed turn reads back exactly what it still holds.
//!
//! Built only under the integration-tests feature; needs TEST_DATABASE_URL.

use outturn::api::grant::{self, NewGrant};
use outturn::egress::grant::{Extent, Granted};
use uuid::Uuid;

mod common;

async fn setup() -> (common::TestDb, Uuid) {
    let db = common::TestDb::new().await;
    let workspace_id = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(workspace_id)
        .bind(format!("T{workspace_id}"))
        .bind(format!("t-{}", workspace_id.simple()))
        .execute(&db.pool)
        .await
        .expect("workspace");
    (db, workspace_id)
}

fn grant_for(workspace_id: Uuid, job_id: Uuid, extent: Extent, keyed_on: &str) -> NewGrant {
    NewGrant {
        workspace_id,
        session_id: Uuid::now_v7(),
        job_id,
        requires: "charge".into(),
        extent,
        keyed_on: keyed_on.into(),
        granted_by: None,
    }
}

/// What a resumed turn carries: the grants it still holds, whole.
///
/// Whole rather than the acts they name, which was the bug. Returning bare
/// `requires` let the caller drop every gate declaring that act, so one approved
/// GET disabled `approve_new_hosts` for every host the turn could reach.
#[tokio::test]
async fn a_resumed_turn_reads_back_what_it_holds() {
    let (db, ws) = setup().await;
    let job = Uuid::now_v7();

    grant::write(
        &db.pool,
        grant_for(ws, job, Extent::Call, "the-approved-charge"),
    )
    .await
    .expect("write");

    let held = grant::live_for(&db.pool, ws, job).await.expect("live_for");
    assert_eq!(
        held,
        vec![Granted {
            requires: "charge".into(),
            extent: Extent::Call,
            keyed_on: "the-approved-charge".into(),
        }],
        "a grant must come back with its extent and its key, not just its act"
    );

    db.cleanup().await;
}

/// A grant belongs to the turn it was given for. Nothing granted at nine reaches
/// a call at five -- the bound that keeps an approval from becoming a standing
/// authority.
#[tokio::test]
async fn a_grant_does_not_reach_another_turn() {
    let (db, ws) = setup().await;

    grant::write(
        &db.pool,
        grant_for(ws, Uuid::now_v7(), Extent::Call, "a-charge"),
    )
    .await
    .expect("write");

    let later = grant::live_for(&db.pool, ws, Uuid::now_v7())
        .await
        .expect("live_for");
    assert!(
        later.is_empty(),
        "a grant must die with its turn, or one yes covers the rest of the session"
    );

    db.cleanup().await;
}

/// And to its workspace, which is the isolation boundary everything else rests
/// on.
#[tokio::test]
async fn a_grant_does_not_reach_another_workspace() {
    let (db, ws) = setup().await;
    let job = Uuid::now_v7();
    let other = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(other)
        .bind(format!("T{other}"))
        .bind(format!("t-{}", other.simple()))
        .execute(&db.pool)
        .await
        .expect("workspace");

    grant::write(&db.pool, grant_for(ws, job, Extent::Call, "a-charge"))
        .await
        .expect("write");

    let theirs = grant::live_for(&db.pool, other, job)
        .await
        .expect("live_for");
    assert!(
        theirs.is_empty(),
        "a grant must not cross a workspace boundary"
    );

    db.cleanup().await;
}

/// Grants come back in the order they were given, because that order is part of
/// the commitment the turn token carries.
#[tokio::test]
async fn grants_come_back_in_a_stable_order() {
    let (db, ws) = setup().await;
    let job = Uuid::now_v7();

    for keyed_on in ["first", "second", "third"] {
        grant::write(&db.pool, grant_for(ws, job, Extent::Call, keyed_on))
            .await
            .expect("write");
    }

    let held = grant::live_for(&db.pool, ws, job).await.expect("live_for");
    let keys: Vec<&str> = held.iter().map(|g| g.keyed_on.as_str()).collect();
    assert_eq!(keys, vec!["first", "second", "third"]);

    db.cleanup().await;
}

/// A `unit` grant survives the round trip as a unit grant.
///
/// The extent is half of what a grant means, and a row whose extent did not come
/// back would be read as the other kind -- a unit grant misread as a call grant
/// is keyed on a booking id and compared against a digest, which matches nothing
/// and silently revokes an approval somebody gave.
#[tokio::test]
async fn a_unit_grant_keeps_its_extent() {
    let (db, ws) = setup().await;
    let job = Uuid::now_v7();

    grant::write(&db.pool, grant_for(ws, job, Extent::Unit, "bk_8812"))
        .await
        .expect("write");

    let held = grant::live_for(&db.pool, ws, job).await.expect("live_for");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].extent, Extent::Unit);
    assert_eq!(held[0].keyed_on, "bk_8812");

    db.cleanup().await;
}

/// A row this build cannot interpret is left out rather than guessed at.
///
/// Unreachable today, and deliberately still handled. The check constraint
/// refuses an extent this build does not know, so the only way such a row exists
/// is a migration that widened the constraint against a pod that has not been
/// replaced yet -- a rolling deploy, which is the ordinary way this platform
/// ships. Narrowing is the safe direction: a grant nobody can interpret must not
/// become one that permits everything.
///
/// Driven by widening the constraint the way that migration would, rather than
/// by pretending the constraint is not there -- which is what makes this a test
/// of `live_for` rather than a test of the fixture.
#[tokio::test]
async fn a_grant_with_an_unknown_extent_is_left_out() {
    let (db, ws) = setup().await;
    let job = Uuid::now_v7();

    grant::write(&db.pool, grant_for(ws, job, Extent::Call, "a-charge"))
        .await
        .expect("write");

    sqlx::query("alter table approval_grants drop constraint approval_grants_extent_check")
        .execute(&db.pool)
        .await
        .expect("widen the constraint as a later migration would");

    sqlx::query(
        "insert into approval_grants \
         (workspace_id, id, session_id, job_id, requires, extent, keyed_on) \
         values ($1, $2, $3, $4, 'charge', 'something-newer', 'whatever')",
    )
    .bind(ws)
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(job)
    .execute(&db.pool)
    .await
    .expect("insert");

    let held = grant::live_for(&db.pool, ws, job).await.expect("live_for");
    assert_eq!(
        held.len(),
        1,
        "only the grant this build understands should travel"
    );
    assert_eq!(held[0].keyed_on, "a-charge");

    db.cleanup().await;
}

/// What a settled approval leaves in the conversation.
///
/// Without it the transcript keeps a refused `POST /charges` that was later
/// approved and succeeded, with nothing joining them -- and a decline leaves no
/// trace at all. The banner is live-only, so a reader who reloads sees a pause
/// that never happened.
#[tokio::test]
async fn answering_an_approval_is_recorded_in_the_conversation() {
    use outturn::api::chat::{ApprovalAnswer, ChatStore, PostgresChatStore};

    let (db, ws) = setup().await;
    let agent = Uuid::now_v7();
    sqlx::query(
        "insert into agents (id, workspace_id, name, slug, system_prompt, enabled) \
         values ($1, $2, 'T', 'agent-' || replace($1::text, '-', ''), '', true)",
    )
    .bind(agent)
    .bind(ws)
    .execute(&db.pool)
    .await
    .expect("agent");

    let user = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, 'Ada')")
        .bind(user)
        .execute(&db.pool)
        .await
        .expect("user");

    let chat = PostgresChatStore::new(db.pool.clone());
    let session = chat
        .create_session(
            ws,
            user,
            outturn::api::chat::CreateSession {
                agent_id: agent,
                title: String::new(),
                account: None,
            },
        )
        .await
        .expect("session");

    let recorded = chat
        .record_approval(
            session.id,
            ApprovalAnswer {
                requires: "charge",
                approved: true,
                answered_by: None,
                answered_by_name: Some("Ada"),
                note: Some("checked with the guest"),
            },
        )
        .await
        .expect("record");

    // Marked, so the client can draw it as a boundary rather than as speech.
    let mark = &recorded.metadata[outturn::api::chat::APPROVAL_MARK];
    assert_eq!(mark["requires"], "charge");
    assert_eq!(mark["approved"], true);
    assert_eq!(mark["answered_by_name"], "Ada");
    assert_eq!(mark["note"], "checked with the guest");

    // And says it in words, so a client that does not know the mark still shows
    // something true.
    assert!(recorded.content.contains("Ada"), "{}", recorded.content);
    assert!(
        recorded.content.contains("approved"),
        "{}",
        recorded.content
    );

    db.cleanup().await;
}

/// A decline is recorded too. It is the one that leaves the conversation
/// stopped, so a transcript that does not say it happened is the one a person
/// would most want.
#[tokio::test]
async fn a_decline_is_recorded_as_a_decline() {
    use outturn::api::chat::{ApprovalAnswer, ChatStore, PostgresChatStore};

    let (db, ws) = setup().await;
    let agent = Uuid::now_v7();
    sqlx::query(
        "insert into agents (id, workspace_id, name, slug, system_prompt, enabled) \
         values ($1, $2, 'T', 'agent-' || replace($1::text, '-', ''), '', true)",
    )
    .bind(agent)
    .bind(ws)
    .execute(&db.pool)
    .await
    .expect("agent");

    let user = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, 'Ada')")
        .bind(user)
        .execute(&db.pool)
        .await
        .expect("user");

    let chat = PostgresChatStore::new(db.pool.clone());
    let session = chat
        .create_session(
            ws,
            user,
            outturn::api::chat::CreateSession {
                agent_id: agent,
                title: String::new(),
                account: None,
            },
        )
        .await
        .expect("session");

    let recorded = chat
        .record_approval(
            session.id,
            ApprovalAnswer {
                requires: "charge",
                approved: false,
                answered_by: None,
                answered_by_name: None,
                note: None,
            },
        )
        .await
        .expect("record");

    assert_eq!(
        recorded.metadata[outturn::api::chat::APPROVAL_MARK]["approved"],
        false
    );
    assert!(
        recorded.content.contains("declined"),
        "{}",
        recorded.content
    );
    // Lifting the hold is what keeps the conversation usable, so something has
    // to stop the agent calling again a second later and asking the same
    // question. The transcript is where it is told.
    assert!(
        recorded
            .content
            .contains(outturn::api::chat::DECLINED_GUIDANCE),
        "a declined agent must be told to talk to the user first: {}",
        recorded.content
    );
    // Not a prohibition. Somebody who declines and then changes their mind must
    // be able to say so, so the agent has to be free to try again afterwards --
    // the gate is what holds the line, by minting no grant.
    // Asserted against the constant rather than by hunting for words: the note
    // a person typed is interpolated into this same string, so "never mind" in
    // a decline note would fail a substring search for "never".
    assert!(
        outturn::api::chat::DECLINED_GUIDANCE.contains("before discussing"),
        "the guidance defers the call rather than forbidding it: {}",
        outturn::api::chat::DECLINED_GUIDANCE
    );

    db.cleanup().await;
}

/// A turn resuming after an approval writes a *new* reply, leaving the refused
/// one where it is.
///
/// The bug this closes: the resumed turn used to take back the reply it had
/// already made and stream over it, so the refusal the reader approved against
/// was overwritten. The transcript ended up showing a charge that succeeded,
/// with a green "approved" beside it and nothing in it that had ever needed
/// approving.
#[tokio::test]
async fn a_turn_resuming_after_an_approval_keeps_the_refused_reply() {
    use outturn::api::chat::{ChatStore, PostgresChatStore, Usage};

    let (db, ws) = setup().await;
    let (chat, prompt) = a_prompt(&db, ws).await;

    // The refused attempt, finished with what the reader saw.
    let first = chat
        .claim_placeholder(prompt, prompt_session(&db, prompt).await, 1)
        .await
        .expect("first");
    chat.set_message_content(
        first.message.id,
        "I need approval for that charge.",
        None,
        None,
        Usage::default(),
        serde_json::json!({}),
    )
    .await
    .expect("finish");

    // Resuming asks for the next attempt; a crashed retry would not.
    //
    // The flag is "a person answered", not "a grant was minted" -- a decline
    // answers and grants nothing, and it must keep the refused reply just as an
    // approval does. Taking it back overwrites what the person read when they
    // decided, and leaves the resumed turn no refusal in its history, so the
    // model re-derives the task and calls the gate again.
    assert_eq!(
        chat.attempt_for(prompt, true).await.expect("resuming"),
        2,
        "a resumed turn must not take back the reply somebody answered against"
    );
    assert_eq!(
        chat.attempt_for(prompt, false).await.expect("retrying"),
        1,
        "a crashed retry still takes its own attempt back"
    );

    let second = chat
        .claim_placeholder(prompt, prompt_session(&db, prompt).await, 2)
        .await
        .expect("second");
    assert_ne!(
        second.message.id, first.message.id,
        "the resumed turn wrote a new reply"
    );

    // And the refusal is still there.
    let still: String = sqlx::query_scalar("select content from agent_messages where id = $1")
        .bind(first.message.id)
        .fetch_one(&db.pool)
        .await
        .expect("first still there");
    assert_eq!(still, "I need approval for that charge.");

    db.cleanup().await;
}

/// An attempt that never finished is taken back rather than added to.
///
/// A turn refused before its first token leaves an empty reply. Starting a new
/// attempt past it would strand that one, and an empty assistant message with
/// no live job is exactly what the abandoned-placeholder guard trips over --
/// which wedges the session.
#[tokio::test]
async fn an_unfinished_attempt_is_taken_back_even_when_resuming() {
    use outturn::api::chat::{ChatStore, PostgresChatStore};

    let (db, ws) = setup().await;
    let (chat, prompt) = a_prompt(&db, ws).await;

    chat.claim_placeholder(prompt, prompt_session(&db, prompt).await, 1)
        .await
        .expect("placeholder");

    assert_eq!(
        chat.attempt_for(prompt, true).await.expect("resuming"),
        1,
        "an attempt that never finished has nothing worth keeping"
    );

    db.cleanup().await;
}

/// `finished_at` is when a reply stopped being written, not when it was made.
#[tokio::test]
async fn a_reply_records_when_it_finished() {
    use outturn::api::chat::{ChatStore, PostgresChatStore, Usage};

    let (db, ws) = setup().await;
    let (chat, prompt) = a_prompt(&db, ws).await;

    let reply = chat
        .claim_placeholder(prompt, prompt_session(&db, prompt).await, 1)
        .await
        .expect("placeholder");

    let before: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select finished_at from agent_messages where id = $1")
            .bind(reply.message.id)
            .fetch_one(&db.pool)
            .await
            .expect("read");
    assert!(before.is_none(), "an unfinished reply has no finish time");

    chat.set_message_content(
        reply.message.id,
        "done",
        None,
        None,
        Usage::default(),
        serde_json::json!({}),
    )
    .await
    .expect("finish");

    let after: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select finished_at from agent_messages where id = $1")
            .bind(reply.message.id)
            .fetch_one(&db.pool)
            .await
            .expect("read");
    assert!(after.is_some(), "a finished reply records when");

    db.cleanup().await;
}

/// A session with an agent and one user prompt, which is what an attempt hangs
/// off.
async fn a_prompt(db: &common::TestDb, ws: Uuid) -> (outturn::api::chat::PostgresChatStore, Uuid) {
    use outturn::api::chat::{ChatStore, Delivery, PostgresChatStore, Usage};

    let agent = Uuid::now_v7();
    sqlx::query(
        "insert into agents (id, workspace_id, name, slug, system_prompt, enabled) \
         values ($1, $2, 'T', 'agent-' || replace($1::text, '-', ''), '', true)",
    )
    .bind(agent)
    .bind(ws)
    .execute(&db.pool)
    .await
    .expect("agent");

    let user = Uuid::now_v7();
    sqlx::query("insert into users (id, display_name) values ($1, 'Ada')")
        .bind(user)
        .execute(&db.pool)
        .await
        .expect("user");

    let chat = PostgresChatStore::new(db.pool.clone());
    let session = chat
        .create_session(
            ws,
            user,
            outturn::api::chat::CreateSession {
                agent_id: agent,
                title: String::new(),
                account: None,
            },
        )
        .await
        .expect("session");

    let prompt = chat
        .append_message(
            session.id,
            "user",
            "charge it",
            None,
            Usage::default(),
            Delivery::Steer,
            Some(user),
        )
        .await
        .expect("prompt");

    (chat, prompt.id)
}

/// The session a prompt belongs to.
async fn prompt_session(db: &common::TestDb, prompt: Uuid) -> Uuid {
    sqlx::query_scalar("select session_id from agent_messages where id = $1")
        .bind(prompt)
        .fetch_one(&db.pool)
        .await
        .expect("session")
}

/// A decline keeps the refused reply, exactly as an approval does.
///
/// The bug: the attempt was chosen from whether the turn held a *grant*, and a
/// decline mints none -- so a declined turn fell through to "a crashed retry
/// takes its own attempt back" and overwrote the very reply the person had just
/// read and refused. Nothing was left saying what had been declined, and the
/// resumed turn's history had no refusal in it either, so the model re-derived
/// the task from the bare prompt and called the gate again -- raising a fresh
/// approval nobody asked for, which is what the decline guidance exists to stop.
///
/// The predicate is "a person answered", which a decline satisfies.
#[tokio::test]
async fn a_declined_turn_keeps_the_reply_it_was_refused_on() {
    use outturn::api::chat::{ChatStore, PostgresChatStore, Usage};

    let (db, ws) = setup().await;
    let (chat, prompt) = a_prompt(&db, ws).await;

    let refused = chat
        .claim_placeholder(prompt, prompt_session(&db, prompt).await, 1)
        .await
        .expect("refused");
    chat.set_message_content(
        refused.message.id,
        "I need approval to charge that.",
        None,
        None,
        Usage::default(),
        serde_json::json!({}),
    )
    .await
    .expect("finish");

    // Answered -- declined -- so the next attempt, leaving the refusal where it
    // is. This is the value `prepare_turn` now derives from the queue item
    // rather than from whether a grant exists.
    assert_eq!(
        chat.attempt_for(prompt, true).await.expect("declined"),
        2,
        "a declined turn must not overwrite the reply that was refused"
    );

    let next = chat
        .claim_placeholder(prompt, prompt_session(&db, prompt).await, 2)
        .await
        .expect("next");
    assert_ne!(next.message.id, refused.message.id);

    let kept: String = sqlx::query_scalar("select content from agent_messages where id = $1")
        .bind(refused.message.id)
        .fetch_one(&db.pool)
        .await
        .expect("still there");
    assert_eq!(
        kept, "I need approval to charge that.",
        "the refusal the person declined is the record of what they declined"
    );

    db.cleanup().await;
}
