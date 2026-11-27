use sqlx::PgExecutor;

use super::RequestScope;

pub struct RouteOutcome {
    pub offer_id: String,
    pub samples: i64,
    pub succeeded: i64,
    pub unconfirmed: i64,
    pub median_ms: Option<i64>,
    pub settled_samples: i64,
    pub settled_usd_nanos: Option<String>,
}

pub async fn recent_outcomes<'e>(
    exec: impl PgExecutor<'e>,
    scope: RequestScope<'_>,
    workload: &serde_json::Value,
    offers: &serde_json::Value,
    window_days: i32,
    sample_limit: i64,
    include_cost: bool,
) -> sqlx::Result<Vec<RouteOutcome>> {
    sqlx::query_as!(
        RouteOutcome,
        r#"WITH bounds AS (
             SELECT CASE WHEN $8::boolean THEN date_trunc('day', statement_timestamp(), 'UTC')
                         ELSE statement_timestamp() END AS until
           )
           SELECT offer.id AS "offer_id!", stats.samples AS "samples!",
                  stats.succeeded AS "succeeded!", stats.unconfirmed AS "unconfirmed!", stats.median_ms,
                  stats.settled_samples AS "settled_samples!", stats.settled_usd_nanos
           FROM jsonb_to_recordset($5::jsonb) AS offer (
               id text, binding_digest text, upstream_model text,
               applied_aspect_ratio text, output_size jsonb, estimated_usd_nanos bigint
           ) CROSS JOIN bounds w
           CROSS JOIN LATERAL (
             SELECT count(*) AS samples, count(sample.succeeded_at) AS succeeded,
                    count(sample.uncertain_at) AS unconfirmed,
                    round(percentile_cont(0.5) WITHIN GROUP (
                        ORDER BY greatest(0, extract(epoch FROM sample.succeeded_at - sample.accepted_at) * 1000)
                    ) FILTER (WHERE sample.succeeded_at IS NOT NULL))::bigint AS median_ms,
                    count(settled.amount_usd_nanos) AS settled_samples,
                    sum(settled.amount_usd_nanos)::text AS settled_usd_nanos
             FROM (
               SELECT r.id, r.succeeded_at, r.uncertain_at, p.accepted_at
               FROM gateway_requests r JOIN gateway_provider_runs p ON p.request_id = r.id
               JOIN LATERAL (
                   SELECT candidate
                   FROM jsonb_array_elements(
                       CASE WHEN jsonb_typeof(r.quote->'candidates') = 'array'
                            THEN r.quote->'candidates' ELSE '[]'::jsonb END
                   ) AS candidate
                   WHERE candidate->>'offer_id' = offer.id
                   LIMIT 1
               ) saved ON true
               WHERE r.account_id = $1 AND r.project_id = $2
                 AND coalesce(r.client_ref, '') = coalesce($3::text, '')
                 AND r.created_at >= w.until - $6::integer * interval '24 hours'
                 AND r.created_at < w.until
                 AND r.adapter IN ('fal_queue', 'kinovi_queue', 'connector_queue')
                 AND (r.succeeded_at IS NOT NULL OR r.uncertain_at IS NOT NULL OR r.failed_at IS NOT NULL)
                 AND coalesce(r.succeeded_at, r.uncertain_at, r.failed_at) < w.until
                 AND (r.failed_at IS NULL OR p.error_code = 'provider_generation_failed')
                 AND jsonb_hash_extended(r.quote #> '{input,workload}', 0) = jsonb_hash_extended($4::jsonb, 0)
                 AND r.quote #> '{input,workload}' = $4::jsonb
                 AND p.accepted_at IS NOT NULL
                 AND coalesce(p.route_retries->-1->'decision'->>'next_offer_id', r.offer_id) = offer.id
                 AND p.binding_digest = offer.binding_digest AND p.upstream_model = offer.upstream_model
                 AND (NOT $8::boolean OR (r.offer_id = offer.id
                         AND r.estimated_usd_nanos = offer.estimated_usd_nanos))
                 AND saved.candidate->>'applied_aspect_ratio' IS NOT DISTINCT FROM offer.applied_aspect_ratio
                 AND nullif(saved.candidate->'output_size', 'null'::jsonb)
                     IS NOT DISTINCT FROM offer.output_size
               ORDER BY r.created_at DESC, r.sid DESC LIMIT $7
             ) sample
             LEFT JOIN LATERAL (
               SELECT s.amount_usd_nanos FROM gateway_supplier_settlements s
               WHERE $8::boolean AND s.request_id = sample.id
               ORDER BY s.revision DESC LIMIT 1
             ) settled ON true
           ) stats"#,
        scope.account.as_str(),
        scope.project.as_str(),
        scope.client_ref,
        workload,
        offers,
        window_days,
        sample_limit,
        include_cost,
    )
    .fetch_all(exec)
    .await
}
