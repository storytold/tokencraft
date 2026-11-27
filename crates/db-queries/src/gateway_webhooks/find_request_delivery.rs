use ids::GatewayRequestId;
use sqlx::PgExecutor;

use crate::gateway_requests::RequestScope;

use super::queue::MAX_ATTEMPTS;
use super::Delivery;

pub async fn find_request_delivery<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    request: &GatewayRequestId,
) -> sqlx::Result<Option<Delivery>> {
    sqlx::query_as!(
        Delivery,
        r#"SELECT d.id, d.request_id, d.workflow_run_id, s.id AS subscription_id,
                  d.event_type, d.state, d.occurred_at, d.available_at, d.finished_at,
                  d.attempt_count, d.last_error,
                  c.available_after AS "receiver_available_after?",
                  CASE WHEN d.state = 'pending' AND s.revoked_at IS NULL
                            AND d.attempt_count - d.attempt_baseline < $5
                            AND greatest(d.available_at, c.available_after, statement_timestamp()) < d.deadline_at
                       THEN greatest(d.available_at, c.available_after) END AS "next_attempt_at?"
           FROM gateway_webhook_deliveries d JOIN gateway_requests r ON r.id = d.request_id
           JOIN gateway_webhook_subscriptions s ON s.id = r.webhook_subscription_id
           LEFT JOIN gateway_webhook_cooldowns c
             ON d.state = 'pending' AND s.revoked_at IS NULL AND c.available_after > statement_timestamp()
               AND c.receiver_digest = sha256(convert_to(s.destination_url, 'UTF8'))
           WHERE r.account_id = $1 AND r.project_id = $2
             AND ($3::text IS NULL OR r.client_ref = $3) AND r.id = $4"#,
        scope.account.as_str(),
        scope.project.as_str(),
        scope.client_ref,
        request.as_str(),
        MAX_ATTEMPTS,
    )
    .fetch_optional(exec)
    .await
}
