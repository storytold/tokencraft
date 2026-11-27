use sqlx::PgPool;

use super::cooldowns;

pub(super) const MAX_ATTEMPTS: i32 = 6;

pub struct Claim {
    pub id: String,
    pub subject_id: String,
    pub is_workflow: bool,
    pub trace_id: Option<String>,
    pub admission_span_id: Option<String>,
    pub subscription_id: String,
    pub destination_id: String,
    pub destination_url: String,
    pub payload: String,
    pub signing_version: i32,
    pub sign_with_previous: bool,
    pub attempt: i32,
    pub tries_in_cycle: i32,
}

pub enum ClaimOutcome {
    Claimed(Claim),
    Closed,
    Idle,
}

pub async fn claim(pool: &PgPool, excluded_destinations: &[String]) -> sqlx::Result<ClaimOutcome> {
    let mut tx = pool.begin().await?;
    // Use a stable cutoff so the due index can skip future retries. Holding the
    // subscription lock through commit orders new leases with revocation and rotation.
    let ready = sqlx::query!(
        "SELECT d.id, d.state, d.payload, d.attempt_count, d.attempt_baseline, s.id AS subscription_id,
           s.destination_id, s.destination_url, s.signing_version,
           coalesce(d.request_id, d.workflow_run_id) AS \"subject_id!\",
           (d.workflow_run_id IS NOT NULL) AS \"is_workflow!\", r.trace_id, r.admission_span_id,
           (s.signing_rotated_at > clock_timestamp() - interval '24 hours') AS sign_with_previous,
           (d.deadline_at <= clock_timestamp()) AS \"expired!\", s.revoked_at
         FROM gateway_webhook_deliveries d
           LEFT JOIN gateway_requests r ON r.id = d.request_id
           LEFT JOIN workflow_runs w ON w.id = d.workflow_run_id
           JOIN gateway_webhook_subscriptions s ON s.id = coalesce(r.webhook_subscription_id, w.webhook_subscription_id)
         WHERE d.state IN ('pending', 'sending') AND d.available_at <= statement_timestamp()
           AND s.destination_id <> ALL($1::text[])
           AND (s.revoked_at IS NOT NULL OR d.deadline_at <= statement_timestamp()
                OR d.attempt_count - d.attempt_baseline >= $2
                OR NOT EXISTS (
                    SELECT 1 FROM gateway_webhook_cooldowns c
                    WHERE c.receiver_digest = sha256(convert_to(s.destination_url, 'UTF8'))
                      AND c.available_after > statement_timestamp()
                ))
         ORDER BY d.available_at, d.id LIMIT 1
         FOR UPDATE OF d SKIP LOCKED FOR SHARE OF s SKIP LOCKED",
        excluded_destinations, MAX_ATTEMPTS,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(ready) = ready else {
        tx.commit().await?;
        return Ok(ClaimOutcome::Idle);
    };

    if ready.state == "sending" {
        sqlx::query!(
            "UPDATE gateway_webhook_attempts SET finished_at = clock_timestamp(), outcome = 'lease_expired'
             WHERE delivery_id = $1 AND number = $2 AND finished_at IS NULL",
            ready.id, ready.attempt_count,
        )
        .execute(&mut *tx)
        .await?;
    }

    let tries = ready.attempt_count - ready.attempt_baseline;
    let stop_reason = if ready.revoked_at.is_some() {
        Some("subscription_revoked")
    } else if ready.expired {
        Some("retry_window_expired")
    } else if tries >= MAX_ATTEMPTS {
        Some("lease_expired")
    } else {
        None
    };
    if let Some(reason) = stop_reason {
        sqlx::query!(
            "UPDATE gateway_webhook_deliveries SET state = 'dead', finished_at = clock_timestamp(), last_error = $2
             WHERE id = $1",
            ready.id, reason,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(ClaimOutcome::Closed);
    }

    let attempt = ready.attempt_count + 1;
    sqlx::query!(
        "UPDATE gateway_webhook_deliveries SET state = 'sending', attempt_count = $2,
           available_at = clock_timestamp() + interval '30 seconds'
         WHERE id = $1",
        ready.id,
        attempt,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO gateway_webhook_attempts (delivery_id, number) VALUES ($1, $2)",
        ready.id,
        attempt,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(ClaimOutcome::Claimed(Claim {
        id: ready.id,
        subject_id: ready.subject_id,
        is_workflow: ready.is_workflow,
        trace_id: ready.trace_id,
        admission_span_id: ready.admission_span_id,
        subscription_id: ready.subscription_id,
        destination_id: ready.destination_id,
        destination_url: ready.destination_url,
        payload: ready.payload,
        signing_version: ready.signing_version,
        sign_with_previous: ready.sign_with_previous.unwrap_or(false),
        attempt,
        tries_in_cycle: tries + 1,
    }))
}

pub enum DeliveryResult {
    Delivered(i16),
    HttpError {
        status: i16,
        retry_after_ms: Option<i32>,
        receiver_cooldown_ms: Option<i32>,
    },
    NetworkError {
        retry_after_ms: i32,
    },
    DestinationUnavailable,
}

pub async fn finish(pool: &PgPool, claim: &Claim, result: DeliveryResult) -> sqlx::Result<bool> {
    let (outcome, status, delay, cooldown) = match result {
        DeliveryResult::Delivered(status) => ("delivered", Some(status), None, None),
        DeliveryResult::HttpError {
            status,
            retry_after_ms,
            receiver_cooldown_ms,
        } => ("http_error", Some(status), retry_after_ms, receiver_cooldown_ms),
        DeliveryResult::NetworkError { retry_after_ms } => {
            ("network_error", None, Some(retry_after_ms), None)
        }
        DeliveryResult::DestinationUnavailable => ("destination_unavailable", None, None, None),
    };
    let error = (outcome != "delivered").then_some(outcome);
    let retry_delay = delay.filter(|_| claim.tries_in_cycle < MAX_ATTEMPTS);
    let mut tx = pool.begin().await?;
    let result = sqlx::query!(
        "UPDATE gateway_webhook_deliveries
         SET state = CASE WHEN $3::text IS NULL THEN 'delivered'
                          WHEN $4::int IS NOT NULL THEN 'pending' ELSE 'dead' END,
             available_at = CASE WHEN $4 IS NOT NULL THEN
                 least(deadline_at, clock_timestamp() + $4 * interval '1 millisecond') ELSE available_at END,
             finished_at = CASE WHEN $4 IS NULL THEN clock_timestamp() ELSE NULL END,
             last_error = $3
         WHERE id = $1 AND attempt_count = $2 AND state = 'sending' AND available_at > clock_timestamp()",
        claim.id, claim.attempt, error, retry_delay,
    )
    .execute(&mut *tx)
    .await?;
    let recorded = result.rows_affected() == 1;
    if recorded {
        sqlx::query!(
            "UPDATE gateway_webhook_attempts
             SET finished_at = clock_timestamp(), outcome = $3, http_status = $4
             WHERE delivery_id = $1 AND number = $2",
            claim.id,
            claim.attempt,
            outcome,
            status,
        )
        .execute(&mut *tx)
        .await?;
        if let Some(wait_ms) = cooldown {
            cooldowns::record(&mut tx, &claim.destination_url, wait_ms).await?;
        }
    }
    tx.commit().await?;
    Ok(recorded)
}
