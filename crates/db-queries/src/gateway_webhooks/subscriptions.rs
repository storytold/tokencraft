use chrono::{DateTime, Utc};
use ids::{GatewayWebhookSubscriptionId, WorkflowAppId};
use serde::Serialize;
use sqlx::PgExecutor;

use crate::gateway_requests::RequestScope;

#[derive(Serialize)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub destination_id: String,
    pub client_ref: Option<String>,
    pub workflow_app_id: Option<String>,
    pub signing_version: i32,
    pub signing_rotated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub struct NewSubscription<'a> {
    pub id: &'a GatewayWebhookSubscriptionId,
    pub name: &'a str,
    pub destination_id: &'a str,
    pub destination_url: &'a str,
    pub client_ref: Option<&'a str>,
    pub workflow_app_id: Option<&'a WorkflowAppId>,
}

pub async fn create_subscription<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    subscription: NewSubscription<'_>,
) -> sqlx::Result<Subscription> {
    sqlx::query_as!(
        Subscription,
        "INSERT INTO gateway_webhook_subscriptions
         (id, account_id, project_id, name, destination_id, destination_url, client_ref, workflow_app_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id, name, destination_id, client_ref, workflow_app_id, signing_version, signing_rotated_at,
                   created_at, revoked_at",
        subscription.id.as_str(), scope.account.as_str(), scope.project.as_str(), subscription.name,
        subscription.destination_id, subscription.destination_url, subscription.client_ref,
        subscription.workflow_app_id.map(WorkflowAppId::as_str),
    )
    .fetch_one(exec)
    .await
}

pub async fn subscriptions<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    before: Option<&GatewayWebhookSubscriptionId>,
    limit: i64,
) -> sqlx::Result<Vec<Subscription>> {
    sqlx::query_as!(
        Subscription,
        "SELECT id, name, destination_id, client_ref, workflow_app_id, signing_version, signing_rotated_at, created_at, revoked_at
         FROM gateway_webhook_subscriptions
         WHERE account_id = $1 AND project_id = $2 AND ($3::text IS NULL OR id < $3)
         ORDER BY id DESC LIMIT $4",
        scope.account.as_str(), scope.project.as_str(), before.map(GatewayWebhookSubscriptionId::as_str), limit,
    )
    .fetch_all(exec)
    .await
}

pub async fn revoke_subscription<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    subscription: &GatewayWebhookSubscriptionId,
) -> sqlx::Result<bool> {
    let res = sqlx::query!(
        "UPDATE gateway_webhook_subscriptions SET revoked_at = clock_timestamp()
         WHERE id = $1 AND account_id = $2 AND project_id = $3 AND revoked_at IS NULL",
        subscription.as_str(),
        scope.account.as_str(),
        scope.project.as_str(),
    )
    .execute(exec)
    .await?;
    Ok(res.rows_affected() == 1)
}

pub async fn find_subscription<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    subscription: &GatewayWebhookSubscriptionId,
) -> sqlx::Result<Option<Subscription>> {
    sqlx::query_as!(
        Subscription,
        "SELECT id, name, destination_id, client_ref, workflow_app_id, signing_version, signing_rotated_at, created_at, revoked_at
         FROM gateway_webhook_subscriptions
         WHERE id = $1 AND account_id = $2 AND project_id = $3",
        subscription.as_str(), scope.account.as_str(), scope.project.as_str(),
    )
    .fetch_optional(exec)
    .await
}

pub async fn rotate_subscription<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    subscription: &GatewayWebhookSubscriptionId,
) -> sqlx::Result<Option<Subscription>> {
    sqlx::query_as!(
        Subscription,
        "UPDATE gateway_webhook_subscriptions
         SET signing_version = signing_version + 1, signing_rotated_at = clock_timestamp()
         WHERE id = $1 AND account_id = $2 AND project_id = $3 AND revoked_at IS NULL
           AND (signing_rotated_at IS NULL OR signing_rotated_at <= clock_timestamp() - interval '24 hours')
         RETURNING id, name, destination_id, client_ref, workflow_app_id, signing_version, signing_rotated_at,
                   created_at, revoked_at",
        subscription.as_str(), scope.account.as_str(), scope.project.as_str(),
    )
    .fetch_optional(exec)
    .await
}

pub async fn authorize_destination<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    subscription: &GatewayWebhookSubscriptionId,
    request_client: Option<&str>,
) -> sqlx::Result<Option<(String, String)>> {
    let row = sqlx::query!(
        "SELECT destination_id, destination_url FROM gateway_webhook_subscriptions
         WHERE id = $1 AND account_id = $2 AND project_id = $3 AND revoked_at IS NULL
           AND workflow_app_id IS NULL
           AND ((client_ref IS NULL AND $4::text IS NULL) OR client_ref = $5::text)
         FOR SHARE",
        subscription.as_str(),
        scope.account.as_str(),
        scope.project.as_str(),
        scope.client_ref,
        request_client,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|row| (row.destination_id, row.destination_url)))
}

pub async fn workflow_app_exists<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    app: &WorkflowAppId,
) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM workflow_apps WHERE account_id = $1 AND project_id = $2 AND id = $3
           ) AS "present!""#,
        scope.account.as_str(),
        scope.project.as_str(),
        app.as_str(),
    )
    .fetch_one(exec)
    .await
}

pub async fn active_workflow_subscription<'e, E: PgExecutor<'e>>(
    exec: E,
    scope: crate::workflows::Scope<'_>,
    app: &WorkflowAppId,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT id FROM gateway_webhook_subscriptions
         WHERE account_id = $1 AND project_id = $2 AND workflow_app_id = $3 AND revoked_at IS NULL
         FOR SHARE",
        scope.account.as_str(),
        scope.project.as_str(),
        app.as_str(),
    )
    .fetch_optional(exec)
    .await
}
