use chrono::{DateTime, Utc};
use ids::GatewayRequestId;
use sqlx::PgExecutor;

use super::RequestScope;

pub struct BaselineRow {
    pub model: String,
    pub provider: String,
    pub offer_id: String,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub request_ids: Vec<String>,
    pub total_ms: Option<i64>,
    pub queue_ms: Option<i64>,
    pub publication_ms: Option<i64>,
    pub total_median_ms: Option<i64>,
    pub queue_median_ms: Option<i64>,
    pub publication_median_ms: Option<i64>,
    pub total_samples: i64,
    pub queue_samples: i64,
    pub publication_samples: i64,
}

pub async fn trace_baseline<'e>(
    exec: impl PgExecutor<'e>,
    scope: RequestScope<'_>,
    id: &GatewayRequestId,
) -> sqlx::Result<Option<BaselineRow>> {
    sqlx::query_as!(
        BaselineRow,
        r#"WITH subject AS MATERIALIZED (
             SELECT r.sid, r.account_id, r.project_id, r.client_ref, r.app_ref, r.model, r.provider,
                    r.adapter, r.quote, r.created_at, r.dispatched_at, r.succeeded_at,
                    coalesce(r.succeeded_at, r.failed_at, r.canceled_at, r.uncertain_at) AS finished_at,
                    coalesce(p.route_retries->-1->'decision'->>'next_offer_id', r.offer_id) AS offer_id,
                    p.binding_digest, p.upstream_model, p.result_observed_at
             FROM gateway_requests r LEFT JOIN gateway_provider_runs p ON p.request_id = r.id
             WHERE r.id = $1 AND r.account_id = $2 AND r.project_id = $3
               AND ($4::text IS NULL OR r.client_ref = $4)
           ), matches AS MATERIALIZED (
             SELECT sample.*
             FROM subject s CROSS JOIN LATERAL (
               SELECT r.id, r.sid, r.created_at,
                      CASE WHEN r.succeeded_at >= r.created_at THEN r.succeeded_at - r.created_at END AS total,
                      CASE WHEN r.dispatched_at >= r.created_at THEN r.dispatched_at - r.created_at END AS queue,
                      CASE WHEN r.succeeded_at >= p.result_observed_at THEN r.succeeded_at - p.result_observed_at END AS publication
               FROM gateway_requests r
               LEFT JOIN gateway_provider_runs p ON p.request_id = r.id
               WHERE r.account_id = s.account_id AND r.project_id = s.project_id
                 AND coalesce(r.client_ref, '') = coalesce(s.client_ref, '')
                 AND coalesce(r.app_ref, '') = coalesce(s.app_ref, '')
                 AND r.model = s.model AND r.provider = s.provider AND r.adapter = s.adapter
                 AND r.sid < s.sid AND r.created_at >= s.created_at - interval '168 hours'
                 AND r.created_at < s.created_at AND r.succeeded_at <= s.created_at
                 AND r.quote->>'profile' IS NOT DISTINCT FROM s.quote->>'profile'
                 AND r.quote #> '{input,workload}' = s.quote #> '{input,workload}'
                 AND coalesce(p.route_retries->-1->'decision'->>'next_offer_id', r.offer_id) = s.offer_id
                 AND p.binding_digest IS NOT DISTINCT FROM s.binding_digest
                 AND p.upstream_model IS NOT DISTINCT FROM s.upstream_model
                 AND (
                   SELECT jsonb_build_array(c->'applied_aspect_ratio', c->'output_size')
                   FROM jsonb_array_elements(r.quote->'candidates') c WHERE c->>'offer_id' = s.offer_id LIMIT 1
                 ) IS NOT DISTINCT FROM (
                   SELECT jsonb_build_array(c->'applied_aspect_ratio', c->'output_size')
                   FROM jsonb_array_elements(s.quote->'candidates') c WHERE c->>'offer_id' = s.offer_id LIMIT 1
                 )
               ORDER BY r.created_at DESC, r.sid DESC LIMIT 50
             ) sample
           )
           SELECT s.model, s.provider, s.offer_id AS "offer_id!",
                  s.created_at - interval '168 hours' AS "window_start!", s.created_at AS "window_end!",
                  ARRAY(SELECT id FROM matches ORDER BY created_at DESC, sid DESC) AS "request_ids!",
                  CASE WHEN s.finished_at >= s.created_at
                    THEN round(extract(epoch FROM s.finished_at - s.created_at) * 1000)::bigint END AS total_ms,
                  CASE WHEN s.dispatched_at >= s.created_at
                    THEN round(extract(epoch FROM s.dispatched_at - s.created_at) * 1000)::bigint END AS queue_ms,
                  CASE WHEN s.succeeded_at >= s.result_observed_at
                    THEN round(extract(epoch FROM s.succeeded_at - s.result_observed_at) * 1000)::bigint END AS publication_ms,
                  stats.total_median_ms, stats.queue_median_ms, stats.publication_median_ms,
                  stats.total_samples AS "total_samples!", stats.queue_samples AS "queue_samples!",
                  stats.publication_samples AS "publication_samples!"
           FROM subject s CROSS JOIN (
             SELECT round(extract(epoch FROM percentile_cont(0.5) WITHIN GROUP (ORDER BY total)) * 1000)::bigint AS total_median_ms,
                    round(extract(epoch FROM percentile_cont(0.5) WITHIN GROUP (ORDER BY queue)) * 1000)::bigint AS queue_median_ms,
                    round(extract(epoch FROM percentile_cont(0.5) WITHIN GROUP (ORDER BY publication)) * 1000)::bigint AS publication_median_ms,
                    count(total) AS total_samples, count(queue) AS queue_samples, count(publication) AS publication_samples
             FROM matches
           ) stats"#,
        id.as_str(),
        scope.account.as_str(),
        scope.project.as_str(),
        scope.client_ref,
    )
    .fetch_optional(exec)
    .await
}
