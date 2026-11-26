use chrono::{DateTime, Utc};
use db_queries::gateway_operations::{WorkflowSample, WORKFLOW_OUTCOME_SAMPLE_LIMIT};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub(super) struct WorkflowOperations {
    queued: WorkflowDepth,
    running: WorkflowDepth,
    reconciling: WorkflowDepth,
    /// Queued runs eligible to be claimed at the observation time.
    due_queued: i64,
    /// Active runs whose deadline has already passed.
    overdue: i64,
    outcomes: WorkflowOutcomes,
}

#[derive(Serialize, ToSchema)]
struct WorkflowDepth {
    count: i64,
    /// Admission timestamp of the oldest run in this state.
    oldest_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
struct WorkflowOutcomes {
    /// Terminal runs sampled from the past 24 hours.
    sampled: i64,
    sample_limit: i64,
    succeeded: i64,
    failed: i64,
    canceled: i64,
    expired: i64,
    timing_samples: i64,
    /// Time from admission to completion for successful runs.
    p95_succeeded_ms: Option<i64>,
}

impl From<WorkflowSample> for WorkflowOperations {
    fn from(row: WorkflowSample) -> Self {
        Self {
            queued: WorkflowDepth { count: row.queued, oldest_at: row.oldest_queued_at },
            running: WorkflowDepth { count: row.running, oldest_at: row.oldest_running_at },
            reconciling: WorkflowDepth { count: row.reconciling, oldest_at: row.oldest_reconciling_at },
            due_queued: row.due_queued,
            overdue: row.overdue,
            outcomes: WorkflowOutcomes {
                sampled: row.sampled,
                sample_limit: WORKFLOW_OUTCOME_SAMPLE_LIMIT,
                succeeded: row.succeeded,
                failed: row.failed,
                canceled: row.canceled,
                expired: row.expired,
                timing_samples: row.timing_samples,
                p95_succeeded_ms: row.p95_succeeded_ms,
            },
        }
    }
}
