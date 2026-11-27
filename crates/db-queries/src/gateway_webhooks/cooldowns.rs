use sqlx::{PgPool, Postgres, Transaction};

/// Record a receiver delay in the same transaction that records the current attempt.
pub(super) async fn record(
    tx: &mut Transaction<'_, Postgres>,
    url: &str,
    wait_ms: i32,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO gateway_webhook_cooldowns (receiver_digest, available_after)
         VALUES (sha256(convert_to($1, 'UTF8')), clock_timestamp() + $2::integer * interval '1 millisecond')
         ON CONFLICT (receiver_digest) DO UPDATE
         SET available_after = greatest(gateway_webhook_cooldowns.available_after, excluded.available_after)",
        url, wait_ms,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn purge_expired(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query!(
        "WITH expired AS (
            SELECT receiver_digest FROM gateway_webhook_cooldowns
            WHERE available_after <= statement_timestamp()
            ORDER BY available_after LIMIT 128 FOR UPDATE SKIP LOCKED
         )
         DELETE FROM gateway_webhook_cooldowns c USING expired
         WHERE c.receiver_digest = expired.receiver_digest",
    )
    .execute(pool)
    .await?;
    Ok(())
}
