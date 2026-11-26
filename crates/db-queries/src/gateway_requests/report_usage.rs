use chrono::{DateTime, Utc};
use sqlx::PgExecutor;

use super::RequestScope;

pub struct UsageFilter<'a> {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
    pub alternate_model: Option<&'a str>,
    pub app_ref: Option<&'a str>,
    pub group_by: &'a str,
    pub after: Option<&'a str>,
}

pub struct UsageRow {
    pub group_key: Option<String>,
    pub is_total: bool,
    pub requests: i64,
    pub succeeded: i64,
    pub canceled: i64,
    pub failed: i64,
    pub awaiting_reconciliation: i64,
    pub in_progress: i64,
    pub provider_requests: i64,
    pub completed_outputs: i64,
    pub estimated_usd_nanos: String,
    pub in_progress_estimated_usd_nanos: String,
    pub mock_reported_usd_nanos: String,
    pub provider_reported_usd_nanos: Option<String>,
    pub provider_reports_missing_cost: i64,
    pub provider_charges: sqlx::types::Json<Vec<UsageChargeTotal>>,
    pub supplier_settled_usd_nanos: Option<String>,
    pub supplier_settled_requests: i64,
    pub provider_input_tokens: Option<i64>,
    pub provider_output_tokens: Option<i64>,
    pub avg_success_latency_ms: Option<i64>,
}

#[derive(serde::Deserialize)]
pub struct UsageChargeTotal {
    pub provider: String,
    pub unit: String,
    pub quantity: String,
    pub observations: i64,
}

pub async fn report_usage<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    filter: UsageFilter<'_>,
    limit: i64,
) -> sqlx::Result<Vec<UsageRow>> {
    // Keep client equality visible to a generic plan on a reused connection.
    macro_rules! select_usage {
        ($client_filter:literal) => {
            sqlx::query_as!(
                UsageRow,
                r#"WITH activity AS (
            SELECT CASE $8::text
                   WHEN 'provider' THEN r.provider
                   WHEN 'model' THEN r.model
                   WHEN 'client' THEN r.client_ref
                   WHEN 'app' THEN r.app_ref
                   WHEN 'day' THEN to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD')
                 END AS group_key,
                 r.estimated_usd_nanos, r.succeeded_at, r.canceled_at, r.created_at, r.failed_at, r.uncertain_at,
                 r.provider, u.charge_unit, u.charge_quantity,
                 r.adapter <> 'mock' AS provider_request,
                 u.mock_reported, u.provider_reported, u.provider_reports_missing_cost, u.output_count,
                 u.input_tokens, u.output_tokens, settled.amount_usd_nanos AS supplier_settled
            FROM gateway_requests r
            LEFT JOIN LATERAL (
              SELECT sum(o.amount_usd_nanos) FILTER (WHERE o.source = 'mock_provider') AS mock_reported,
                     sum(o.amount_usd_nanos) FILTER (
                         WHERE o.source NOT IN ('mock_provider', 'connector_charge')) AS provider_reported,
                     count(*) FILTER (WHERE o.source NOT IN ('mock_provider', 'connector_charge')
                         AND o.amount_usd_nanos IS NULL) AS provider_reports_missing_cost,
                     sum(o.output_count) FILTER (WHERE o.source <> 'connector_charge') AS output_count,
                     sum(o.input_tokens) FILTER (WHERE o.source <> 'connector_charge') AS input_tokens,
                     sum(o.output_tokens) FILTER (WHERE o.source <> 'connector_charge') AS output_tokens,
                     max(o.charge_unit) FILTER (
                         WHERE o.attempt_number = 1 AND o.source = 'connector_charge') AS charge_unit,
                     max(o.charge_quantity) FILTER (
                         WHERE o.attempt_number = 1 AND o.source = 'connector_charge') AS charge_quantity
              FROM gateway_usage_observations o
              WHERE o.account_id = r.account_id AND o.request_id = r.id
            ) u ON true
            LEFT JOIN LATERAL (
              SELECT amount_usd_nanos FROM gateway_supplier_settlements
              WHERE request_id = r.id ORDER BY revision DESC LIMIT 1
            ) settled ON true
            WHERE r.account_id = $1 AND r.project_id = $2 AND r.created_at >= $3 AND r.created_at < $4
              AND "# + $client_filter + r#"
              AND ($6::text IS NULL OR r.provider = $6)
              AND ($7::text IS NULL OR r.model IN ($7, $12::text))
              AND ($11::text IS NULL OR r.app_ref = $11)
          ), totals AS (
            SELECT group_key, GROUPING(group_key) = 1 AS is_total,
              count(*) AS requests,
              count(succeeded_at) AS succeeded,
              count(canceled_at) AS canceled,
              count(failed_at) AS failed,
              count(uncertain_at) AS awaiting_reconciliation,
              count(*) FILTER (WHERE num_nonnulls(succeeded_at, canceled_at, failed_at, uncertain_at) = 0) AS in_progress,
              count(*) FILTER (WHERE provider_request) AS provider_requests,
              coalesce(sum(output_count), 0)::bigint AS completed_outputs,
              coalesce(sum(estimated_usd_nanos), 0)::text AS estimated_usd_nanos,
              coalesce(sum(estimated_usd_nanos)
                FILTER (WHERE num_nonnulls(succeeded_at, canceled_at, failed_at, uncertain_at) = 0), 0)::text AS in_progress_estimated_usd_nanos,
              coalesce(sum(mock_reported), 0)::text AS mock_reported_usd_nanos,
              sum(provider_reported)::text AS provider_reported_usd_nanos,
              sum(supplier_settled)::text AS supplier_settled_usd_nanos,
              count(supplier_settled) AS supplier_settled_requests,
              coalesce(sum(provider_reports_missing_cost), 0)::bigint AS provider_reports_missing_cost,
              sum(input_tokens)::bigint AS provider_input_tokens,
              sum(output_tokens)::bigint AS provider_output_tokens,
              round(avg(extract(epoch FROM succeeded_at - created_at) * 1000))::bigint AS avg_success_latency_ms
            FROM activity GROUP BY GROUPING SETS ((), (group_key))
          ), charges_by_unit AS (
            SELECT group_key, GROUPING(group_key) = 1 AS is_total, provider, charge_unit,
                   sum(charge_quantity)::text AS quantity, count(*) AS observations
            FROM activity WHERE charge_unit IS NOT NULL
            GROUP BY GROUPING SETS ((provider, charge_unit), (group_key, provider, charge_unit))
          ), charges AS (
            SELECT group_key, is_total,
                   jsonb_agg(jsonb_build_object('provider', provider, 'unit', charge_unit,
                     'quantity', quantity, 'observations', observations) ORDER BY provider, charge_unit) AS metrics
            FROM charges_by_unit GROUP BY group_key, is_total
          )
          SELECT totals.group_key AS "group_key?", totals.is_total AS "is_total!", requests AS "requests!",
            succeeded AS "succeeded!", canceled AS "canceled!", failed AS "failed!",
            awaiting_reconciliation AS "awaiting_reconciliation!", in_progress AS "in_progress!",
            provider_requests AS "provider_requests!",
            completed_outputs AS "completed_outputs!",
            estimated_usd_nanos AS "estimated_usd_nanos!",
            in_progress_estimated_usd_nanos AS "in_progress_estimated_usd_nanos!",
            mock_reported_usd_nanos AS "mock_reported_usd_nanos!",
            provider_reported_usd_nanos, provider_reports_missing_cost AS "provider_reports_missing_cost!",
            coalesce(charges.metrics, '[]'::jsonb) AS "provider_charges!: sqlx::types::Json<Vec<UsageChargeTotal>>",
            supplier_settled_usd_nanos, supplier_settled_requests AS "supplier_settled_requests!",
            provider_input_tokens, provider_output_tokens, avg_success_latency_ms
          FROM totals LEFT JOIN charges ON charges.is_total = totals.is_total
            AND charges.group_key IS NOT DISTINCT FROM totals.group_key
          WHERE totals.is_total OR $9::text IS NULL OR totals.group_key COLLATE "C" > $9
          ORDER BY totals.is_total DESC, totals.group_key COLLATE "C" NULLS FIRST LIMIT $10"#,
                scope.account.as_str(),
                scope.project.as_str(),
                filter.from,
                filter.to,
                scope.client_ref,
                filter.provider,
                filter.model,
                filter.group_by,
                filter.after,
                limit,
                filter.app_ref,
                filter.alternate_model,
            )
            .fetch_all(exec)
            .await
        };
    }
    if scope.client_ref.is_some() {
        select_usage!("r.client_ref = $5")
    } else {
        select_usage!("$5::text IS NULL")
    }
}
