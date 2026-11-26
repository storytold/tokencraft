use ids::WorkflowRunId;

pub struct Checkpoint<'a> {
    pub id: &'a WorkflowRunId,
    pub lease: i64,
    pub status: &'a str,
    pub content: &'a [u8],
    pub delay_seconds: i32,
}

pub async fn checkpoint_run<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    args: Checkpoint<'_>,
) -> sqlx::Result<bool> {
    sqlx::query!(
        r#"UPDATE workflow_runs SET status = $3, content = $4, revision = revision + 1, updated_at = clock_timestamp(),
         next_at = least(deadline, clock_timestamp() + $5::int * interval '1 second')
        WHERE id = $1 AND lease = $2 AND status IN ('queued','running','reconciling')
          AND next_at > clock_timestamp() AND deadline > clock_timestamp()"#,
        args.id.as_str(), args.lease, args.status, args.content, args.delay_seconds
    )
    .execute(exec)
    .await.map(|result| result.rows_affected() > 0)
}
