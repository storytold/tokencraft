use super::Scope;

pub async fn has_passed_test<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
    revision: i64,
) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3
        AND draft_revision = $4 AND version IS NULL AND status = 'succeeded') AS "passed!" "#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(), revision
    )
    .fetch_one(exec)
    .await
}
