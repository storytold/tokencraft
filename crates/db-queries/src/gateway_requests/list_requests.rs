use chrono::{DateTime, Utc};
use ids::GatewayRequestId;
use sqlx::{PgExecutor, Postgres, QueryBuilder};

use super::{CostState, RequestDiagnostic, RequestRow, RequestScope, RequestStatus};

#[derive(Default, PartialEq)]
pub struct RequestFilter<'a> {
    pub after: Option<&'a GatewayRequestId>,
    pub app_ref: Option<&'a str>,
    pub client_unattributed: bool,
    pub app_unattributed: bool,
    pub trace_id: Option<&'a str>,
    pub model: Option<&'a str>,
    pub alternate_model: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub status: Option<RequestStatus>,
    pub cost_state: Option<CostState>,
    pub diagnostic: Option<RequestDiagnostic>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

const PAGE: &str = "WITH page AS MATERIALIZED (SELECT r AS request FROM gateway_requests r";

const REQUEST_PROJECTION: &str = r#"
SELECT r.id, r.project_id, r.client_ref, r.quote, r.created_at,
       r.app_ref, r.provider, lifecycle.status,
       cost.cost_state, r.webhook_subscription_id,
       coalesce(p.upstream_handle, a.provider_handle) AS provider_handle,
       dispatch.submitted_at,
       r.succeeded_at AS completed_at, r.canceled_at, a.reported_usd_nanos,
       gateway_request_can_cancel(page.request) AS can_cancel,
       r.adapter, r.failed_at, r.uncertain_at, p.upstream_model,
       p.reported_model, p.reported_provider, p.finish_reason, p.error_code,
       p.result_expires_at, r.content_removed_at, u.input_tokens, u.output_tokens, u.amount_usd_nanos AS upstream_usd_nanos,
       charge.charge_unit, charge.charge_quantity::text AS charge_quantity,
       charge.reported_at AS charge_observed_at,
       p.accepted_at AS provider_accepted_at, p.queue_state AS provider_queue_state,
       p.result_observed_at AS provider_result_observed_at,
       CASE WHEN r.adapter IN ('fal_queue', 'kinovi_queue', 'connector_queue') AND p.accepted_at IS NOT NULL
              AND p.upstream_handle IS NOT NULL AND p.error_code = 'provider_poll_deadline'
              AND p.recheck_requested_at IS NULL AND r.content_removed_at IS NULL
              AND (r.provider_credential_version IS NULL OR EXISTS (
                SELECT 1 FROM gateway_provider_credentials c
                WHERE (c.account_id, c.scope_id, c.adapter, c.binding_slot, c.version) =
                      (r.account_id, r.provider_credential_scope_id, r.adapter,
                       r.provider_credential_binding_slot, r.provider_credential_version)
                  AND c.binding_digest = p.binding_digest
                  AND c.encrypted_key IS NOT NULL AND c.revoked_at IS NULL
              ))
            THEN r.uncertain_at + interval '24 hours' END AS provider_recheck_available_until,
       p.recheck_requested_at AS provider_recheck_requested_at,
       p.recheck_unconfirmed_at AS provider_recheck_unconfirmed_at,
       CASE WHEN p.recheck_requested_at IS NOT NULL THEN p.poll_deadline
            END AS provider_recheck_deadline,
       callback.received_at AS provider_callback_at,
       callback.recovered_from AS provider_callback_recovered_from,
       callback.reconciled_at AS provider_callback_reconciled_at, r.provider_credential_version,
       (r.provider_credential_scope_id = r.account_id) AS organization_credential,
       r.provider_credential_binding_slot,
       (lifecycle.status = 'queued' AND
        coalesce(control.pause_all OR r.provider = ANY(control.paused_providers), false)) AS dispatch_paused,
       settled.revision AS settlement_revision, settled.amount_usd_nanos AS settlement_amount,
       settled.recorded_at AS settlement_recorded_at, p.stream_started_at, p.stream_detached_at,
       r.trace_id, r.admission_span_id, r.caller_trace_id, r.caller_span_id,
       r.client_budget_revision, r.client_budget_limit_usd_nanos
FROM page
CROSS JOIN LATERAL (SELECT (page.request).*) r
"#;

const ATTEMPT: &str = r#"
LEFT JOIN gateway_provider_attempts a
  ON a.account_id = r.account_id AND a.request_id = r.id AND a.attempt_number = 1
"#;

const COST_EVIDENCE: &str = r#"
LEFT JOIN LATERAL (
  SELECT revision, amount_usd_nanos, recorded_at FROM gateway_supplier_settlements
  WHERE request_id = r.id ORDER BY revision DESC LIMIT 1
) settled ON true
LEFT JOIN LATERAL (
  SELECT input_tokens, output_tokens, amount_usd_nanos FROM gateway_usage_observations
  WHERE account_id = r.account_id AND request_id = r.id AND attempt_number = 1
    AND source IN ('openrouter_report', 'openrouter_stream_partial',
                   'local_chat_report', 'local_chat_stream_partial')
  ORDER BY source LIMIT 1
) u ON true
CROSS JOIN LATERAL (
  SELECT CASE
           WHEN settled.amount_usd_nanos IS NOT NULL THEN 'supplier_settled'
           WHEN u.amount_usd_nanos IS NOT NULL THEN 'provider_reported'
           WHEN a.reported_usd_nanos IS NOT NULL THEN 'mock_reported'
           ELSE 'estimated'
         END AS cost_state
) cost
"#;

const STATUS: &str = r#"
CROSS JOIN LATERAL (
  SELECT CASE WHEN r.adapter = 'mock' THEN a.submitted_at ELSE r.dispatched_at END AS submitted_at
) dispatch
CROSS JOIN LATERAL (
  SELECT CASE
           WHEN r.uncertain_at IS NOT NULL THEN 'awaiting_reconciliation'
           WHEN r.failed_at IS NOT NULL THEN 'failed'
           WHEN r.canceled_at IS NOT NULL THEN 'canceled'
           WHEN r.succeeded_at IS NOT NULL THEN 'succeeded'
           WHEN dispatch.submitted_at IS NOT NULL THEN 'running'
           ELSE 'queued'
         END AS status
) lifecycle
"#;

const TRACE_DETAILS: &str = r#"
LEFT JOIN gateway_dispatch_controls control ON control.account_id = r.account_id
LEFT JOIN gateway_provider_runs p ON p.request_id = r.id
LEFT JOIN LATERAL (
  SELECT charge_unit, charge_quantity, reported_at FROM gateway_usage_observations
  WHERE account_id = r.account_id AND request_id = r.id
    AND attempt_number = 1 AND source = 'connector_charge'
  LIMIT 1
) charge ON true
LEFT JOIN gateway_provider_callback_receipts callback ON callback.request_id = r.id
"#;

// Match durable warning evidence before attaching the full trace to a request.
const WARNINGS: &str = r#"
  r.uncertain_at IS NOT NULL
  OR EXISTS (
    SELECT 1 FROM gateway_provider_runs p WHERE p.request_id = r.id
    AND (
      jsonb_array_length(p.route_retries) > 0
      OR (r.adapter = 'openai_compatible_chat' AND r.dispatched_at IS NOT NULL AND EXISTS (
        SELECT 1 FROM jsonb_array_elements(p.compute_attempts) WITH ORDINALITY AS attempt(record, ordinal)
        WHERE record #>> '{result,status}' IN (
          'connection_failed', 'capacity_rejected', 'upstream_error',
          'transport_unconfirmed', 'deadline_before_send'
        ) OR (
          record ->> 'result' IS NULL
          AND (ordinal < jsonb_array_length(p.compute_attempts)
            OR num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at) > 0)
        )
      ))
    )
  )
  OR (r.webhook_subscription_id IS NOT NULL AND EXISTS (
    SELECT 1 FROM gateway_webhook_deliveries delivery
    CROSS JOIN LATERAL (
      SELECT outcome FROM gateway_webhook_attempts
      WHERE delivery_id = delivery.id ORDER BY number DESC LIMIT 50
    ) attempt
    WHERE delivery.request_id = r.id AND attempt.outcome = 'lease_expired'
  ))
"#;

pub async fn get_request<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    id: &GatewayRequestId,
) -> sqlx::Result<Option<RequestRow>> {
    let mut query = QueryBuilder::<Postgres>::new(PAGE);
    query
        .push(" WHERE r.id = ")
        .push_bind(id.as_str())
        .push(" AND ");
    push_scope(&mut query, scope);
    query.push(" LIMIT 1)");
    push_details(&mut query);
    query.build_query_as().fetch_optional(exec).await
}

pub async fn list_requests<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    filter: RequestFilter<'_>,
    limit: i64,
) -> sqlx::Result<Vec<RequestRow>> {
    // Select the page before attaching detailed traces, charges, and settlement evidence.
    let mut query = QueryBuilder::<Postgres>::new(PAGE);
    let indexed_filter = filter
        == RequestFilter {
            after: filter.after,
            app_ref: filter.app_ref,
            ..RequestFilter::default()
        };
    if indexed_filter && !(scope.client_ref.is_some() && filter.app_ref.is_some()) {
        push_indexed_page(&mut query, scope, filter.app_ref, filter.after);
    } else {
        if filter.status.is_some() || filter.cost_state.is_some() {
            query.push(ATTEMPT);
        }
        if filter.status.is_some() {
            query.push(STATUS);
        }
        if filter.cost_state.is_some() {
            query.push(COST_EVIDENCE);
        }
        query.push(" WHERE ");
        push_scope(&mut query, scope);
        push_cursor(&mut query, scope, filter.after);
        push_filters(&mut query, filter);
        query.push(" ORDER BY r.sid DESC");
    }
    query.push(" LIMIT ").push_bind(limit).push(")");
    push_details(&mut query);
    query.push(" ORDER BY (page.request).sid DESC");
    query.build_query_as().fetch_all(exec).await
}

fn push_indexed_page<'a>(
    query: &mut QueryBuilder<'a, Postgres>,
    scope: RequestScope<'a>,
    app_ref: Option<&'a str>,
    after: Option<&'a GatewayRequestId>,
) {
    query
        .push(" WHERE r.account_id = ")
        .push_bind(scope.account.as_str())
        .push(" AND ");
    let attribution = match (scope.client_ref, app_ref) {
        (Some(client), _) => Some(("r.client_ref", client)),
        (None, Some(app)) => Some(("r.app_ref", app)),
        (None, None) => None,
    };
    let (column, reference) = if let Some(attribution) = attribution {
        query
            .push("r.project_id = ")
            .push_bind(scope.project.as_str())
            .push(" AND ");
        attribution
    } else {
        ("r.project_id", scope.project.as_str())
    };
    // The one-element array keeps the scoped sort prefix without losing the index
    // equality lookup. PostgreSQL can seek by scope and SID even with a generic plan.
    query
        .push(column)
        .push(" = ANY(ARRAY[")
        .push_bind(reference)
        .push("])");
    if app_ref.is_some() {
        // Keep the partial app index eligible for generic plans.
        query.push(" AND r.app_ref IS NOT NULL");
    }
    push_cursor(query, scope, after);
    query.push(" ORDER BY ").push(column).push(", r.sid DESC");
}

fn push_scope<'a>(query: &mut QueryBuilder<'a, Postgres>, scope: RequestScope<'a>) {
    query
        .push("r.account_id = ")
        .push_bind(scope.account.as_str())
        .push(" AND r.project_id = ")
        .push_bind(scope.project.as_str());
    if let Some(client) = scope.client_ref {
        query.push(" AND r.client_ref = ").push_bind(client);
    }
}

fn push_cursor<'a>(
    query: &mut QueryBuilder<'a, Postgres>,
    scope: RequestScope<'a>,
    after: Option<&'a GatewayRequestId>,
) {
    if let Some(after) = after {
        query
            .push(" AND r.sid < (SELECT r.sid FROM gateway_requests r WHERE r.id = ")
            .push_bind(after.as_str())
            .push(" AND ");
        push_scope(query, scope);
        query.push(")");
    }
}

fn push_filters<'a>(query: &mut QueryBuilder<'a, Postgres>, filter: RequestFilter<'a>) {
    if let Some(app) = filter.app_ref {
        query.push(" AND r.app_ref = ").push_bind(app);
    }
    if let Some(model) = filter.model {
        query
            .push(" AND r.model IN (")
            .push_bind(model)
            .push(", ")
            .push_bind(filter.alternate_model)
            .push(")");
    }
    if let Some(provider) = filter.provider {
        query.push(" AND r.provider = ").push_bind(provider);
    }
    if let Some(status) = filter.status {
        query.push(" AND lifecycle.status = ").push_bind(status);
    }
    if let Some(cost) = filter.cost_state {
        query.push(" AND cost.cost_state = ").push_bind(cost);
    }
    if let Some(from) = filter.from {
        query.push(" AND r.created_at >= ").push_bind(from);
    }
    if let Some(to) = filter.to {
        query.push(" AND r.created_at < ").push_bind(to);
    }
    if let Some(trace) = filter.trace_id {
        query
            .push(" AND (r.trace_id = ")
            .push_bind(trace)
            .push(" OR r.caller_trace_id = ")
            .push_bind(trace)
            .push(")");
    }
    if filter.client_unattributed {
        query.push(" AND r.client_ref IS NULL");
    }
    if filter.app_unattributed {
        query.push(" AND r.app_ref IS NULL");
    }
    if let Some(diagnostic) = filter.diagnostic {
        query.push(" AND (");
        if diagnostic == RequestDiagnostic::FailedOrWarning {
            query.push("r.failed_at IS NOT NULL OR ");
        }
        query.push(WARNINGS).push(")");
    }
}

fn push_details(query: &mut QueryBuilder<'_, Postgres>) {
    query
        .push(REQUEST_PROJECTION)
        .push(ATTEMPT)
        .push(COST_EVIDENCE)
        .push(STATUS)
        .push(TRACE_DETAILS);
}
