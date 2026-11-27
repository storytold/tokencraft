use chrono::{DateTime, Utc};
use db_queries::gateway_webhooks::{Delivery, DeliveryAttempt, Subscription};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub(in crate::gateway_demo) struct WebhookSubscription {
    pub(super) id: String,
    name: String,
    destination_id: String,
    #[schema(required = true)]
    client_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_app_id: Option<String>,
    pub(super) signing_version: i32,
    #[schema(required = true)]
    signing_rotated_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    #[schema(required = true)]
    revoked_at: Option<DateTime<Utc>>,
}

impl From<Subscription> for WebhookSubscription {
    fn from(row: Subscription) -> Self {
        Self {
            id: row.id,
            name: row.name,
            destination_id: row.destination_id,
            client_ref: row.client_ref,
            workflow_app_id: row.workflow_app_id,
            signing_version: row.signing_version,
            signing_rotated_at: row.signing_rotated_at,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub(in crate::gateway_demo) struct WebhookDelivery {
    pub(super) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow_run_id: Option<String>,
    subscription_id: String,
    event_type: String,
    /// One of pending, sending, delivered, or dead.
    state: String,
    occurred_at: DateTime<Utc>,
    /// Delivery-specific retry time or active lease deadline.
    available_at: DateTime<Utc>,
    /// End of an active receiver delay affecting this pending delivery.
    #[schema(required = true)]
    receiver_available_after: Option<DateTime<Utc>>,
    /// Earliest eligible attempt for a pending delivery, including receiver delay and the retry window.
    #[schema(required = true)]
    next_attempt_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    finished_at: Option<DateTime<Utc>>,
    attempt_count: i32,
    #[schema(required = true)]
    last_error: Option<String>,
}

impl From<Delivery> for WebhookDelivery {
    fn from(row: Delivery) -> Self {
        Self {
            id: row.id,
            request_id: row.request_id,
            workflow_run_id: row.workflow_run_id,
            subscription_id: row.subscription_id,
            event_type: row.event_type,
            state: row.state,
            occurred_at: row.occurred_at,
            available_at: row.available_at,
            receiver_available_after: row.receiver_available_after,
            next_attempt_at: row.next_attempt_at,
            finished_at: row.finished_at,
            attempt_count: row.attempt_count,
            last_error: row.last_error,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub(super) struct WebhookDeliveryAttempt {
    number: i32,
    started_at: DateTime<Utc>,
    #[schema(required = true)]
    finished_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    outcome: Option<String>,
    #[schema(required = true)]
    http_status: Option<i16>,
}

impl From<DeliveryAttempt> for WebhookDeliveryAttempt {
    fn from(row: DeliveryAttempt) -> Self {
        Self {
            number: row.number,
            started_at: row.started_at,
            finished_at: row.finished_at,
            outcome: row.outcome,
            http_status: row.http_status,
        }
    }
}
