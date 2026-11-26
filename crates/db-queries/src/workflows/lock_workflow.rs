use super::{Scope, Workflow};
use ids::WorkflowId;

pub async fn lock_workflow<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
) -> sqlx::Result<Option<Workflow>> {
    sqlx::query_as!(Workflow,
        r#"SELECT id AS "id: WorkflowId", title, draft, revision, active_version, paused FROM workflows WHERE account_id = $1 AND project_id = $2 AND id = $3 FOR UPDATE"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str()
    )
    .fetch_optional(exec)
    .await
}
