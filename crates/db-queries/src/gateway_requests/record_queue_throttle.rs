use sqlx::{PgConnection, PgPool};

use super::{
    connector_completions, provider_runs::finish_provider_run_in, queue_cooldowns, ClaimedProviderRun,
    RunOutcome,
};

/// Commit a cooldown with a retry or a failure on the original submission lease.
pub async fn record_queue_throttle(
    pool: &PgPool,
    run: &ClaimedProviderRun,
    cooldown_ms: i32,
    retry_after_ms: Option<i32>,
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    if !connector_completions::lock_request(&mut tx, &run.request_id).await? {
        return Ok(());
    }

    // A callback for this request can contradict the pre-acceptance response.
    let outcome = if connector_completions::received(&mut tx, &run.request_id).await? {
        RunOutcome::Uncertain("connector_submission_unconfirmed")
    } else {
        if !queue_cooldowns::record(&mut tx, run, cooldown_ms).await? {
            return Ok(());
        }
        let rescheduled = match retry_after_ms {
            Some(delay_ms) => reschedule(&mut tx, run, delay_ms).await?,
            None => false,
        };
        if rescheduled {
            return tx.commit().await;
        }
        RunOutcome::Rejected("provider_rate_limited")
    };
    if !finish_provider_run_in(
        &mut tx,
        &run.request_id,
        run.lease,
        outcome,
        Default::default(),
    )
    .await?
    {
        return Ok(());
    }
    tx.commit().await
}

async fn reschedule(
    conn: &mut PgConnection,
    run: &ClaimedProviderRun,
    delay_ms: i32,
) -> sqlx::Result<bool> {
    let changed = sqlx::query!(
        "WITH ready AS (
             UPDATE gateway_requests r SET dispatched_at = NULL,
                 dispatch_after = statement_timestamp() + $3::integer * interval '1 millisecond'
             FROM gateway_provider_runs p
             WHERE r.id = $1 AND r.adapter = 'connector_queue'
               AND p.request_id = r.id AND p.lease_until = $2 AND p.lease_until > clock_timestamp()
               AND r.dispatched_at IS NOT NULL AND p.request_payload IS NOT NULL
               AND p.accepted_at IS NULL AND p.upstream_handle IS NULL
               AND num_nonnulls(r.failed_at, r.succeeded_at, r.canceled_at, r.uncertain_at) = 0
               AND p.route_retries = '[]'::jsonb
               AND statement_timestamp() + $3::integer * interval '1 millisecond'
                   < r.created_at + interval '15 minutes'
             RETURNING r.id, r.offer_id, r.estimated_usd_nanos
         )
         UPDATE gateway_provider_runs p SET lease_until = NULL,
             route_retries = p.route_retries || jsonb_build_object(
                 'at', statement_timestamp(),
                 'decision', jsonb_build_object(
                     'offer_id', ready.offer_id,
                     'next_offer_id', ready.offer_id,
                     'estimated_usd_nanos', ready.estimated_usd_nanos,
                     'delay_ms', $3::integer
                 )
             )
         FROM ready WHERE p.request_id = ready.id",
        run.request_id,
        run.lease,
        delay_ms,
    )
    .execute(conn)
    .await?
    .rows_affected();
    Ok(changed == 1)
}
