use super::{Scope, Workflow};
use ids::WorkflowId;

pub struct NewWorkflow<'a> {
    pub scope: Scope<'a>,
    pub title: &'a str,
    pub draft: &'a serde_json::Value,
    pub actor: &'a str,
}

pub async fn create_workflow<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: NewWorkflow<'_>,
) -> sqlx::Result<Workflow> {
    sqlx::query_as!(Workflow,
        r#"INSERT INTO workflows (account_id, project_id, id, title, draft, created_by) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id AS "id: WorkflowId", title, draft, revision, active_version, paused"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.title, args.draft, args.actor
    )
    .fetch_one(exec)
    .await
}
