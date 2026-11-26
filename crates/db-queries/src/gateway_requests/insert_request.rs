use chrono::{DateTime, Utc};
use ids::{GatewayRequestId, GatewayWebhookSubscriptionId};
use sqlx::PgExecutor;

use super::AdmissionKey;
use crate::gateway_provider_credentials::Effective;

pub struct RequestCorrelation<'a> {
    pub trace_id: &'a str,
    pub admission_span_id: &'a str,
    pub caller: Option<(&'a str, &'a str)>,
}

pub struct NewGatewayRequest<'a> {
    pub id: &'a GatewayRequestId,
    pub key: AdmissionKey<'a>,
    pub intent_digest: &'a str,
    pub client_ref: Option<&'a str>,
    pub app_ref: Option<&'a str>,
    pub webhook_subscription: Option<&'a GatewayWebhookSubscriptionId>,
    pub model: &'a str,
    pub offer_id: &'a str,
    pub provider: &'a str,
    pub quote: &'a serde_json::Value,
    pub adapter: &'a str,
    pub mock_latency_ms: Option<i32>,
    pub estimated_usd_nanos: i64,
    pub budget_reserved_usd_nanos: i64,
    pub workflow_budget_month_start: Option<DateTime<Utc>>,
    pub correlation: RequestCorrelation<'a>,
    pub client_budget: Option<(i32, i64)>,
    pub credential: Option<&'a Effective>,
}

pub async fn insert_request<'e, E: PgExecutor<'e>>(
    exec: E,
    request: NewGatewayRequest<'_>,
) -> sqlx::Result<bool> {
    let affected = sqlx::query!(
        "INSERT INTO gateway_requests
         (id, account_id, project_id, actor_id, idempotency_digest, intent_digest, client_ref, model, offer_id,
          provider, quote, adapter, mock_latency_ms, estimated_usd_nanos, webhook_subscription_id,
          budget_reserved_usd_nanos, app_ref,
          trace_id, admission_span_id, caller_trace_id, caller_span_id,
          client_budget_revision, client_budget_limit_usd_nanos, provider_credential_scope_id,
          provider_credential_binding_slot, provider_credential_version, workflow_budget_month_start)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20,
                 $21, $22, $23, $24, $25, $26, $27)
         ON CONFLICT (account_id, project_id, actor_id, idempotency_digest) DO NOTHING",
        request.id.as_str(),
        request.key.scope.account.as_str(),
        request.key.scope.project.as_str(),
        request.key.actor_id,
        request.key.idempotency_digest,
        request.intent_digest,
        request.client_ref,
        request.model,
        request.offer_id,
        request.provider,
        request.quote,
        request.adapter,
        request.mock_latency_ms,
        request.estimated_usd_nanos,
        request.webhook_subscription.map(GatewayWebhookSubscriptionId::as_str),
        request.budget_reserved_usd_nanos,
        request.app_ref,
        request.correlation.trace_id,
        request.correlation.admission_span_id,
        request.correlation.caller.map(|(trace_id, _)| trace_id),
        request.correlation.caller.map(|(_, span_id)| span_id),
        request.client_budget.map(|(revision, _)| revision),
        request.client_budget.map(|(_, limit)| limit),
        request.credential.map(|key| {
            if key.organization { request.key.scope.account.as_str() } else { request.key.scope.project.as_str() }
        }),
        request.credential.map(|key| key.binding.as_str()),
        request.credential.map(|key| key.version),
        request.workflow_budget_month_start,
    )
    .execute(exec)
    .await?
    .rows_affected();
    Ok(affected == 1)
}
