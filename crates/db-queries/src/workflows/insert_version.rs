use super::Scope;

pub struct NewVersion<'a> {
    pub scope: Scope<'a>,
    pub version: i32,
    pub revision: i64,
    pub graph: &'a serde_json::Value,
    pub actor: &'a str,
}

pub async fn insert_version<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: NewVersion<'_>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"INSERT INTO workflow_versions (account_id, project_id, workflow_id, version, draft_revision, graph, published_by)
        VALUES ($1,$2,$3,$4,$5,$6,$7)"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.version, args.revision, args.graph, args.actor
    )
    .execute(exec)
    .await.map(|_| ())
}
