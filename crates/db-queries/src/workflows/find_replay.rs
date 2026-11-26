use super::{Replay, Scope};
use ids::WorkflowRunId;

pub struct ReplayLookup<'a> {
    pub scope: Scope<'a>,
    pub actor: &'a str,
    pub digest: &'a str,
}

pub async fn find_replay<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: ReplayLookup<'_>,
) -> sqlx::Result<Option<Replay>> {
    sqlx::query_as!(Replay,
        r#"SELECT id AS "id: WorkflowRunId", version, intent_digest FROM workflow_runs WHERE account_id = $1 AND project_id = $2 AND workflow_id = $3
        AND actor_id = $4 AND idempotency_digest = $5"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(), args.actor, args.digest
    )
    .fetch_optional(exec)
    .await
}
