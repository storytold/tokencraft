use super::{AppKey, Scope};
use ids::{WorkflowAppId, WorkflowKeyId};

pub async fn list_keys<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
) -> sqlx::Result<Vec<AppKey>> {
    sqlx::query_as!(AppKey,
        r#"SELECT a.id AS "app_id: WorkflowAppId", a.name, a.scopes, k.id AS "key_id: WorkflowKeyId",
        k.prefix, k.expires_at, k.revoked_at FROM workflow_apps a JOIN workflow_keys k
        ON (a.account_id,a.project_id,a.workflow_id,a.id)=(k.account_id,k.project_id,k.workflow_id,k.app_id)
        WHERE a.account_id = $1 AND a.project_id = $2 AND a.workflow_id = $3 ORDER BY a.created_at, k.created_at DESC LIMIT 150"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str()
    )
    .fetch_all(exec)
    .await
}
