use super::Summary;
use ids::{AccountId, GatewayProjectId, WorkflowId};

pub async fn list_workflows<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    account: &AccountId,
    project: &GatewayProjectId,
) -> sqlx::Result<Vec<Summary>> {
    sqlx::query_as!(Summary,
        r#"SELECT id AS "id: WorkflowId", title, revision, active_version, paused FROM workflows WHERE account_id = $1 AND project_id = $2 ORDER BY updated_at DESC, id LIMIT 100"#,
        account.as_str(), project.as_str()
    )
    .fetch_all(exec)
    .await
}
