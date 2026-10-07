//! What the skill figures say, against rows built to say it.
//!
//! The interesting two cuts -- idle and lagging -- come back empty on any
//! healthy workspace, which is exactly why they need constructing rather than
//! eyeballing: an empty result and a broken query look identical.

#![cfg(feature = "integration-tests")]

mod common;

use outturn::api::actor::Actor;
use outturn::api::skill::PostgresSkillStatsStore;
use outturn::api::skill::stats::SkillStatsStore;
use uuid::Uuid;

struct World {
    db: common::TestDb,
    workspace: Uuid,
    agent: Uuid,
    session: Uuid,
}

async fn setup() -> World {
    let db = common::TestDb::new().await;
    let workspace = Uuid::now_v7();
    sqlx::query("insert into workspaces (id, name, slug) values ($1, $2, $3)")
        .bind(workspace)
        .bind(format!("T{workspace}"))
        .bind(format!("t-{}", workspace.simple()))
        .execute(&db.pool)
        .await
        .expect("workspace");

    let agent = Uuid::now_v7();
    sqlx::query(
        "insert into agents (id, workspace_id, name, slug, system_prompt, enabled) \
         values ($1, $2, 'A', 'a-' || replace($1::text, '-', ''), '', true)",
    )
    .bind(agent)
    .bind(workspace)
    .execute(&db.pool)
    .await
    .expect("agent");

    let session = Uuid::now_v7();
    sqlx::query("insert into agent_sessions (id, workspace_id, agent_id) values ($1, $2, $3)")
        .bind(session)
        .bind(workspace)
        .bind(agent)
        .execute(&db.pool)
        .await
        .expect("session");

    World {
        db,
        workspace,
        agent,
        session,
    }
}

/// A skill with one version, returning both ids.
async fn a_skill(w: &World, name: &str) -> (Uuid, Uuid) {
    let skill = Uuid::now_v7();
    sqlx::query(
        "insert into skills (id, workspace_id, slug, name) \
         values ($1, $2, $3, $4)",
    )
    .bind(skill)
    .bind(w.workspace)
    .bind(format!("{name}-{}", skill.simple()))
    .bind(name)
    .execute(&w.db.pool)
    .await
    .expect("skill");

    let version = a_version(w, skill, 1).await;
    (skill, version)
}

async fn a_version(w: &World, skill: Uuid, ordinal: i32) -> Uuid {
    let version = Uuid::now_v7();
    sqlx::query(
        "insert into skill_versions (id, workspace_id, skill_id, ordinal, body) \
         values ($1, $2, $3, $4, 'do the thing')",
    )
    .bind(version)
    .bind(w.workspace)
    .bind(skill)
    .bind(ordinal)
    .execute(&w.db.pool)
    .await
    .expect("version");
    version
}

async fn bind_to_agent(w: &World, skill: Uuid, pinned: Option<Uuid>) {
    sqlx::query(
        "insert into agent_skills (workspace_id, agent_id, skill_id, version_id) \
         values ($1, $2, $3, $4)",
    )
    .bind(w.workspace)
    .bind(w.agent)
    .bind(skill)
    .bind(pinned)
    .execute(&w.db.pool)
    .await
    .expect("bind");
}

/// A turn that this skill served.
async fn a_turn_using(w: &World, skill: Uuid, version: Uuid) {
    let reply = Uuid::now_v7();
    sqlx::query(
        "insert into agent_messages (id, session_id, role, content) \
         values ($1, $2, 'assistant', 'done')",
    )
    .bind(reply)
    .bind(w.session)
    .execute(&w.db.pool)
    .await
    .expect("reply");

    sqlx::query(
        "insert into turn_skills (reply_id, skill_id, version_id, position) \
         values ($1, $2, $3, 0)",
    )
    .bind(reply)
    .bind(skill)
    .bind(version)
    .execute(&w.db.pool)
    .await
    .expect("turn_skills");
}

fn window() -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    let to = chrono::Utc::now() + chrono::Duration::days(1);
    (to - chrono::Duration::days(30), to)
}

#[tokio::test]
async fn a_skill_reports_the_turns_and_the_conversations_it_served() {
    let w = setup().await;
    let (skill, version) = a_skill(&w, "Booking").await;
    bind_to_agent(&w, skill, None).await;
    for _ in 0..3 {
        a_turn_using(&w, skill, version).await;
    }

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert_eq!(stats.totals.turns, 3);
    assert_eq!(stats.totals.skills, 1);
    assert_eq!(stats.totals.bound, 1);
    assert_eq!(stats.used.len(), 1);
    assert_eq!(stats.used[0].turns, 3);
    // Three turns in one conversation, which is a different fact from three
    // conversations each using it once.
    assert_eq!(stats.used[0].sessions, 1);
    assert!(stats.used[0].last_used.is_some());
    assert!(stats.idle.is_empty(), "a skill in use is not idle");

    w.db.cleanup().await;
}

/// The cut that finds dead weight: carried in every prompt, doing nothing.
#[tokio::test]
async fn a_skill_an_agent_carries_and_never_uses_is_idle() {
    let w = setup().await;
    let (used, used_v) = a_skill(&w, "Booking").await;
    let (idle, _) = a_skill(&w, "Refunds").await;
    bind_to_agent(&w, used, None).await;
    bind_to_agent(&w, idle, None).await;
    a_turn_using(&w, used, used_v).await;

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert_eq!(stats.idle.len(), 1, "only the unused one is idle");
    assert_eq!(stats.idle[0].skill_id, idle);
    assert_eq!(stats.idle[0].agents, 1);
    // Never once, which is a stronger statement than "not lately" and is why
    // this figure reaches outside the window.
    assert!(stats.idle[0].last_used.is_none());

    w.db.cleanup().await;
}

/// A skill nobody carries is not idle -- it is unused, which is a different
/// thing and not something to act on. Idle means "paid for and doing nothing".
#[tokio::test]
async fn a_skill_no_agent_carries_is_not_reported_as_idle() {
    let w = setup().await;
    a_skill(&w, "Shelved").await;

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert!(stats.idle.is_empty());
    assert_eq!(stats.totals.skills, 1, "it still exists");
    assert_eq!(stats.totals.bound, 0, "nothing carries it");

    w.db.cleanup().await;
}

/// The "I fixed the skill and it still does the old thing" case.
#[tokio::test]
async fn a_turn_served_by_an_older_version_is_lagging() {
    let w = setup().await;
    let (skill, first) = a_skill(&w, "Charging").await;
    // Edited since, and the turns below were served by the old one.
    a_version(&w, skill, 2).await;
    bind_to_agent(&w, skill, None).await;
    a_turn_using(&w, skill, first).await;
    a_turn_using(&w, skill, first).await;

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert_eq!(stats.lagging.len(), 1);
    assert_eq!(stats.lagging[0].latest, 2);
    assert_eq!(stats.lagging[0].serving, 1);
    assert_eq!(stats.lagging[0].turns, 2);
    assert!(
        !stats.lagging[0].pinned,
        "nothing pinned it, so this is an edit nobody picked up"
    );

    w.db.cleanup().await;
}

/// A pin is what tells a deliberate lag from a forgotten one.
#[tokio::test]
async fn a_pinned_skill_says_its_lag_was_chosen() {
    let w = setup().await;
    let (skill, first) = a_skill(&w, "Charging").await;
    a_version(&w, skill, 2).await;
    bind_to_agent(&w, skill, Some(first)).await;
    a_turn_using(&w, skill, first).await;

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert_eq!(stats.lagging.len(), 1);
    assert!(stats.lagging[0].pinned);

    w.db.cleanup().await;
}

/// A skill serving its newest version is not lagging, which is the ordinary
/// case and the one that must not produce noise.
#[tokio::test]
async fn a_skill_on_its_newest_version_is_not_lagging() {
    let w = setup().await;
    let (skill, first) = a_skill(&w, "Booking").await;
    bind_to_agent(&w, skill, None).await;
    a_turn_using(&w, skill, first).await;

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert!(stats.lagging.is_empty());

    w.db.cleanup().await;
}

/// Another workspace's skills are not this one's figures.
#[tokio::test]
async fn the_figures_stop_at_the_workspace() {
    let w = setup().await;
    let (mine, version) = a_skill(&w, "Mine").await;
    bind_to_agent(&w, mine, None).await;
    a_turn_using(&w, mine, version).await;

    let other = setup().await;
    let (theirs, theirs_v) = a_skill(&other, "Theirs").await;
    bind_to_agent(&other, theirs, None).await;
    for _ in 0..5 {
        a_turn_using(&other, theirs, theirs_v).await;
    }

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert_eq!(stats.totals.skills, 1);
    assert_eq!(stats.totals.turns, 1, "not the other workspace's five");
    assert_eq!(stats.used.len(), 1);
    assert_eq!(stats.used[0].name, "Mine");

    w.db.cleanup().await;
    other.db.cleanup().await;
}

/// The operator's staff are counted together as the operator, neither named
/// nor told apart, while the workspace's own people are named.
#[tokio::test]
async fn the_operators_staff_are_counted_as_the_operator() {
    let w = setup().await;
    let (skill, _) = a_skill(&w, "Shared").await;

    let user = |name: &'static str, staff: bool| {
        let pool = w.db.pool.clone();
        async move {
            let id = Uuid::now_v7();
            sqlx::query("insert into users (id, display_name) values ($1, $2)")
                .bind(id)
                .bind(name)
                .execute(&pool)
                .await
                .expect("user");
            if staff {
                sqlx::query(
                    "insert into user_system_roles (user_id, role) values ($1, 'system_admin')",
                )
                .bind(id)
                .execute(&pool)
                .await
                .expect("role");
            }
            id
        }
    };
    let ana = user("Ana", false).await;
    let staff = [user("Staff One", true).await, user("Staff Two", true).await];

    for (ordinal, author) in [(2, staff[0]), (3, staff[1]), (4, ana)] {
        let version = a_version(&w, skill, ordinal).await;
        sqlx::query("update skill_versions set created_by = $1 where id = $2")
            .bind(author)
            .bind(version)
            .execute(&w.db.pool)
            .await
            .expect("author");
    }

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    let operator: Vec<_> = stats
        .authors
        .iter()
        .filter(|a| a.author == Actor::Operator)
        .collect();
    assert_eq!(
        operator.len(),
        1,
        "staff were told apart: {:?}",
        stats.authors
    );
    assert_eq!(operator[0].versions, 2);
    assert!(operator[0].user_id.is_none(), "a staff id was sent");

    let ours = stats
        .authors
        .iter()
        .find(|a| a.user_id == Some(ana))
        .expect("Ana's row");
    assert_eq!(ours.author, Actor::Person { name: "Ana".into() });
    assert_eq!(
        stats.authors.iter().map(|a| a.versions).sum::<i64>(),
        stats.totals.versions,
        "the cut must add up to the total"
    );

    w.db.cleanup().await;
}

/// Versions written by an install carry no author, and dropping those rows
/// would make the author counts disagree with the total.
#[tokio::test]
async fn versions_with_no_author_still_count() {
    let w = setup().await;
    a_skill(&w, "Seeded").await;

    let (from, to) = window();
    let stats = PostgresSkillStatsStore::new(w.db.pool.clone())
        .stats(w.workspace, from, to, 20)
        .await
        .expect("stats");

    assert_eq!(stats.totals.versions, 1);
    assert_eq!(stats.authors.len(), 1);
    assert!(stats.authors[0].user_id.is_none());
    assert_eq!(
        stats.authors.iter().map(|a| a.versions).sum::<i64>(),
        stats.totals.versions,
        "the cut must add up to the total"
    );

    w.db.cleanup().await;
}
