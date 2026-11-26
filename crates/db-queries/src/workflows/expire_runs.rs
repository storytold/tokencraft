pub async fn expire_runs<'e>(exec: impl sqlx::PgExecutor<'e>) -> sqlx::Result<()> {
    sqlx::query!(
        r#"WITH due AS (
            SELECT id FROM workflow_runs
            WHERE status IN ('queued', 'running', 'reconciling') AND deadline <= statement_timestamp()
            ORDER BY deadline, id
            LIMIT 64 FOR UPDATE SKIP LOCKED
        )
        UPDATE workflow_runs AS run
        SET status = 'expired', lease = run.lease + 1, revision = run.revision + 1,
            next_at = run.deadline, updated_at = statement_timestamp()
        FROM due WHERE run.id = due.id"#,
    )
    .execute(exec)
    .await
    .map(|_| ())
}
