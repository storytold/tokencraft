use anyhow::{ensure, Result};
use sqlx::{migrate::Migrate, Connection, PgConnection, PgPool};

use super::MIGRATOR;

/// Initialize a new demo schema, or verify an existing schema without changing it.
/// Returns true only when this command installed the migrations.
pub async fn initialize_demo(pool: &PgPool) -> Result<bool> {
    // Use SQLx's session lock across both the inspection and migration. Detaching
    // guarantees the connection closes and releases the lock on every exit path.
    let mut conn = pool.acquire().await?.detach();
    sqlx::query("SET search_path TO public").execute(&mut conn).await?;
    conn.lock().await?;

    let has_ledger: bool = sqlx::query_scalar(
        "SELECT pg_catalog.to_regclass('public._sqlx_migrations') IS NOT NULL",
    )
    .fetch_one(&mut conn)
    .await?;
    if has_ledger {
        ensure!(
            migrations_current(&mut conn).await?,
            "existing demo schema is incompatible; use the deployment cutover procedure and run admin-cli migrate separately",
        );
        return Ok(false);
    }

    let public_is_empty: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (
             SELECT 1 FROM pg_catalog.pg_class WHERE relnamespace = 'public'::regnamespace
             UNION ALL
             SELECT 1 FROM pg_catalog.pg_proc WHERE pronamespace = 'public'::regnamespace
             UNION ALL
             SELECT 1 FROM pg_catalog.pg_type WHERE typnamespace = 'public'::regnamespace
         )",
    )
    .fetch_one(&mut conn)
    .await?;
    ensure!(public_is_empty, "demo initialization requires an empty public schema");
    MIGRATOR.run(&mut conn).await?;
    Ok(true)
}

/// Match the embedded migration prefix and explicitly approved forward migrations.
pub async fn migrations_current(conn: &mut PgConnection) -> sqlx::Result<bool> {
    // Read both the ledger and compatibility records in one snapshot.
    let mut snapshot = conn
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .await?;
    let allowed = schema_compatible(&mut snapshot).await?;
    snapshot.commit().await?;
    Ok(allowed)
}

async fn schema_compatible(conn: &mut PgConnection) -> sqlx::Result<bool> {
    let expected: Vec<_> = MIGRATOR
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .collect();
    let Some(last) = expected.last() else {
        return Ok(false);
    };
    let applied: Vec<(i64, Vec<u8>, bool)> = sqlx::query_as(
        "SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version LIMIT $1",
    )
    .bind(expected.len() as i64 + 1)
    .fetch_all(&mut *conn)
    .await?;
    let prefix_matches = applied.len() >= expected.len()
        && applied.iter().zip(&expected).all(|((version, checksum, success), migration)| {
            *success && *version == migration.version && checksum.as_slice() == migration.checksum.as_ref()
        });
    if !prefix_matches {
        return Ok(false);
    }
    if applied.len() == expected.len() {
        return Ok(true);
    }

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
             SELECT 1 FROM _sqlx_migrations applied
             LEFT JOIN public.gateway_schema_compatibility declared
                 ON declared.migration_version = applied.version
             WHERE applied.version > $1
               AND (NOT applied.success OR NOT coalesce(declared.oldest_gateway_schema BETWEEN 1 AND $1, false))
         )",
    )
    .bind(last.version)
    .fetch_one(conn)
    .await
}
