use super::{Run, Scope};
use ids::{AccountId, GatewayProjectId, WorkflowId, WorkflowRunId};

pub async fn find_run<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
    run: &WorkflowRunId,
) -> sqlx::Result<Option<Run>> {
    sqlx::query_as!(Run,
        r#"SELECT id AS "id: WorkflowRunId",
            account_id AS "account_id: AccountId", project_id AS "project_id: GatewayProjectId",
            workflow_id AS "workflow_id: WorkflowId", actor_id, status, revision, lease,
            version, draft_revision, graph, content, deadline, created_at FROM workflow_runs WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3 AND id = $4"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(), run.as_str()
    )
    .fetch_optional(exec)
    .await
}
