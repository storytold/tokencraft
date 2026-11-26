use super::Scope;
use ids::WorkflowAppId;

pub struct ActiveRuns {
    pub workflow: i64,
    pub application: i64,
}

/// Count up to the workflow cap. Once the workflow is full, its app count is immaterial.
pub async fn count_active_runs<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: Scope<'_>,
    application: Option<&WorkflowAppId>,
    workflow_cap: i64,
) -> sqlx::Result<ActiveRuns> {
    sqlx::query_as!(
        ActiveRuns,
        r#"SELECT count(*) AS "workflow!",
                  count(*) FILTER (WHERE version IS NOT NULL AND actor_id = $4) AS "application!"
           FROM (
               SELECT version, actor_id FROM workflow_runs
               WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3
                 AND status IN ('queued', 'running', 'reconciling')
               LIMIT $5
           ) AS active"#,
        scope.account.as_str(),
        scope.project.as_str(),
        scope.workflow.as_str(),
        application.map(WorkflowAppId::as_str),
        workflow_cap,
    )
    .fetch_one(exec)
    .await
}
