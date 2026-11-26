use super::Scope;
use ids::WorkflowRunId;

pub struct NewRun<'a> {
    pub scope: Scope<'a>,
    pub id: &'a WorkflowRunId,
    pub actor: &'a str,
    pub version: Option<i32>,
    pub draft_revision: i64,
    pub idempotency_digest: &'a str,
    pub intent_digest: &'a str,
    pub graph: &'a serde_json::Value,
    pub content: &'a [u8],
}

pub async fn create_run<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: NewRun<'_>,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"INSERT INTO workflow_runs (account_id, project_id, workflow_id, id, actor_id, version, draft_revision,
        idempotency_digest, intent_digest, graph, content) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)"#,
        args.scope.account.as_str(), args.scope.project.as_str(), args.scope.workflow.as_str(),
        args.id.as_str(), args.actor, args.version, args.draft_revision, args.idempotency_digest, args.intent_digest, args.graph, args.content
    )
    .execute(exec)
    .await.map(|_| ())
}
