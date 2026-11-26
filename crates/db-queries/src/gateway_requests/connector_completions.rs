use chrono::{DateTime, Utc};
use ids::GatewayRequestId;
use sqlx::{PgConnection, PgPool};

pub(super) async fn lock_request(conn: &mut PgConnection, request: &str) -> sqlx::Result<bool> {
    let row = sqlx::query_scalar::<_, String>(
        "SELECT id FROM gateway_requests WHERE id = $1 AND adapter = 'connector_queue' FOR UPDATE",
    )
    .bind(request)
    .fetch_optional(conn)
    .await?;
    Ok(row.is_some())
}

/// Accept only the first hint and keep an acknowledged poll's lease and deadline.
pub async fn receive(pool: &PgPool, request: &GatewayRequestId) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    if !lock_request(&mut tx, request.as_str()).await? {
        return Ok(());
    }
    sqlx::query(
        "WITH current_run AS (
             SELECT r.id, p.upstream_handle IS NULL AS needs_lookup,
                    r.dispatched_at, r.uncertain_at, p.poll_deadline,
                    coalesce(r.connector_submit_started_at, CASE
                        WHEN r.dispatched_at IS NULL AND p.route_retries <> '[]'::jsonb
                        THEN r.guarded_dispatch_at END) AS last_started_at
             FROM gateway_requests r JOIN gateway_provider_runs p ON p.request_id = r.id
             WHERE r.id = $1
               AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at, r.content_removed_at) = 0
         ), eligible AS (
             SELECT id, needs_lookup, CASE
                 WHEN needs_lookup AND uncertain_at IS NULL
                      AND (dispatched_at IS NULL OR last_started_at < dispatched_at)
                 THEN last_started_at END AS recover_dispatch
             FROM current_run
             WHERE poll_deadline > clock_timestamp() OR (
                 needs_lookup AND coalesce(dispatched_at, last_started_at) > clock_timestamp() - interval '15 minutes'
             )
         ), received AS (
             INSERT INTO gateway_connector_completions AS c (request_id, received_at, next_lookup_at)
             SELECT id, clock_timestamp(), CASE WHEN needs_lookup THEN clock_timestamp() END FROM eligible
             ON CONFLICT (request_id) DO UPDATE SET received_at = excluded.received_at,
                 next_lookup_at = coalesce(c.next_lookup_at, excluded.next_lookup_at)
             WHERE c.received_at IS NULL
             RETURNING request_id
         ), suspended AS (
             UPDATE gateway_requests r SET dispatched_at = eligible.recover_dispatch,
                 uncertain_at = clock_timestamp()
             FROM eligible JOIN received ON received.request_id = eligible.id
             WHERE r.id = eligible.id AND eligible.recover_dispatch IS NOT NULL
               AND eligible.recover_dispatch > clock_timestamp() - interval '15 minutes'
             RETURNING r.id
         )
         UPDATE gateway_provider_runs p SET
             poll_after = CASE WHEN p.poll_after IS NOT NULL AND p.result_observed_at IS NULL
                 THEN least(p.poll_after, clock_timestamp()) ELSE p.poll_after END,
             request_payload = CASE WHEN suspended.id IS NULL THEN p.request_payload END,
             lease_until = CASE WHEN suspended.id IS NULL THEN p.lease_until END,
             error_code = CASE WHEN suspended.id IS NOT NULL THEN 'connector_submission_unconfirmed' ELSE p.error_code END
         FROM received LEFT JOIN suspended ON suspended.id = received.request_id
         WHERE p.request_id = received.request_id AND (p.poll_after IS NOT NULL OR suspended.id IS NOT NULL)",
    )
    .bind(request.as_str())
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

pub(super) async fn received(conn: &mut PgConnection, request: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM gateway_connector_completions WHERE request_id = $1 AND received_at IS NOT NULL)",
    )
        .bind(request)
        .fetch_one(conn)
        .await
}

#[derive(sqlx::FromRow)]
pub struct Lookup {
    pub request_id: String,
    pub binding_digest: String,
    pub lease_until: DateTime<Utc>,
    pub trace_id: Option<String>,
    pub admission_span_id: Option<String>,
}

pub async fn claim_lookup(pool: &PgPool, bindings: &[String]) -> sqlx::Result<Option<Lookup>> {
    sqlx::query_as(
        "WITH eligible AS (
             SELECT r.id, p.binding_digest, r.trace_id, r.admission_span_id
             FROM gateway_requests r JOIN gateway_provider_runs p ON p.request_id = r.id
             LEFT JOIN gateway_connector_completions c ON c.request_id = r.id
             WHERE r.adapter = 'connector_queue'
               AND r.uncertain_at > statement_timestamp() - interval '15 minutes'
               AND (c.next_lookup_at IS NULL OR c.next_lookup_at <= statement_timestamp())
               AND p.binding_digest = ANY($1) AND p.upstream_handle IS NULL
               AND p.error_code IN ('connector_submission_unconfirmed', 'submission_interrupted')
               AND r.dispatched_at > statement_timestamp() - interval '15 minutes'
               AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at, r.content_removed_at) = 0
             ORDER BY coalesce(c.next_lookup_at, r.uncertain_at), r.id
             LIMIT 1 FOR UPDATE OF r SKIP LOCKED
         ), leased AS (
             INSERT INTO gateway_connector_completions AS c (request_id, received_at, next_lookup_at)
             SELECT id, NULL, clock_timestamp() + interval '10 seconds' FROM eligible
             ON CONFLICT (request_id) DO UPDATE SET next_lookup_at = excluded.next_lookup_at
             WHERE c.next_lookup_at IS NULL OR c.next_lookup_at <= statement_timestamp()
             RETURNING request_id, next_lookup_at
         )
         SELECT leased.request_id, eligible.binding_digest, leased.next_lookup_at AS lease_until,
                eligible.trace_id, eligible.admission_span_id
         FROM leased JOIN eligible ON eligible.id = leased.request_id",
    )
    .bind(bindings)
    .fetch_optional(pool)
    .await
}

/// The worker resolved the handle at the original connector, using the request ID.
pub async fn reconcile(pool: &PgPool, lookup: &Lookup, handle: &str) -> sqlx::Result<()> {
    let result = sqlx::query(
        "WITH eligible AS (
             SELECT r.id, r.uncertain_at FROM gateway_requests r
             JOIN gateway_provider_runs p ON p.request_id = r.id
             JOIN gateway_connector_completions c ON c.request_id = r.id
             WHERE r.id = $1 AND p.binding_digest = $2 AND p.upstream_handle IS NULL
               AND p.error_code IN ('connector_submission_unconfirmed', 'submission_interrupted')
               AND c.next_lookup_at = $3 AND c.next_lookup_at > clock_timestamp()
               AND r.uncertain_at IS NOT NULL AND r.dispatched_at > clock_timestamp() - interval '15 minutes'
               AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at, r.content_removed_at) = 0
               AND NOT EXISTS (SELECT 1 FROM gateway_provider_runs t WHERE t.binding_digest = $2
                   AND t.upstream_handle = $4 AND t.accepted_at IS NOT NULL)
             FOR UPDATE OF r
         ), acknowledged AS (
             UPDATE gateway_provider_runs p SET upstream_handle = $4, accepted_at = clock_timestamp(),
                 request_payload = NULL, lease_until = NULL, error_code = NULL, queue_state = 'in_queue',
                 poll_after = clock_timestamp(), poll_deadline = clock_timestamp() + interval '15 minutes'
             FROM eligible WHERE p.request_id = eligible.id RETURNING p.request_id
         ), recovered AS (
             UPDATE gateway_requests r SET uncertain_at = NULL FROM acknowledged
             WHERE r.id = acknowledged.request_id RETURNING r.id
         )
         UPDATE gateway_connector_completions c SET next_lookup_at = NULL,
             recovered_from = eligible.uncertain_at, reconciled_at = clock_timestamp()
         FROM recovered JOIN eligible ON eligible.id = recovered.id WHERE c.request_id = recovered.id",
    )
    .bind(&lookup.request_id)
    .bind(&lookup.binding_digest)
    .bind(lookup.lease_until)
    .bind(handle)
    .execute(pool)
    .await;
    match result {
        // A different worker may have claimed the handle after the NOT EXISTS check.
        Err(error) if super::queued_runs::is_handle_conflict(&error) => Ok(()),
        result => result.map(|_| ()),
    }
}
