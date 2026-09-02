use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

/// Migrations are embedded at compile time, so the runtime image needs no
/// migration tooling and cannot drift from the binary.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

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

/// Applies pending migrations. sqlx takes a Postgres advisory lock for the
/// duration, so concurrent replicas serialize rather than race; losers wake to
/// find nothing left to apply.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    MIGRATOR.run(pool).await.map_err(DbError::Migrate)
}
