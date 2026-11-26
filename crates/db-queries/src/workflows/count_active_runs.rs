use super::Scope;

pub async fn count_active_runs<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "active!" FROM workflow_runs WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3
        AND status IN ('queued','running','reconciling')"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str()
    )
    .fetch_one(exec)
    .await
}
