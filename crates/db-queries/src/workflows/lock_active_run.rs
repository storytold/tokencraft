use chrono::{DateTime, Utc};
use ids::WorkflowRunId;

pub struct LockedRun {
    pub budget_month_start: Option<DateTime<Utc>>,
}

pub async fn lock_active_run<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    run: &WorkflowRunId,
    lease: i64,
) -> sqlx::Result<Option<LockedRun>> {
    sqlx::query_as!(
        LockedRun,
        "SELECT budget_month_start FROM workflow_runs WHERE id = $1 AND lease = $2
        AND status IN ('queued','running','reconciling')
        AND next_at > clock_timestamp() AND deadline > clock_timestamp() FOR UPDATE",
        run.as_str(),
        lease
    )
    .fetch_optional(exec)
    .await
}
