pub async fn forget_expired_content<'e, E: sqlx::PgExecutor<'e>>(exec: E) -> sqlx::Result<()> {
    sqlx::query!(
        r#"UPDATE workflow_runs SET content = NULL WHERE id IN (
        SELECT id FROM workflow_runs WHERE content IS NOT NULL AND status IN ('succeeded','failed','canceled','expired')
        AND updated_at < now() - interval '7 days' ORDER BY updated_at LIMIT 128 FOR UPDATE SKIP LOCKED)"#,

    )
    .execute(exec)
    .await.map(|_| ())
}
