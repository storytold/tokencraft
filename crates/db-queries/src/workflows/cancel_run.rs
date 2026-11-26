use super::Scope;
use ids::WorkflowRunId;

pub struct CancelRun<'a> {
    pub scope: Scope<'a>,
    pub id: &'a WorkflowRunId,
    pub actor: Option<&'a str>,
}

pub async fn cancel_run<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: CancelRun<'_>,
) -> sqlx::Result<bool> {
    sqlx::query!(
        r#"UPDATE workflow_runs SET status = 'canceled', lease = lease + 1, revision = revision + 1, updated_at = now()
        WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3 AND id = $4 AND ($5::text IS NULL OR actor_id = $5)
        AND status IN ('queued','running','reconciling')"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.id.as_str(), args.actor
    )
    .execute(exec)
    .await.map(|result| result.rows_affected() > 0)
}
