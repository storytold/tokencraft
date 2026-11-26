use super::{RunSummary, Scope};
use ids::WorkflowRunId;

pub async fn list_runs<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
) -> sqlx::Result<Vec<RunSummary>> {
    sqlx::query_as!(RunSummary,
        r#"SELECT id AS "id: WorkflowRunId", status, version, draft_revision, created_at FROM workflow_runs
        WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3 ORDER BY created_at DESC, id LIMIT 50"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str()
    )
    .fetch_all(exec)
    .await
}
