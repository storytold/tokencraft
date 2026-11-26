use anyhow::{ensure, Result};
use sqlx::{migrate::{Migrate, Migration}, Connection, PgConnection, PgPool};

use super::MIGRATOR;

/// Initialize a new demo schema, or verify an existing schema without changing it.
/// Returns true only when this command installed the migrations.
pub async fn initialize_demo(pool: &PgPool) -> Result<bool> {
    // Use SQLx's session lock across both the inspection and migration. Detaching
    // guarantees the connection closes and releases the lock on every exit path.
    let mut conn = pool.acquire().await?.detach();
    sqlx::query("SET search_path TO public").execute(&mut conn).await?;
    conn.lock().await?;

    if has_migration_ledger(&mut conn).await? {
        ensure!(
            migrations_current(&mut conn).await?,
            "existing demo schema is incompatible; use the deployment cutover procedure and run admin-cli migrate separately",
        );
        return Ok(false);
    }

    ensure!(
        public_is_empty(&mut conn).await?,
        "demo initialization requires an empty public schema"
    );
    MIGRATOR.run(&mut conn).await?;
    Ok(true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaState {
    Empty,
    Unmanaged,
    UpgradeRequired,
    Current,
    ForwardCompatible,
    Diverged,
    IncompatibleNewer,
}

impl SchemaState {
    pub fn is_compatible(self) -> bool {
        matches!(self, Self::Current | Self::ForwardCompatible)
    }
}

#[derive(Debug)]
pub struct SchemaStatus {
    pub state: SchemaState,
    pub binary_version: i64,
    pub database_version: Option<i64>,
    pub pending_versions: Vec<i64>,
}

/// Inspect the public schema without applying migrations or acquiring the migration lock.
pub async fn schema_status(pool: &PgPool) -> sqlx::Result<SchemaStatus> {
    let mut conn = pool.acquire().await?;
    let mut snapshot = conn
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?;
    let expected = embedded_migrations();
    let (state, database_version) = if has_migration_ledger(&mut snapshot).await? {
        let state = inspect_ledger(&mut snapshot, &expected).await?;
        let version = sqlx::query_scalar("SELECT max(version) FROM public._sqlx_migrations")
            .fetch_one(&mut *snapshot)
            .await?;
        (state, version)
    } else if public_is_empty(&mut snapshot).await? {
        (SchemaState::Empty, None)
    } else {
        (SchemaState::Unmanaged, None)
    };
    snapshot.commit().await?;

    let pending_versions = match state {
        SchemaState::Empty | SchemaState::UpgradeRequired => expected
            .iter()
            .filter(|migration| database_version.is_none_or(|version| migration.version > version))
            .map(|migration| migration.version)
            .collect(),
        _ => Vec::new(),
    };
    Ok(SchemaStatus {
        state,
        binary_version: expected.last().map_or(0, |migration| migration.version),
        database_version,
        pending_versions,
    })
}

async fn has_migration_ledger(conn: &mut PgConnection) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT pg_catalog.to_regclass('public._sqlx_migrations') IS NOT NULL")
        .fetch_one(conn)
        .await
}

async fn public_is_empty(conn: &mut PgConnection) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT NOT EXISTS (
             SELECT 1 FROM pg_catalog.pg_class WHERE relnamespace = 'public'::regnamespace
             UNION ALL
             SELECT 1 FROM pg_catalog.pg_proc WHERE pronamespace = 'public'::regnamespace
             UNION ALL
             SELECT 1 FROM pg_catalog.pg_type WHERE typnamespace = 'public'::regnamespace
         )",
    )
    .fetch_one(conn)
    .await
}

fn embedded_migrations() -> Vec<&'static Migration> {
    MIGRATOR
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .collect()
}

/// Match the embedded migration prefix and explicitly approved forward migrations.
pub async fn migrations_current(conn: &mut PgConnection) -> sqlx::Result<bool> {
    // Read both the ledger and compatibility records in one snapshot.
    let mut snapshot = conn
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?;
    let state = inspect_ledger(&mut snapshot, &embedded_migrations()).await?;
    snapshot.commit().await?;
    Ok(state.is_compatible())
}

async fn inspect_ledger(
    conn: &mut PgConnection,
    expected: &[&Migration],
) -> sqlx::Result<SchemaState> {
    let Some(last) = expected.last() else {
        return Ok(SchemaState::Diverged);
    };
    let applied: Vec<(i64, Vec<u8>, bool)> = sqlx::query_as(
        "SELECT version, checksum, success FROM public._sqlx_migrations ORDER BY version LIMIT $1",
    )
    .bind(expected.len() as i64 + 1)
    .fetch_all(&mut *conn)
    .await?;
    if applied.iter().zip(expected).any(|((version, checksum, success), migration)| {
        !success || *version != migration.version || checksum.as_slice() != migration.checksum.as_ref()
    }) {
        return Ok(SchemaState::Diverged);
    }
    if applied.len() < expected.len() {
        return Ok(SchemaState::UpgradeRequired);
    }
    if applied.len() == expected.len() {
        return Ok(SchemaState::Current);
    }

    if forward_compatible(conn, last.version).await? {
        Ok(SchemaState::ForwardCompatible)
    } else {
        Ok(SchemaState::IncompatibleNewer)
    }
}

async fn forward_compatible(conn: &mut PgConnection, version: i64) -> sqlx::Result<bool> {
    let declared: bool = sqlx::query_scalar(
        "SELECT pg_catalog.to_regclass('public.gateway_schema_compatibility') IS NOT NULL",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !declared {
        return Ok(false);
    }
    sqlx::query_scalar(
        "SELECT NOT EXISTS (
             SELECT 1 FROM public._sqlx_migrations applied
             LEFT JOIN public.gateway_schema_compatibility declared
                 ON declared.migration_version = applied.version
             WHERE applied.version > $1
               AND (NOT applied.success OR NOT coalesce(declared.oldest_gateway_schema BETWEEN 1 AND $1, false))
         )",
    )
    .bind(version)
    .fetch_one(conn)
    .await
}
