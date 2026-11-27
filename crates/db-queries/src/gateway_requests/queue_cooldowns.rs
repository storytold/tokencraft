use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgExecutor, PgPool};

use super::{ClaimedProviderRun, RequestScope};

pub struct QueueCooldown {
    pub binding_digest: String,
    pub upstream_model: String,
    pub available_after: DateTime<Utc>,
}

pub async fn active<'e>(
    exec: impl PgExecutor<'e>,
    scope: RequestScope<'_>,
) -> sqlx::Result<Vec<QueueCooldown>> {
    sqlx::query_as!(
        QueueCooldown,
        "SELECT binding_digest, upstream_model, available_after
         FROM gateway_queue_cooldowns WHERE account_id = $1 AND project_id = $2
           AND available_after > statement_timestamp()",
        scope.account.as_str(), scope.project.as_str(),
    )
    .fetch_all(exec)
    .await
}

/// Extend a cooldown from an explicit pre-acceptance rejection on the current lease.
pub(super) async fn record(
    conn: &mut PgConnection,
    run: &ClaimedProviderRun,
    wait_ms: i32,
) -> sqlx::Result<bool> {
    let changed = sqlx::query!(
        "INSERT INTO gateway_queue_cooldowns (account_id, project_id, binding_digest, upstream_model, available_after)
         SELECT r.account_id, r.project_id, p.binding_digest, p.upstream_model,
                statement_timestamp() + $3::integer * interval '1 millisecond'
         FROM gateway_requests r JOIN gateway_provider_runs p ON p.request_id = r.id
         WHERE r.id = $1 AND r.dispatched_at IS NOT NULL AND p.request_payload IS NOT NULL
           AND p.lease_until = $2 AND p.lease_until > clock_timestamp()
           AND p.accepted_at IS NULL AND p.upstream_handle IS NULL
           AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at, r.uncertain_at) = 0
         ON CONFLICT (account_id, project_id, binding_digest, upstream_model) DO UPDATE
         SET available_after = greatest(gateway_queue_cooldowns.available_after, excluded.available_after)",
        run.request_id, run.lease, wait_ms,
    )
    .execute(conn)
    .await?;
    Ok(changed.rows_affected() == 1)
}

pub async fn purge_expired(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query!(
        "WITH expired AS (
            SELECT account_id, project_id, binding_digest, upstream_model
            FROM gateway_queue_cooldowns
            WHERE available_after <= statement_timestamp()
            ORDER BY available_after LIMIT 128 FOR UPDATE SKIP LOCKED
         )
         DELETE FROM gateway_queue_cooldowns USING expired
         WHERE gateway_queue_cooldowns.account_id = expired.account_id
           AND gateway_queue_cooldowns.project_id = expired.project_id
           AND gateway_queue_cooldowns.binding_digest = expired.binding_digest
           AND gateway_queue_cooldowns.upstream_model = expired.upstream_model",
    )
    .execute(pool)
    .await?;
    Ok(())
}
