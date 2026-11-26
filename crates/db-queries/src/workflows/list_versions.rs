use super::{Scope, Version};

pub async fn list_versions<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as!(Version,
        r#"SELECT version, draft_revision, graph, published_at FROM workflow_versions WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3 ORDER BY version DESC LIMIT 100"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str()
    )
    .fetch_all(exec)
    .await
}
