use super::{Scope, Version};

pub async fn find_version<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
    version: i32,
) -> sqlx::Result<Option<Version>> {
    sqlx::query_as!(Version,
        r#"SELECT version, draft_revision, graph, published_at FROM workflow_versions WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3 AND version = $4"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(), version
    )
    .fetch_optional(exec)
    .await
}
