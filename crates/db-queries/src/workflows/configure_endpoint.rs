use super::{Scope, Workflow};
use ids::WorkflowId;

pub struct EndpointChange<'a> {
    pub scope: Scope<'a>,
    pub version: Option<i32>,
    pub paused: bool,
}

pub async fn configure_endpoint<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: EndpointChange<'_>,
) -> sqlx::Result<Workflow> {
    sqlx::query_as!(Workflow,
        r#"UPDATE workflows SET active_version = $4, paused = $5, updated_at = now()
        WHERE account_id = $1 AND project_id = $2 AND id = $3 RETURNING id AS "id: WorkflowId", title, draft, revision, active_version, paused"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.version, args.paused
    )
    .fetch_one(exec)
    .await
}
