use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::queue_turns::QueueLane;

pub struct ClaimedQueuePoll {
    pub request_id: String,
    pub trace_id: Option<String>,
    pub admission_span_id: Option<String>,
    pub adapter: String,
    pub binding_digest: String,
    pub upstream_model: String,
    pub handle: String,
    pub expected_outputs: i16,
    pub duration_seconds: Option<i16>,
    pub image_width: Option<i32>,
    pub image_height: Option<i32>,
    pub workload: Option<serde_json::Value>,
    pub applied_aspect_ratio: Option<String>,
    pub lease: DateTime<Utc>,
    pub credential_version: Option<i32>,
    pub result_observed: bool,
    pub charge_recorded: bool,
}

pub struct PollBindings<'a> {
    pub connectors: &'a [String],
}

pub async fn claim_queue_poll(
    pool: &PgPool,
    bindings: PollBindings<'_>,
) -> sqlx::Result<Option<ClaimedQueuePoll>> {
    let lane = QueueLane::ProviderPoll;
    let Some(mut tx) = lane.begin(pool).await? else {
        return Ok(None);
    };
    let poll = sqlx::query_as!(
        ClaimedQueuePoll,
        r#"WITH ready AS (
             SELECT r.id, r.account_id, r.project_id, r.adapter, r.provider_credential_version,
                    r.trace_id, r.admission_span_id,
                    coalesce((r.quote #>> '{input,workload,count}')::smallint, 1::smallint) AS output_count,
                    (r.quote #>> '{input,workload,duration_seconds}')::smallint AS duration_seconds,
                    (r.quote #>> '{candidates,0,output_size,width}')::integer AS image_width,
                    (r.quote #>> '{candidates,0,output_size,height}')::integer AS image_height,
                    r.quote #> '{input,workload}' AS workload,
                    r.quote #>> '{candidates,0,applied_aspect_ratio}' AS applied_aspect_ratio
             FROM gateway_provider_runs p JOIN gateway_requests r ON r.id = p.request_id
             LEFT JOIN gateway_queue_turns q ON q.project_id = r.project_id AND q.lane = $2
             WHERE r.adapter = 'connector_queue' AND p.binding_digest = ANY($1::text[])
               AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at) = 0
               AND (r.uncertain_at IS NULL OR p.recheck_requested_at IS NOT NULL)
               AND p.poll_after <= clock_timestamp()
               AND p.poll_deadline > clock_timestamp()
               AND (p.lease_until IS NULL OR p.lease_until <= clock_timestamp())
             ORDER BY coalesce(q.turn, 0), p.poll_after, r.sid
             LIMIT 1 FOR UPDATE OF r SKIP LOCKED
           ), served AS (
             INSERT INTO gateway_queue_turns (project_id, lane)
             SELECT project_id, $2 FROM ready
             ON CONFLICT (project_id, lane) DO UPDATE SET turn = excluded.turn
           )
           UPDATE gateway_provider_runs p SET lease_until = clock_timestamp() + interval '20 seconds'
           FROM ready WHERE p.request_id = ready.id
           RETURNING p.request_id, ready.adapter AS "adapter!", p.binding_digest, p.upstream_model, p.upstream_handle AS "handle!",
                     ready.output_count AS "expected_outputs!", ready.duration_seconds, ready.image_width,
                     ready.image_height, p.lease_until AS "lease!",
                     ready.workload AS "workload?", ready.applied_aspect_ratio AS "applied_aspect_ratio?",
                     ready.trace_id AS "trace_id?", ready.admission_span_id AS "admission_span_id?",
                     ready.provider_credential_version AS "credential_version?",
                     (p.result_observed_at IS NOT NULL) AS "result_observed!",
                     (p.result_observed_at IS NOT NULL AND EXISTS (
                         SELECT 1 FROM gateway_usage_observations u
                         WHERE u.account_id = ready.account_id AND u.request_id = p.request_id
                           AND u.attempt_number = 1 AND u.source = 'connector_charge'
                     )) AS "charge_recorded!""#,
        bindings.connectors,
        lane.key(),
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(poll)
}

/// The accepted-handle index also covers the generic queue connector.
pub fn is_handle_conflict(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(|error| {
        error.is_unique_violation()
            && error.constraint() == Some("gateway_fal_accepted_handle_idx")
    })
}

pub async fn acknowledge_queue(
    pool: &PgPool,
    id: &str,
    lease: DateTime<Utc>,
    handle: &str,
    adapter: &str,
) -> sqlx::Result<()> {
    sqlx::query!(
        "WITH locked AS (
             SELECT id FROM gateway_requests WHERE id = $1 AND adapter = $4
               AND num_nonnulls(succeeded_at, failed_at, uncertain_at, canceled_at) = 0
             FOR UPDATE
         ), acknowledged AS (
             UPDATE gateway_provider_runs p SET upstream_handle = $3, accepted_at = clock_timestamp(),
                 request_payload = NULL, lease_until = NULL,
                 poll_after = clock_timestamp(), poll_deadline = clock_timestamp() + interval '15 minutes',
                 queue_state = 'in_queue'
             FROM locked WHERE p.request_id = locked.id AND p.lease_until = $2
               AND p.lease_until > clock_timestamp()
             RETURNING p.request_id
         )
         UPDATE gateway_connector_completions c SET next_lookup_at = NULL
         FROM acknowledged WHERE c.request_id = acknowledged.request_id AND c.next_lookup_at IS NOT NULL",
        id,
        lease,
        handle,
        adapter,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCharge {
    pub unit: String,
    pub quantity: String,
}

pub async fn record_result_observed(
    pool: &PgPool,
    task: &ClaimedQueuePoll,
    charge: Option<&ProviderCharge>,
) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"WITH observed AS (
             UPDATE gateway_provider_runs SET
                 result_observed_at = coalesce(result_observed_at, clock_timestamp()), queue_state = 'completed'
             WHERE request_id = $1 AND lease_until = $2 AND upstream_handle = $3
               AND lease_until > clock_timestamp() AND poll_deadline > clock_timestamp()
             RETURNING request_id
         ), charged AS (
             INSERT INTO gateway_usage_observations
               (account_id, request_id, attempt_number, source, output_count, reported_at, charge_unit, charge_quantity)
             SELECT r.account_id, observed.request_id, 1, 'connector_charge', 0, clock_timestamp(), $4, $5::text::numeric
             FROM observed JOIN gateway_requests r ON r.id = observed.request_id
             WHERE $4::text IS NOT NULL
             ON CONFLICT (account_id, request_id, attempt_number, source) DO NOTHING
         )
         SELECT EXISTS (SELECT 1 FROM observed) AS "recorded!""#,
        task.request_id,
        task.lease,
        task.handle,
        charge.map(|charge| charge.unit.as_str()),
        charge.map(|charge| charge.quantity.as_str()),
    )
    .fetch_one(pool)
    .await
}

pub async fn renew_publication(
    pool: &PgPool,
    request: &str,
    lease: DateTime<Utc>,
) -> sqlx::Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar!(
        r#"WITH locked AS (
             SELECT id FROM gateway_requests WHERE id = $1 AND content_removed_at IS NULL
               AND num_nonnulls(succeeded_at, failed_at, canceled_at) = 0
             FOR UPDATE
         )
         UPDATE gateway_provider_runs p SET
             lease_until = least(clock_timestamp() + interval '20 seconds', p.poll_deadline)
         FROM locked WHERE p.request_id = locked.id AND p.lease_until = $2
           AND p.lease_until > clock_timestamp() AND p.poll_deadline > clock_timestamp()
           AND p.result_observed_at IS NOT NULL
         RETURNING p.lease_until AS "lease!""#,
        request,
        lease,
    )
    .fetch_optional(pool)
    .await
}

pub async fn defer_queue_poll(
    pool: &PgPool,
    task: &ClaimedQueuePoll,
    state: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "WITH locked AS (
             SELECT id, uncertain_at FROM gateway_requests WHERE id = $1 AND adapter = $4
               AND num_nonnulls(succeeded_at, failed_at, canceled_at) = 0
             FOR UPDATE
         )
         UPDATE gateway_provider_runs p SET lease_until = NULL,
             poll_after = CASE
                 WHEN p.result_observed_at IS NOT NULL THEN least(p.poll_deadline,
                     clock_timestamp() + make_interval(secs =>
                         least(30, 2 << p.publication_retry_step) * (0.9 + 0.1 * random())))
                 WHEN $3::text IS NULL THEN clock_timestamp() + interval '2 seconds'
                 ELSE clock_timestamp() + interval '1 second'
             END,
             publication_retry_step = CASE WHEN p.result_observed_at IS NOT NULL
                 THEN least(4, p.publication_retry_step + 1)
                 ELSE p.publication_retry_step END,
             queue_state = CASE WHEN p.result_observed_at IS NOT NULL THEN 'completed'
                                ELSE coalesce($3, p.queue_state) END
         FROM locked WHERE p.request_id = locked.id AND p.lease_until = $2
           AND (locked.uncertain_at IS NULL OR p.recheck_requested_at IS NOT NULL)
           AND p.lease_until > clock_timestamp() AND p.poll_deadline > clock_timestamp()",
        task.request_id,
        task.lease,
        state,
        task.adapter,
    )
    .execute(pool)
    .await?;
    Ok(())
}
