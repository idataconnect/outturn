use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

/// Migrations are embedded at compile time, so the runtime image needs no
/// migration tooling and cannot drift from the binary.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("DATABASE_URL not set")]
    MissingUrl,
    #[error("connection failed: {0}")]
    Connect(#[source] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),
}

pub async fn connect_from_env() -> Result<PgPool, DbError> {
    let url = std::env::var("DATABASE_URL").map_err(|_| DbError::MissingUrl)?;
    connect_with_retry(&url, Duration::from_secs(60)).await
}

/// Connects, retrying until `budget` is exhausted.
///
/// On a cold start the database is often still accepting no connections when
/// this service begins; failing outright turns an ordinary startup ordering
/// into a crash loop.
pub async fn connect_with_retry(url: &str, budget: Duration) -> Result<PgPool, DbError> {
    let deadline = std::time::Instant::now() + budget;
    let mut backoff = Duration::from_millis(250);

    loop {
        match connect(url).await {
            Ok(pool) => return Ok(pool),
            Err(e) if std::time::Instant::now() < deadline => {
                tracing::warn!(error = %e, "database not ready, retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
            Err(e) => return Err(e),
        }
    }
}

pub async fn connect(url: &str) -> Result<PgPool, DbError> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .map_err(DbError::Connect)
}

/// The advisory lock replicas take turns on to migrate. Any constant will do;
/// it only has to be the same in every replica.
const MIGRATION_LOCK: i64 = 0x6f75_7474_7572_6e00; // "outturn\0"

/// How often a replica that found the lock held asks again.
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Applies pending migrations, one replica at a time; the others wake to find
/// nothing left to apply.
///
/// The lock is asked for, not waited on. sqlx's own blocks in
/// `pg_advisory_lock`, and a statement blocked there holds a snapshot -- which
/// `create index concurrently` waits out before it finishes. So two replicas
/// starting together deadlocked: the one migrating waited for the other's
/// snapshot, and the other waited for the lock. Postgres then aborted the
/// build and left an invalid index, and every start after failed on it until
/// someone dropped it by hand. A replica that only asks holds no snapshot
/// between asking.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    let failed = |e| DbError::Migrate(sqlx::migrate::MigrateError::Execute(e));
    // One connection throughout: an advisory lock belongs to the session that
    // took it.
    let mut conn = pool.acquire().await.map_err(failed)?;
    while !sqlx::query_scalar::<_, bool>("select pg_try_advisory_lock($1)")
        .bind(MIGRATION_LOCK)
        .fetch_one(&mut *conn)
        .await
        .map_err(failed)?
    {
        tokio::time::sleep(LOCK_POLL).await;
    }
    let unlocked = sqlx::migrate::Migrator {
        locking: false,
        migrations: MIGRATOR.migrations.clone(),
        ignore_missing: MIGRATOR.ignore_missing,
        no_tx: MIGRATOR.no_tx,
        table_name: MIGRATOR.table_name.clone(),
        create_schemas: MIGRATOR.create_schemas.clone(),
    };
    let applied = unlocked.run(&mut *conn).await.map_err(DbError::Migrate);
    // Released whatever happened; the session ending would release it too, but
    // a pooled connection outlives this call.
    let released = sqlx::query("select pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *conn)
        .await;
    applied?;
    released.map_err(failed)?;
    Ok(())
}
