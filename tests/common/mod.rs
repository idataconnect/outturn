//! Shared setup for the integration tests.

pub mod fake_gateway;

use sqlx::postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

/// The database a URL names.
fn database_name(url: &str) -> &str {
    url.rsplit('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
}

/// Tests create and drop databases on this server, so one that is not clearly
/// a test database must be refused: pointing TEST_DATABASE_URL at a dev
/// database would put its template and its copies beside real data, and the
/// template is dropped and rebuilt on every run.
pub fn assert_test_database(url: &str) {
    let name = database_name(url);
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

/// The base name for the template and the copies taken from it.
///
/// Derived from the configured database so two checkouts pointed at different
/// test databases do not fight over one template.
fn template_name(url: &str) -> String {
    format!("{}_template", database_name(url))
}

/// Migrates the template once per test binary.
///
/// The connection is closed before returning, because `create database ...
/// template` refuses while anything is connected to the template -- and every
/// test that follows is about to ask for a copy.
async fn template(url: &str) -> &'static str {
    use tokio::sync::OnceCell;
    static READY: OnceCell<String> = OnceCell::const_new();

    let name = READY
        .get_or_init(|| async {
            let name = template_name(url);
            let admin = PgPoolOptions::new()
                .max_connections(1)
                .connect(&server_url(url))
                .await
                .expect("connect to build the template");

            // The database the URL names, if nobody made it. Nothing here
            // opens it -- the template and its copies are what tests use --
            // but it is what the URL points at, and a fresh Postgres has only
            // the one `POSTGRES_DB` creates. Documenting a `createdb` step
            // instead would be a step that is written down in two places and
            // automated in none. Safe to attempt unconditionally: the name was
            // already asserted to be a test database, and the error from it
            // already existing is the expected case and is discarded.
            let named = database_name(url);
            let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "create database \"{named}\""
            )))
            .execute(&admin)
            .await;

            // Once per test binary, before anything is created. Here rather
            // than in `TestDb::new` because it is about other runs' leavings,
            // not this test's, and doing it per test would be the same scan
            // seventy times.
            sweep(&admin).await;

            // Rebuilt from nothing each run. A template left over from an
            // older migration set would hand every test a schema that does
            // not match the code, and the failure would look like a bug in
            // whatever ran first.
            let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "drop database if exists \"{name}\" with (force)"
            )))
            .execute(&admin)
            .await;
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
                .execute(&admin)
                .await
                .expect("create template database");
            admin.close().await;

            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&with_database(url, &name))
                .await
                .expect("connect to the template");
            outturn::db::migrate(&pool).await.expect("migrate the template");
            pool.close().await;

            name
        })
        .await;
    name.as_str()
}

/// The same URL pointed at another database on the same server.
fn with_database(url: &str, name: &str) -> String {
    match url.rsplit_once('/') {
        Some((head, tail)) => {
            let query = tail.split_once('?').map(|(_, q)| q);
            match query {
                Some(q) => format!("{head}/{name}?{q}"),
                None => format!("{head}/{name}"),
            }
        }
        None => url.to_string(),
    }
}

/// The server's own `postgres` database, for statements that cannot run inside
/// the database they are about.
fn server_url(url: &str) -> String {
    with_database(url, "postgres")
}

/// A private Postgres database, dropped when the test finishes.
///
/// Copied from a template that was migrated once, rather than migrated itself.
/// Replaying the migrations per test cost about a second each -- measured at
/// 1200ms against 48ms for a copy -- which for this suite was most of its
/// runtime and none of its value.
///
/// A database rather than a schema inside one, because `create database ...
/// template` is the only thing Postgres offers that copies a migrated state
/// wholesale. Tests are isolated either way; this is the same isolation
/// arriving twenty-five times faster.
///
/// Concurrent copies from one template serialise in Postgres, so a suite of
/// seventy queues rather than parallelising here. At forty-eight milliseconds
/// apiece that is a few seconds in total, against the eighty it replaced.
pub struct TestDb {
    pub pool: PgPool,
    name: String,
    admin: PgPool,
    /// Held for as long as this database is in use. Its only job is to die
    /// with the process: Postgres releases a session advisory lock when the
    /// connection goes, including when the run was killed rather than ended,
    /// so an unlocked test database is one nobody is using. See `sweep`.
    _claim: sqlx::pool::PoolConnection<sqlx::Postgres>,
}

/// Connections a test's pool may open.
///
/// Enough for a `PgListener` to hold one for as long as it runs and a long
/// poll another for the whole of its timeout, with room to spare.
const POOL: u32 = 5;

/// The advisory lock a test database is claimed with.
///
/// Derived from the name so any run can ask about any database without
/// needing to have created it. Advisory locks are a single 64-bit space
/// shared with anything else that uses them, so the key is salted with a
/// constant that makes a collision with application code implausible.
fn claim_key(name: &str) -> i64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "outturn_test_database".hash(&mut hasher);
    name.hash(&mut hasher);
    hasher.finish() as i64
}

/// Takes the lock that marks this database as in use.
///
/// The connection is returned and held by the caller for the test's lifetime.
/// A session advisory lock lives on its connection, so letting this go back to
/// the pool to be reused would release the claim while the test still ran.
async fn claim(admin: &PgPool, name: &str) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    let mut conn = admin.acquire().await.expect("connection to claim with");
    let taken: bool = sqlx::query_scalar("select pg_try_advisory_lock($1)")
        .bind(claim_key(name))
        .fetch_one(&mut *conn)
        .await
        .expect("claim the database");
    // The name comes from a fresh UUID, so a clash means the key derivation is
    // broken rather than that somebody got there first.
    assert!(taken, "could not claim test database {name}");
    conn
}

/// Drops test databases left behind by runs that did not finish.
///
/// A killed run -- a timeout, a panic, a SIGKILL -- never reaches `cleanup`,
/// and without this its databases stay on disk for ever. They are not free:
/// Postgres fsyncs every one of them on startup, and enough of them turn a
/// restart into a crash loop.
///
/// The test is the advisory lock rather than an age or a timestamp, so this
/// is safe to run while other suites are running in parallel. Taking the
/// lock proves the owner is gone, because a live owner still holds it;
/// failing to take it means somebody is using that database right now.
async fn sweep(admin: &PgPool) {
    let names: Vec<String> = match sqlx::query_scalar(
        "select datname from pg_database where datname like 't\\_%'",
    )
    .fetch_all(admin)
    .await
    {
        Ok(names) => names,
        // A sweep that cannot list is not a reason to fail the run.
        Err(e) => {
            eprintln!("could not list test databases to sweep: {e}");
            return;
        }
    };

    let mut dropped = 0;
    for name in names {
        let taken: bool = match sqlx::query_scalar("select pg_try_advisory_lock($1)")
            .bind(claim_key(&name))
            .fetch_one(admin)
            .await
        {
            Ok(taken) => taken,
            Err(_) => continue,
        };
        if !taken {
            // In use by a run happening right now.
            continue;
        }

        let dropped_ok = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop database if exists \"{name}\" with (force)"
        )))
        .execute(admin)
        .await
        .is_ok();
        if dropped_ok {
            dropped += 1;
        }

        let _ = sqlx::query("select pg_advisory_unlock($1)")
            .bind(claim_key(&name))
            .execute(admin)
            .await;
    }

    if dropped > 0 {
        eprintln!("swept {dropped} test database(s) left by earlier runs");
    }
}

impl TestDb {
    pub async fn new() -> Self {
        let url = database_url();
        let template = template(&url).await;

        // Names cannot be parameterised, so this is built rather than bound.
        // Derived from a UUID and asserted to be a plain identifier before
        // use, so nothing external reaches the statement.
        let name = format!("t_{}", Uuid::now_v7().simple());
        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "generated database name must be a plain identifier: {name}"
        );

        let admin = PgPoolOptions::new()
            // Two: one held for the whole test as the claim, one free for the
            // copy now and the drop at the end.
            .max_connections(2)
            .connect(&server_url(&url))
            .await
            .expect("connect");

        // Claimed before the database exists, not after. A lock is on a name,
        // and the name need not name anything yet -- which closes the window
        // where a parallel run's sweep would find a finished copy that nobody
        // had locked and drop it out from under this test.
        let claim = claim(&admin, &name).await;

        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "create database \"{name}\" template \"{template}\""
        )))
        .execute(&admin)
        .await
        .expect("copy the template");

        let pool = PgPoolOptions::new()
            // Five, of which a `PgListener` may hold one for as long as it
            // runs and a long poll another for the whole of its timeout.
            .max_connections(POOL)
            // These two together, for tests that pause tokio's clock.
            //
            // `tokio::time::pause()` advances the clock whenever the runtime
            // has nothing runnable, and waiting on a real socket is exactly
            // that -- so a timeout measured in virtual time can elapse during
            // work that takes milliseconds of real time. A connection opened
            // after the pause loses that race, and the acquire then fails
            // reporting a pool that is in fact sitting idle.
            //
            // So: open every connection up front, and set a ceiling the
            // virtual clock cannot race past. Measured over twenty runs of
            // `long_poll_returns_empty_on_timeout` in isolation, each alone
            // still fails -- without the timeout every run, without the
            // minimum about half -- and together none do.
            .min_connections(POOL)
            .acquire_timeout(std::time::Duration::from_secs(86_400))
            .connect(&with_database(&url, &name))
            .await
            .expect("connect");

        Self { pool, name, admin, _claim: claim }
    }

    /// Drops the database. Called explicitly so a failing test leaves its data
    /// behind for inspection rather than tidying it away.
    pub async fn cleanup(self) {
        self.pool.close().await;
        // Released explicitly. Returning the connection to the pool would not
        // do it -- a session lock outlives the borrow and would ride the
        // recycled connection into the next test.
        let mut claim = self._claim;
        let _ = sqlx::query("select pg_advisory_unlock($1)")
            .bind(claim_key(&self.name))
            .execute(&mut *claim)
            .await;
        drop(claim);
        // `with (force)` because a listener spawned by the code under test may
        // still hold a connection, and a drop that failed on that would leave
        // a database behind every run.
        let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop database if exists \"{}\" with (force)",
            self.name
        )))
        .execute(&self.admin)
        .await;
        self.admin.close().await;
    }
}
