use super::Scope;
use chrono::{DateTime, Utc};
use ids::{WorkflowAppId, WorkflowKeyId};

pub struct NewKey<'a> {
    pub scope: Scope<'a>,
    pub app: &'a WorkflowAppId,
    pub id: &'a WorkflowKeyId,
    pub hash: &'a str,
    pub prefix: &'a str,
    pub days: i32,
    pub actor: &'a str,
}

pub async fn insert_key<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: NewKey<'_>,
) -> sqlx::Result<Option<DateTime<Utc>>> {
    sqlx::query_scalar!(
        r#"INSERT INTO workflow_keys (account_id, project_id, workflow_id, app_id, id, key_hash, prefix, expires_at, created_by)
        SELECT account_id, project_id, workflow_id, id, $5, $6, $7, now() + $8::int * interval '1 day', $9
        FROM workflow_apps WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3 AND id = $4 RETURNING expires_at"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.app.as_str(), args.id.as_str(),
        args.hash, args.prefix, args.days, args.actor
    )
    .fetch_optional(exec)
    .await
}
