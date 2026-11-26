use super::Credential;
use ids::{AccountId, GatewayProjectId, WorkflowAppId, WorkflowId};

pub async fn find_credential<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    hash: &str,
) -> sqlx::Result<Option<Credential>> {
    sqlx::query_as!(Credential,
        r#"SELECT k.account_id AS "account_id: AccountId", k.project_id AS "project_id: GatewayProjectId",
        k.workflow_id AS "workflow_id: WorkflowId", k.app_id AS "app_id: WorkflowAppId", a.scopes
        FROM workflow_keys k JOIN workflow_apps a ON (a.account_id,a.project_id,a.workflow_id,a.id) =
        (k.account_id,k.project_id,k.workflow_id,k.app_id) JOIN accounts ac ON ac.id=k.account_id
        WHERE k.key_hash = $1 AND k.revoked_at IS NULL AND k.expires_at > now() AND ac.status = 'active'"#,
        hash
    )
    .fetch_optional(exec)
    .await
}
