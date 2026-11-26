use super::{Scope, Workflow};
use ids::WorkflowId;

pub struct SaveDraft<'a> {
    pub scope: Scope<'a>,
    pub draft: &'a serde_json::Value,
    pub title: &'a str,
    pub revision: i64,
}

pub async fn save_draft<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: SaveDraft<'_>,
) -> sqlx::Result<Option<Workflow>> {
    sqlx::query_as!(Workflow,
        r#"UPDATE workflows SET draft = $4, title = $5, revision = revision + 1, updated_at = now()
        WHERE account_id = $1 AND project_id = $2 AND id = $3 AND revision = $6 RETURNING id AS "id: WorkflowId", title, draft, revision, active_version, paused"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.draft, args.title, args.revision
    )
    .fetch_optional(exec)
    .await
}
