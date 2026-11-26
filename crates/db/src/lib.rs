//! Data layer: the connection pool and the embedded migrator. The ONLY SQL in the
//! codebase lives in `queries/` (not yet built out); migrations live in `migrations/`
//! and are applied through administrative commands. The gateway only reads the migration ledger.

use anyhow::Result;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};

pub use sqlx::PgPool;

mod schema;
pub use schema::{initialize_demo, migrations_current};

/// Migrations embedded at compile time from `crates/db/migrations`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Require TCP and hostname-verified PostgreSQL TLS for hosted processes.
pub fn verify_hosted_url(database_url: &str) -> Result<()> {
    let options: PgConnectOptions = database_url
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid hosted database connection settings"))?;
    anyhow::ensure!(
        options.get_socket().is_none() && matches!(options.get_ssl_mode(), PgSslMode::VerifyFull),
        "the hosted database requires TCP and DATABASE_URL with sslmode=verify-full"
    );
    Ok(())
}

/// Open a connection pool.
pub async fn connect(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// Apply all pending migrations (used by `admin-cli migrate`).
pub async fn migrate(pool: &PgPool) -> Result<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// Liveness check — `SELECT 1`.
pub async fn ping(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
        .is_ok()
}

/// Identify the cluster before an operator tool applies migrations to a local listener.
pub async fn data_directory(pool: &PgPool) -> sqlx::Result<String> {
    sqlx::query_scalar("SHOW data_directory").fetch_one(pool).await
}

/// Count application tables (excludes sqlx bookkeeping) — handy for `admin-cli db-info`.
pub async fn table_count(pool: &PgPool) -> Result<i64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema='public' AND table_type='BASE TABLE' AND table_name <> '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await?;
    Ok(n)
}

pub mod test_fixtures;
