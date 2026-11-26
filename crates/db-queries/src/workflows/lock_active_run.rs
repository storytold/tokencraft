use ids::WorkflowRunId;

pub async fn lock_active_run<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    run: &WorkflowRunId,
    lease: i64,
) -> sqlx::Result<Option<WorkflowRunId>> {
    sqlx::query_scalar!(
        r#"SELECT id AS "id: WorkflowRunId" FROM workflow_runs WHERE id = $1 AND lease = $2
        AND status IN ('queued','running','reconciling') AND deadline > now() FOR UPDATE"#,
        run.as_str(),
        lease
    )
    .fetch_optional(exec)
    .await
}
