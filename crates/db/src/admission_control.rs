use sqlx::{PgConnection, PgExecutor, PgPool, Result};

/// A standby can expose a replayed value after the primary has changed state.
pub async fn primary_pause_state<'e>(executor: impl PgExecutor<'e>) -> Result<Option<bool>> {
    sqlx::query_scalar(
        "SELECT CASE WHEN NOT pg_is_in_recovery() THEN paused END
         FROM public.gateway_admission_control WHERE singleton",
    )
    .fetch_one(executor)
    .await
}

/// Hold the shared table lock until the admission transaction ends. The read
/// gets a fresh snapshot in the caller's READ COMMITTED transaction.
pub async fn lock_and_read_pause(conn: &mut PgConnection) -> Result<bool> {
    sqlx::query("LOCK TABLE public.gateway_admission_control IN ACCESS SHARE MODE")
        .execute(&mut *conn)
        .await?;
    sqlx::query_scalar("SELECT paused FROM public.gateway_admission_control WHERE singleton")
        .fetch_one(conn)
        .await
}

pub async fn set_paused(pool: &PgPool, paused: bool) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("LOCK TABLE public.gateway_admission_control IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    sqlx::query_scalar::<_, bool>("UPDATE public.gateway_admission_control SET paused = $1 WHERE singleton RETURNING paused")
        .bind(paused)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await
}
