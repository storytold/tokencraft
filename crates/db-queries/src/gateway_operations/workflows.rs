use chrono::{DateTime, Utc};
use ids::{AccountId, GatewayProjectId};
use sqlx::PgExecutor;

pub const WORKFLOW_OUTCOME_SAMPLE_LIMIT: i64 = 5_000;

pub struct WorkflowSample {
    pub queued: i64,
    pub running: i64,
    pub reconciling: i64,
    pub due_queued: i64,
    pub overdue: i64,
    pub oldest_queued_at: Option<DateTime<Utc>>,
    pub oldest_running_at: Option<DateTime<Utc>>,
    pub oldest_reconciling_at: Option<DateTime<Utc>>,
    pub sampled: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub canceled: i64,
    pub expired: i64,
    pub timing_samples: i64,
    pub p95_succeeded_ms: Option<i64>,
}

pub async fn workflow_snapshot<'e>(
    exec: impl PgExecutor<'e>,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
) -> sqlx::Result<WorkflowSample> {
    sqlx::query_as!(
        WorkflowSample,
        r#"WITH active AS (
             SELECT count(*) FILTER (WHERE status = 'queued') AS queued,
                    count(*) FILTER (WHERE status = 'running') AS running,
                    count(*) FILTER (WHERE status = 'reconciling') AS reconciling,
                    count(*) FILTER (WHERE status = 'queued' AND next_at <= transaction_timestamp()
                                     AND deadline > transaction_timestamp()) AS due_queued,
                    count(*) FILTER (WHERE deadline <= transaction_timestamp()) AS overdue,
                    min(created_at) FILTER (WHERE status = 'queued') AS oldest_queued_at,
                    min(created_at) FILTER (WHERE status = 'running') AS oldest_running_at,
                    min(created_at) FILTER (WHERE status = 'reconciling') AS oldest_reconciling_at
             FROM workflow_runs
             WHERE account_id = $1 AND ($2::text IS NULL OR project_id = $2)
               AND status IN ('queued', 'running', 'reconciling')
           ), terminal AS NOT MATERIALIZED (
             SELECT project_id, id, status, created_at, updated_at
             FROM workflow_runs
             WHERE account_id = $1
               AND status IN ('succeeded', 'failed', 'canceled', 'expired')
               AND updated_at BETWEEN transaction_timestamp() - interval '24 hours' AND transaction_timestamp()
           ), recent AS (
             (SELECT status, created_at, updated_at FROM terminal WHERE $2::text IS NULL
              ORDER BY updated_at DESC, id DESC LIMIT $3)
             UNION ALL
             (SELECT status, created_at, updated_at FROM terminal WHERE project_id = $2
              ORDER BY updated_at DESC, id DESC LIMIT $3)
           ), timing AS (
             SELECT status, CASE WHEN status = 'succeeded' AND updated_at >= created_at
                                 THEN extract(epoch FROM updated_at - created_at) * 1000 END AS elapsed_ms
             FROM recent
           ), outcomes AS (
             SELECT count(*) AS sampled,
                    count(*) FILTER (WHERE status = 'succeeded') AS succeeded,
                    count(*) FILTER (WHERE status = 'failed') AS failed,
                    count(*) FILTER (WHERE status = 'canceled') AS canceled,
                    count(*) FILTER (WHERE status = 'expired') AS expired,
                    count(elapsed_ms) AS timing_samples,
                    round(percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_ms))::bigint AS p95_succeeded_ms
             FROM timing
           )
           SELECT queued AS "queued!", running AS "running!", reconciling AS "reconciling!",
                  due_queued AS "due_queued!", overdue AS "overdue!",
                  oldest_queued_at, oldest_running_at, oldest_reconciling_at,
                  sampled AS "sampled!", succeeded AS "succeeded!", failed AS "failed!",
                  canceled AS "canceled!", expired AS "expired!", timing_samples AS "timing_samples!", p95_succeeded_ms
           FROM active CROSS JOIN outcomes"#,
        account.as_str(),
        project.map(GatewayProjectId::as_str),
        WORKFLOW_OUTCOME_SAMPLE_LIMIT,
    )
    .fetch_one(exec)
    .await
}
