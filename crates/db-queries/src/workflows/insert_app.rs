use super::Scope;
use ids::WorkflowAppId;

pub struct NewApp<'a> {
    pub scope: Scope<'a>,
    pub id: &'a WorkflowAppId,
    pub name: &'a str,
    pub scopes: &'a [String],
    pub actor: &'a str,
}

pub async fn insert_app<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: NewApp<'_>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"INSERT INTO workflow_apps (account_id, project_id, workflow_id, id, name, scopes, created_by)
        VALUES ($1,$2,$3,$4,$5,$6,$7)"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.id.as_str(), args.name, args.scopes, args.actor
    )
    .execute(exec)
    .await.map(|_| ())
}
