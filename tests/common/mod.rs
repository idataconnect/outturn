//! Shared test setup.

use sqlx::postgres::PgPool;

/// Tests truncate every table, so a database that is not clearly a test
/// database must be refused — pointing TEST_DATABASE_URL at a dev database
/// would silently destroy its data.
pub fn assert_test_database(url: &str) {
    let name = url
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    assert!(
        name.contains("test"),
        "refusing to run destructive tests against database {name:?}: \
         TEST_DATABASE_URL must name a database containing 'test' \
         (e.g. .../outturn_test)"
    );
}

/// Wipes every table these tests touch.
pub async fn reset(pool: &PgPool) {
    sqlx::query(
        "truncate users, tenants, user_system_roles, user_tenant_roles, events, jobs, \
         refresh_tokens, agents, agent_sessions, agent_messages cascade",
    )
    .execute(pool)
    .await
    .expect("truncate");
}
