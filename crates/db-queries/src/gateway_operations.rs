use chrono::{DateTime, Utc};
use ids::{AccountId, GatewayProjectId};
use sqlx::PgExecutor;

mod workflows;
pub use workflows::{workflow_snapshot, WorkflowSample, WORKFLOW_OUTCOME_SAMPLE_LIMIT};

pub struct QueueSample {
    pub kind: String,
    pub provider: Option<String>,
    pub workload: Option<String>,
    pub count: i64,
    pub dispatch_paused: bool,
    pub oldest_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

pub async fn queue_snapshot<'e>(
    exec: impl PgExecutor<'e>,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
) -> sqlx::Result<Vec<QueueSample>> {
    sqlx::query_as!(
        QueueSample,
        r#"WITH open_requests AS (
            SELECT CASE
                WHEN uncertain_at IS NOT NULL THEN 'awaiting_reconciliation'
                WHEN dispatched_at IS NULL THEN 'queued'
                ELSE 'running'
            END AS kind,
            CASE WHEN provider IN ('fal', 'kinovi', 'openrouter', 'self_hosted') THEN provider
                 ELSE 'other' END AS provider,
            CASE WHEN quote #>> '{input,workload,kind}' IN ('image', 'video', 'audio', 'text', 'mesh')
                 THEN quote #>> '{input,workload,kind}' ELSE 'other' END AS workload,
            coalesce(r.uncertain_at, r.dispatched_at, r.created_at) AS entered_at,
            (r.uncertain_at IS NULL AND r.dispatched_at IS NULL AND
                coalesce(control.pause_all OR r.provider = ANY(control.paused_providers), false)) AS dispatch_paused
            FROM gateway_requests r
            LEFT JOIN gateway_dispatch_controls control ON control.account_id = r.account_id
            WHERE r.account_id = $1 AND ($2::text IS NULL OR r.project_id = $2)
              AND num_nonnulls(succeeded_at, failed_at, canceled_at) = 0
        ), deliveries AS NOT MATERIALIZED (
            SELECT 'webhook_pending' AS kind, w.request_id, w.workflow_run_id, w.occurred_at AS entered_at
            FROM gateway_webhook_deliveries w WHERE state IN ('pending', 'sending')
            UNION ALL
            SELECT 'webhook_dead' AS kind, w.request_id, w.workflow_run_id, w.finished_at AS entered_at
            FROM gateway_webhook_deliveries w
            WHERE state = 'dead' AND finished_at >= transaction_timestamp() - interval '24 hours'
        ), scoped_deliveries AS (
            SELECT w.kind, w.entered_at
            FROM deliveries w JOIN gateway_requests r ON r.id = w.request_id
            WHERE r.account_id = $1 AND ($2::text IS NULL OR r.project_id = $2)
            UNION ALL
            SELECT w.kind, w.entered_at
            FROM deliveries w JOIN workflow_runs r ON r.id = w.workflow_run_id
            WHERE r.account_id = $1 AND ($2::text IS NULL OR r.project_id = $2)
        ), queues AS (
            SELECT kind, provider, workload, count(*) AS count, min(entered_at) AS oldest_at, dispatch_paused
            FROM open_requests GROUP BY kind, provider, workload, dispatch_paused
            UNION ALL
            SELECT w.kind, NULL, NULL, count(*), min(w.entered_at), false
            FROM scoped_deliveries w
            GROUP BY w.kind
            UNION ALL
            SELECT 'export_pending', NULL, NULL, count(*), min(occurred_at), false
            FROM gateway_export_events
            WHERE account_id = $1 AND ($2::text IS NULL OR project_id = $2) AND published_seq IS NULL
        )
        SELECT kind AS "kind!", provider, workload, count AS "count!", oldest_at, dispatch_paused AS "dispatch_paused!",
               transaction_timestamp() AS "observed_at!"
        FROM queues ORDER BY kind, provider, workload"#,
        account.as_str(),
        project.map(GatewayProjectId::as_str),
    )
    .fetch_all(exec)
    .await
}

pub const PUBLICATION_SAMPLE_LIMIT: i64 = 5_000;

pub struct PublicationSample {
    pub total: bool,
    pub provider: Option<String>,
    pub workload: Option<String>,
    pub observed: i64,
    pub published: i64,
    pub pending: i64,
    pub unconfirmed: i64,
    pub timing_samples: i64,
    pub p95_ms: Option<i64>,
    pub oldest_pending_at: Option<DateTime<Utc>>,
    pub sampled_admissions: i64,
    pub observed_at: DateTime<Utc>,
}

pub async fn publication_snapshot<'e>(
    exec: impl PgExecutor<'e>,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
) -> sqlx::Result<Vec<PublicationSample>> {
    sqlx::query_as!(
        PublicationSample,
        r#"WITH recent AS MATERIALIZED (
             SELECT r.id, r.provider, r.quote, r.succeeded_at, r.failed_at, r.canceled_at, r.uncertain_at
             FROM gateway_requests r
             WHERE r.account_id = $1 AND ($2::text IS NULL OR r.project_id = $2)
               AND r.adapter IN ('fal_queue', 'kinovi_queue', 'connector_queue')
               AND r.created_at >= statement_timestamp() - interval '24 hours'
               AND r.created_at <= statement_timestamp()
             ORDER BY r.created_at DESC, r.sid DESC LIMIT $3
           ), publication AS (
             SELECT CASE WHEN r.provider IN ('fal', 'kinovi', 'self_hosted') THEN r.provider ELSE 'other' END AS provider,
                    CASE WHEN r.quote #>> '{input,workload,kind}' IN ('image', 'video', 'audio', 'mesh')
                         THEN r.quote #>> '{input,workload,kind}' ELSE 'other' END AS workload,
                    p.result_observed_at, r.succeeded_at, r.uncertain_at,
                    num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at, r.uncertain_at) = 0 AS pending,
                    CASE WHEN r.succeeded_at >= p.result_observed_at
                         THEN extract(epoch FROM r.succeeded_at - p.result_observed_at) * 1000 END AS lag_ms
             FROM recent r JOIN gateway_provider_runs p ON p.request_id = r.id
             WHERE p.result_observed_at IS NOT NULL
           )
           SELECT grouping(provider, workload) = 3 AS "total!", provider, workload,
                  count(*) AS "observed!", count(succeeded_at) AS "published!",
                  count(*) FILTER (WHERE pending) AS "pending!", count(uncertain_at) AS "unconfirmed!",
                  count(lag_ms) AS "timing_samples!",
                  round(percentile_cont(0.95) WITHIN GROUP (ORDER BY lag_ms))::bigint AS p95_ms,
                  min(result_observed_at) FILTER (WHERE pending) AS oldest_pending_at,
                  (SELECT count(*) FROM recent) AS "sampled_admissions!",
                  statement_timestamp() AS "observed_at!"
           FROM publication GROUP BY GROUPING SETS ((), (provider, workload))
           ORDER BY grouping(provider, workload) DESC, provider, workload"#,
        account.as_str(),
        project.map(GatewayProjectId::as_str),
        PUBLICATION_SAMPLE_LIMIT,
    )
    .fetch_all(exec)
    .await
}
