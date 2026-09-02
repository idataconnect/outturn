//! Shared setup for the integration tests.

pub mod fake_gateway;

use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

/// Tests create and drop schemas, so a database that is not clearly a test
/// database must be refused: pointing TEST_DATABASE_URL at a dev database
/// would destroy its data.
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

/// The database these tests run against.
///
/// Panics rather than skipping when unset: the suite only builds under the
/// integration-tests feature, so reaching here without a database configured
/// is a misconfiguration, and a silent skip would report success having tested
/// nothing.
pub fn database_url() -> String {
    let url = std::env::var("TEST_DATABASE_URL").expect(
        "TEST_DATABASE_URL must be set to run integration tests, e.g. \
         postgres://outturn:outturn-dev@localhost:15432/outturn_test",
    );
    assert_test_database(&url);
    url
}

/// A private Postgres schema, dropped when the test finishes.
///
/// Tests get their own schema rather than sharing one and truncating between
/// runs. Truncation forces `--test-threads=1`, since one test wiping the
/// tables mid-run breaks every other; and it cannot isolate operations that
/// are global by nature -- `reap_abandoned` sweeps every expired job in the
/// database regardless of which test enqueued it.
pub struct TestDb {
    pub pool: PgPool,
    schema: String,
    admin: PgPool,
}

impl TestDb {
    pub async fn new() -> Self {
        let url = database_url();

        // Schema names cannot be parameterised, so the name is built rather
        // than bound. It is derived from a UUID and asserted to be alphanumeric
        // before use, so nothing external reaches the statement.
        let schema = format!("t_{}", Uuid::now_v7().simple());
        assert!(
            schema.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "generated schema name must be a plain identifier: {schema}"
        );

        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect");

        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create schema \"{schema}\"")))
            .execute(&admin)
            .await
            .expect("create schema");

        // Every connection in this pool resolves unqualified names to this
        // schema first, so the migrations and all queries land inside it.
        // public stays on the path for extensions such as pgvector, which are
        // installed once per database rather than per schema.
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .after_connect({
                let schema = schema.clone();
                move |conn, _| {
                    let schema = schema.clone();
                    Box::pin(async move {
                        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("set search_path to \"{schema}\", public")))
                            .execute(&mut *conn)
                            .await?;
                        Ok(())
                    })
                }
            })
            .connect(&url)
            .await
            .expect("connect");

        outturn::db::migrate(&pool).await.expect("migrate");

        Self {
            pool,
            schema,
            admin,
        }
    }

    /// Drops the schema. Called explicitly so a failing test leaves its data
    /// behind for inspection rather than tidying it away.
    pub async fn cleanup(self) {
        self.pool.close().await;
        let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!("drop schema \"{}\" cascade", self.schema)))
            .execute(&self.admin)
            .await;
        self.admin.close().await;
    }
}
