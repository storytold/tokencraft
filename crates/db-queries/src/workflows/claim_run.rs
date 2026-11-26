use super::Run;
use ids::{AccountId, GatewayProjectId, WorkflowId, WorkflowRunId};

pub async fn claim_run<'e, E: sqlx::PgExecutor<'e>>(exec: E) -> sqlx::Result<Option<Run>> {
    sqlx::query_as!(Run,
        r#"WITH candidate AS (
            SELECT id, deadline <= statement_timestamp() AS expired
            FROM workflow_runs
            WHERE status IN ('queued', 'running', 'reconciling') AND next_at <= statement_timestamp()
            ORDER BY next_at, id LIMIT 1 FOR UPDATE SKIP LOCKED
        )
        UPDATE workflow_runs w
        SET lease = w.lease + 1,
            next_at = least(w.deadline, statement_timestamp() + interval '45 seconds'),
            status = CASE WHEN candidate.expired THEN 'expired' ELSE w.status END,
            revision = w.revision + CASE WHEN candidate.expired THEN 1 ELSE 0 END,
            updated_at = CASE WHEN candidate.expired THEN statement_timestamp() ELSE w.updated_at END
        FROM candidate
        WHERE w.id = candidate.id
        RETURNING w.id AS "id: WorkflowRunId",
            w.account_id AS "account_id: AccountId", w.project_id AS "project_id: GatewayProjectId",
            w.workflow_id AS "workflow_id: WorkflowId", w.actor_id, w.status AS "status!", w.revision AS "revision!",
            w.lease, w.version, w.draft_revision, w.graph, w.content, w.deadline, w.created_at"#,

    )
    .fetch_optional(exec)
    .await
}
