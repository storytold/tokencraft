use ids::GatewayRequestId;
use sqlx::PgExecutor;

use crate::gateway_requests::RequestScope;

use super::Delivery;

pub async fn find_request_delivery<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    request: &GatewayRequestId,
) -> sqlx::Result<Option<Delivery>> {
    sqlx::query_as!(
        Delivery,
        r#"SELECT d.id, d.request_id, d.workflow_run_id, r.webhook_subscription_id AS "subscription_id!",
                  d.event_type, d.state, d.occurred_at, d.available_at, d.finished_at,
                  d.attempt_count, d.last_error
           FROM gateway_webhook_deliveries d JOIN gateway_requests r ON r.id = d.request_id
           WHERE r.account_id = $1 AND r.project_id = $2
             AND ($3::text IS NULL OR r.client_ref = $3) AND r.id = $4"#,
        scope.account.as_str(),
        scope.project.as_str(),
        scope.client_ref,
        request.as_str(),
    )
    .fetch_optional(exec)
    .await
}
