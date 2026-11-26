use super::Scope;

pub async fn next_version<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
) -> sqlx::Result<i32> {
    sqlx::query_scalar!(
        r#"SELECT COALESCE(max(version), 0) + 1 AS "next!" FROM workflow_versions WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str()
    )
    .fetch_one(exec)
    .await
}
