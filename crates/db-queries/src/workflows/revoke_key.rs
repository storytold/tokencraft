use super::Scope;
use ids::WorkflowKeyId;

pub async fn revoke_key<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
    key: &WorkflowKeyId,
) -> sqlx::Result<bool> {
    sqlx::query!(
        r#"UPDATE workflow_keys SET revoked_at = COALESCE(revoked_at, now()) WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3 AND id = $4"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(), key.as_str()
    )
    .execute(exec)
    .await.map(|result| result.rows_affected() > 0)
}
