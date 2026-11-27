use chrono::{DateTime, Utc};
use ids::{AccountId, GatewayProjectId, GatewayRequestId};
use serde::{Deserialize, Serialize};

pub mod active_requests;
pub mod submission_limits;
pub mod artifact_objects;
pub mod artifacts;
mod cancel_mock_attempts;
mod cancel_request;
pub mod content_history;
pub mod content_restores;
pub mod chat_streams;
pub mod client_budgets;
pub mod compute_attempts;
pub mod connector_completions;
mod complete_mock_attempts;
mod dispatch_mock_requests;
pub mod queued_runs;
pub mod queue_cooldowns;
mod queue_turns;
pub(crate) use queue_turns::lock_submission_lanes;
pub mod route_performance;
mod trace_baseline;
pub use trace_baseline::trace_baseline;
mod find_admission;
mod find_request_location;
mod insert_request;
mod list_requests;
pub mod monthly_budget;
mod provider_runs;
mod read_snapshot;
mod recheck_provider;
mod rollout_backlog;
mod remove_content;
mod report_usage;
pub mod workflow_frames;

pub use cancel_mock_attempts::cancel_mock_attempts;
pub use cancel_request::{begin_provider_submission, cancel_request};
pub use complete_mock_attempts::complete_mock_attempts;
pub use dispatch_mock_requests::dispatch_mock_requests;
pub use find_admission::{find_admission, AdmissionKey};
pub use find_request_location::find_request_location;
pub use insert_request::{insert_request, NewGatewayRequest, RequestCorrelation};
pub use list_requests::{get_request, list_requests, RequestFilter};
pub use provider_runs::*;
pub use read_snapshot::read_snapshot;
pub use recheck_provider::{recheck_provider, Recheck};
pub use remove_content::{expire_results, remove_content, ContentRemoval};
pub use report_usage::{report_usage, UsageFilter, UsageRow};
pub use rollout_backlog::{binding_dependencies, BindingDependency};

pub struct Admission {
    pub id: GatewayRequestId,
    pub intent_digest: String,
    pub trace_id: Option<String>,
}

pub struct RequestLocation {
    pub account_id: AccountId,
    pub project_id: GatewayProjectId,
    pub client_ref: Option<String>,
}

#[derive(Clone, Copy)]
pub struct RequestScope<'a> {
    pub account: &'a AccountId,
    pub project: &'a GatewayProjectId,
    pub client_ref: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum RequestStatus {
    Queued,
    Running,
    Succeeded,
    Canceled,
    Failed,
    AwaitingReconciliation,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum CostState {
    Estimated,
    MockReported,
    ProviderReported,
    SupplierSettled,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestDiagnostic {
    Warnings,
    FailedOrWarning,
}

#[derive(sqlx::FromRow)]
pub struct RequestRow {
    pub id: GatewayRequestId,
    pub project_id: GatewayProjectId,
    pub client_ref: Option<String>,
    pub app_ref: Option<String>,
    pub provider: String,
    pub status: RequestStatus,
    pub cost_state: CostState,
    pub webhook_subscription_id: Option<String>,
    pub quote: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub provider_handle: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub canceled_at: Option<DateTime<Utc>>,
    pub can_cancel: bool,
    pub reported_usd_nanos: Option<i64>,
    pub adapter: String,
    pub failed_at: Option<DateTime<Utc>>,
    pub uncertain_at: Option<DateTime<Utc>>,
    pub upstream_model: Option<String>,
    pub reported_model: Option<String>,
    pub reported_provider: Option<String>,
    pub finish_reason: Option<String>,
    pub error_code: Option<String>,
    pub result_expires_at: Option<DateTime<Utc>>,
    pub content_removed_at: Option<DateTime<Utc>>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub upstream_usd_nanos: Option<i64>,
    pub charge_unit: Option<String>,
    pub charge_quantity: Option<String>,
    pub charge_observed_at: Option<DateTime<Utc>>,
    pub provider_accepted_at: Option<DateTime<Utc>>,
    pub provider_result_observed_at: Option<DateTime<Utc>>,
    pub provider_queue_state: Option<String>,
    pub provider_recheck_available_until: Option<DateTime<Utc>>,
    pub provider_recheck_requested_at: Option<DateTime<Utc>>,
    pub provider_recheck_unconfirmed_at: Option<DateTime<Utc>>,
    pub provider_recheck_deadline: Option<DateTime<Utc>>,
    pub provider_callback_at: Option<DateTime<Utc>>,
    pub provider_callback_recovered_from: Option<DateTime<Utc>>,
    pub provider_callback_reconciled_at: Option<DateTime<Utc>>,
    pub provider_credential_version: Option<i32>,
    pub organization_credential: Option<bool>,
    pub dispatch_paused: bool,
    pub provider_credential_binding_slot: Option<String>,
    pub settlement_revision: Option<i32>,
    pub settlement_amount: Option<i64>,
    pub settlement_recorded_at: Option<DateTime<Utc>>,
    pub stream_started_at: Option<DateTime<Utc>>,
    pub stream_detached_at: Option<DateTime<Utc>>,
    pub trace_id: Option<String>,
    pub admission_span_id: Option<String>,
    pub caller_trace_id: Option<String>,
    pub caller_span_id: Option<String>,
    pub client_budget_revision: Option<i32>,
    pub client_budget_limit_usd_nanos: Option<i64>,
}
mod read_route_retries;
pub use read_route_retries::read_route_retries;
mod reschedule_queue_submission;
pub use reschedule_queue_submission::reschedule_queue_submission;
