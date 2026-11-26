use chrono::{DateTime, Utc};
use ids::GatewayRequestId;
use sqlx::{PgExecutor, PgPool};

pub async fn insert<'e>(
    exec: impl PgExecutor<'e>,
    request: &GatewayRequestId,
    encrypted_png: &[u8],
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO gateway_provider_workflow_frames (request_id, encrypted_png) VALUES ($1, $2)")
        .bind(request.as_str())
        .bind(encrypted_png)
        .execute(exec)
        .await?;
    Ok(())
}

pub async fn read_for_submit(
    pool: &PgPool,
    request: &str,
    lease: DateTime<Utc>,
) -> sqlx::Result<Option<Vec<u8>>> {
    sqlx::query_scalar(
        "SELECT frame.encrypted_png
         FROM gateway_provider_workflow_frames frame
         JOIN gateway_provider_runs run ON run.request_id = frame.request_id
         WHERE frame.request_id = $1 AND run.lease_until = $2 AND run.lease_until > clock_timestamp()
           AND run.request_payload IS NOT NULL",
    )
    .bind(request)
    .bind(lease)
    .fetch_optional(pool)
    .await
}
