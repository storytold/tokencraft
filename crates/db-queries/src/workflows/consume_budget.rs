use ids::WorkflowRunId;

/// The caller holds the run's lease lock and inserts the child in the same transaction.
pub async fn consume_budget<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    run: &WorkflowRunId,
    estimate: i64,
) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE workflow_runs SET budget_remaining_usd_nanos = budget_remaining_usd_nanos - $2
         WHERE id = $1 AND budget_month_start IS NOT NULL AND budget_remaining_usd_nanos >= $2",
        run.as_str(),
        estimate,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() == 1)
}
