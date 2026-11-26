use super::Run;
use ids::{AccountId, GatewayProjectId, WorkflowId, WorkflowRunId};

pub async fn claim_run<'e, E: sqlx::PgExecutor<'e>>(exec: E) -> sqlx::Result<Option<Run>> {
    sqlx::query_as!(Run,
        r#"UPDATE workflow_runs SET lease = lease + 1, next_at = now() + interval '45 seconds'
        WHERE id = (SELECT id FROM workflow_runs WHERE status IN ('queued','running','reconciling') AND next_at <= now()
        ORDER BY next_at, id LIMIT 1 FOR UPDATE SKIP LOCKED)
        RETURNING id AS "id: WorkflowRunId",
            account_id AS "account_id: AccountId", project_id AS "project_id: GatewayProjectId",
            workflow_id AS "workflow_id: WorkflowId", actor_id, status, revision, lease,
            version, draft_revision, graph, content, deadline, created_at"#,

    )
    .fetch_optional(exec)
    .await
}
