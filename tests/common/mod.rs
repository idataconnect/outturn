//! Shared setup for the integration tests.
//!
//! Each test binary compiles this whole module and uses a slice of it, so
//! what one binary leaves unused is not dead.
#![allow(dead_code)]

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

/// A name for this run's template, its own.
///
/// One name per test database used to be shared by every run against it, and
/// each run drops and rebuilds its template first -- so two runs at once, from
/// two checkouts or two agents, dropped the template out from under each other
/// and failed whichever test was copying from it. Named like a test database
/// (`t_`) and claimed the same way, so `sweep` removes one a killed run left
/// behind and never one a live run holds.
fn template_name() -> String {
    format!("t_template_{}", uuid::Uuid::now_v7().simple())
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
            let name = template_name();
            let admin = Admin::get(url);

            // The database the URL names, if nobody made it. Nothing here
            // opens it -- the template and its copies are what tests use --
            // but it is what the URL points at, and a fresh Postgres has only
            // the one `POSTGRES_DB` creates. Documenting a `createdb` step
            // instead would be a step that is written down in two places and
            // automated in none. Safe to attempt unconditionally: the name was
            // already asserted to be a test database, and the error from it
            // already existing is the expected case and is discarded.
            let named = database_name(url);
            let _ = admin.run(format!("create database \"{named}\"")).await;

            // Once per test binary, before anything is created. Here rather
            // than in `TestDb::new` because it is about other runs' leavings,
            // not this test's, and doing it per test would be the same scan
            // seventy times.
            sweep(admin).await;

            // Claimed before it exists, on the admin connection, which lives
            // as long as the process -- so no other run's sweep can drop it
            // while copies are still being taken from it, and the lock goes
            // when this process does, however it ends. Fresh each run, so a
            // template from an older migration set is never handed out.
            assert!(
                admin.claim(&name).await,
                "could not claim the template {name}"
            );
            admin
                .run(format!("create database \"{name}\""))
                .await
                .expect("create template database");

            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&with_database(url, &name))
                .await
                .expect("connect to the template");
            outturn::db::migrate(&pool)
                .await
                .expect("migrate the template");
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
/// Concurrent copies from one template serialize in Postgres, so a suite of
/// seventy queues rather than parallelising here. At forty-eight milliseconds
/// apiece that is a few seconds in total, against the eighty it replaced.
pub struct TestDb {
    pub pool: PgPool,
    name: String,
    /// Holds this database's claim for as long as it is in use: the lock dies
    /// with the process, including when the run was killed rather than
    /// ended, so an unlocked test database is one nobody is using. See
    /// `sweep`.
    admin: &'static Admin,
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

/// What the one admin connection is asked to do.
enum AdminOp {
    /// Takes the lock that marks a database as in use; answers whether it was
    /// free.
    Claim(i64),
    Release(i64),
    /// A statement that cannot run inside the database it is about: a
    /// create, or a drop.
    Run(String),
    /// The names of every test database on the server.
    List,
}

enum AdminAnswer {
    Taken(bool),
    Done(Result<(), String>),
    Names(Vec<String>),
}

/// One connection to the server's `postgres` database, shared by every test
/// in this process, for claims, copies and drops.
///
/// One rather than one per test, because each test used to hold a connection
/// for its whole life just to keep its claim, and another for its copy and
/// drop: a third of a run's connections, doing nothing. A session holds any
/// number of advisory locks, and creates and drops take milliseconds and
/// serialize in Postgres anyway, so one connection taking turns loses
/// nothing. Its locks still die with the process, which is what `sweep`
/// relies on.
///
/// On its own thread with its own runtime, because every test runs on a
/// runtime that ends with it, and a connection opened on one stops answering
/// once that runtime has gone.
struct Admin {
    ops: tokio::sync::mpsc::UnboundedSender<(AdminOp, tokio::sync::oneshot::Sender<AdminAnswer>)>,
}

impl Admin {
    fn get(url: &str) -> &'static Admin {
        static ADMIN: std::sync::OnceLock<Admin> = std::sync::OnceLock::new();
        ADMIN.get_or_init(|| {
            let server = server_url(url);
            let (ops, mut queue) = tokio::sync::mpsc::unbounded_channel::<(
                AdminOp,
                tokio::sync::oneshot::Sender<AdminAnswer>,
            )>();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("admin runtime");
                runtime.block_on(async move {
                    use sqlx::Connection;
                    let mut conn = sqlx::PgConnection::connect(&server)
                        .await
                        .expect("connect the admin connection");
                    while let Some((op, answer)) = queue.recv().await {
                        let said = match op {
                            AdminOp::Claim(key) => AdminAnswer::Taken(
                                sqlx::query_scalar("select pg_try_advisory_lock($1)")
                                    .bind(key)
                                    .fetch_one(&mut conn)
                                    .await
                                    .unwrap_or(false),
                            ),
                            AdminOp::Release(key) => AdminAnswer::Done(
                                sqlx::query("select pg_advisory_unlock($1)")
                                    .bind(key)
                                    .execute(&mut conn)
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.to_string()),
                            ),
                            AdminOp::Run(sql) => AdminAnswer::Done(
                                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                                    .execute(&mut conn)
                                    .await
                                    .map(|_| ())
                                    .map_err(|e| e.to_string()),
                            ),
                            AdminOp::List => AdminAnswer::Names(
                                sqlx::query_scalar(
                                    "select datname from pg_database where datname like 't\\_%'",
                                )
                                .fetch_all(&mut conn)
                                .await
                                .unwrap_or_default(),
                            ),
                        };
                        let _ = answer.send(said);
                    }
                });
            });
            Admin { ops }
        })
    }

    async fn ask(&self, op: AdminOp) -> AdminAnswer {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.ops.send((op, tx)).expect("admin connection gone");
        rx.await.expect("admin connection gone")
    }

    /// Takes the lock that marks `name` as in use. False if somebody holds it.
    async fn claim(&self, name: &str) -> bool {
        match self.ask(AdminOp::Claim(claim_key(name))).await {
            AdminAnswer::Taken(taken) => taken,
            _ => false,
        }
    }

    async fn release(&self, name: &str) {
        let _ = self.ask(AdminOp::Release(claim_key(name))).await;
    }

    async fn run(&self, sql: String) -> Result<(), String> {
        match self.ask(AdminOp::Run(sql)).await {
            AdminAnswer::Done(done) => done,
            _ => Err("unexpected answer".into()),
        }
    }

    async fn test_databases(&self) -> Vec<String> {
        match self.ask(AdminOp::List).await {
            AdminAnswer::Names(names) => names,
            _ => Vec::new(),
        }
    }
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
async fn sweep(admin: &Admin) {
    let mut dropped = 0;
    for name in admin.test_databases().await {
        if !admin.claim(&name).await {
            // In use by a run happening right now.
            continue;
        }
        if admin
            .run(format!("drop database if exists \"{name}\" with (force)"))
            .await
            .is_ok()
        {
            dropped += 1;
        }
        admin.release(&name).await;
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

        let admin = Admin::get(&url);

        // Claimed before the database exists, not after. A lock is on a name,
        // and the name need not name anything yet -- which closes the window
        // where a parallel run's sweep would find a finished copy that nobody
        // had locked and drop it out from under this test. The name comes from
        // a fresh UUID, so a clash means the key derivation is broken rather
        // than that somebody got there first.
        assert!(
            admin.claim(&name).await,
            "could not claim test database {name}"
        );

        admin
            .run(format!(
                "create database \"{name}\" template \"{template}\""
            ))
            .await
            .expect("copy the template");

        let pool = PgPoolOptions::new()
            // Five, of which a `PgListener` may hold one for as long as it
            // runs and a long poll another for the whole of its timeout.
            // Opened as a test needs them, and closed again after a few idle
            // seconds: each test has a database of its own, so a connection
            // one test leaves idle is one no other test can use, and a run's
            // tests together would otherwise hold the server's every slot.
            .max_connections(POOL)
            .idle_timeout(std::time::Duration::from_secs(5))
            .connect(&with_database(&url, &name))
            .await
            .expect("connect");

        Self { pool, name, admin }
    }

    /// Drops the database. Called explicitly so a failing test leaves its data
    /// behind for inspection rather than tidying it away.
    pub async fn cleanup(self) {
        self.pool.close().await;
        // `with (force)` because a listener spawned by the code under test may
        // still hold a connection, and a drop that failed on that would leave
        // a database behind every run.
        let _ = self
            .admin
            .run(format!(
                "drop database if exists \"{}\" with (force)",
                self.name
            ))
            .await;
        // After the drop, so no sweep can find it unclaimed while it exists.
        self.admin.release(&self.name).await;
    }
}
