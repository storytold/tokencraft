use chrono::{DateTime, Utc};
use ids::{GatewayArchiveId, GatewayExportId};
use serde::Serialize;
use sqlx::{FromRow, PgPool};

use crate::gateway_requests::RequestScope;

#[derive(Serialize, FromRow)]
pub struct Archive {
    pub id: String,
    pub project_id: String,
    pub client_ref: Option<String>,
    pub export_id: String,
    pub state: String,
    pub schema_version: i32,
    pub event_count: Option<i32>,
    pub first_event_id: Option<String>,
    pub last_event_id: Option<String>,
    pub has_more: Option<bool>,
    pub content_sha256: Option<String>,
    #[serde(skip)]
    pub object_sha256: Option<String>,
    #[serde(skip)]
    pub storage_generation: Option<i32>,
    #[serde(skip)]
    pub storage_target: String,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub stored_at: Option<DateTime<Utc>>,
    pub restored_until: Option<DateTime<Utc>>,
    pub restore_check_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub automatic: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_retry_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retirement_error_code: Option<String>,
}

pub struct NewArchive<'a> {
    pub id: &'a GatewayArchiveId,
    pub export: &'a GatewayExportId,
    pub actor_id: &'a str,
    pub after: i64,
    pub through: i64,
}

pub async fn create(
    pool: &PgPool,
    scope: RequestScope<'_>,
    job: NewArchive<'_>,
) -> sqlx::Result<GatewayArchiveId> {
    sqlx::query_scalar!(
        r#"INSERT INTO gateway_archives AS a (id, account_id, project_id, client_ref, export_id, requested_by, after_seq, through_seq)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         ON CONFLICT (export_id, after_seq) DO UPDATE SET
            state = CASE WHEN a.state = 'archive_failed' THEN 'queued' ELSE a.state END,
            tries = CASE WHEN a.state = 'archive_failed' THEN 0 ELSE a.tries END,
            error_code = CASE WHEN a.state = 'archive_failed' THEN NULL ELSE a.error_code END,
            automatic_retry_at = NULL
         RETURNING id AS "id: GatewayArchiveId""#,
        job.id.as_str(), scope.account.as_str(), scope.project.as_str(), scope.client_ref,
        job.export.as_str(), job.actor_id, job.after, job.through,
    )
    .fetch_one(pool)
    .await
}

pub async fn list(
    pool: &PgPool,
    scope: RequestScope<'_>,
    id: Option<&GatewayArchiveId>,
    before: Option<&GatewayArchiveId>,
    limit: i64,
) -> sqlx::Result<Vec<Archive>> {
    sqlx::query_as::<_, Archive>(
        "SELECT id, project_id, client_ref, export_id,
            CASE WHEN state = 'available' AND restored_until <= clock_timestamp() THEN 'stored'
                 WHEN state IN ('restore_queued','restoring') AND retrieval_attempted_at IS NOT NULL THEN 'retrieving'
                 ELSE state END AS state,
            schema_version, event_count, first_event_id, last_event_id, has_more, content_sha256,
            object_sha256, storage_generation, storage_target, error_code, created_at, stored_at,
            CASE WHEN restored_until > clock_timestamp() THEN restored_until END AS restored_until,
            restore_check_at, automatic, automatic_retry_at, retired_at, retirement_error_code
         FROM gateway_archives
         WHERE account_id = $1 AND project_id = $2 AND ($3::text IS NULL OR client_ref = $3)
            AND ($4::text IS NULL OR id = $4) AND ($5::text IS NULL OR id < $5)
         ORDER BY id DESC LIMIT $6",
    )
    .bind(scope.account.as_str())
    .bind(scope.project.as_str())
    .bind(scope.client_ref)
    .bind(id.map(GatewayArchiveId::as_str))
    .bind(before.map(GatewayArchiveId::as_str))
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn restore(
    pool: &PgPool,
    scope: RequestScope<'_>,
    archive: &GatewayArchiveId,
    retrieve_cold: bool,
) -> sqlx::Result<()> {
    // Cold retrieval can replace a verification-only restore. Clearing its lease
    // prevents that worker from publishing a stale outcome.
    sqlx::query!(
        "UPDATE gateway_archives SET state = 'restore_queued', tries = 0, restored_until = NULL, error_code = NULL,
                  lease_until = NULL, retrieve_cold = $5, retrieval_attempted_at = NULL, restore_check_at = NULL,
                  retrieval_confirmed = false, retrieval_submissions = 0
         WHERE account_id = $1 AND project_id = $2 AND ($3::text IS NULL OR client_ref = $3) AND id = $4
           AND (state IN ('stored','restore_failed')
                OR (state = 'available' AND restored_until <= clock_timestamp())
                OR ($5 AND NOT retrieve_cold AND state IN ('restore_queued','restoring')))",
        scope.account.as_str(), scope.project.as_str(), scope.client_ref, archive.as_str(), retrieve_cold,
    ).execute(pool).await?;
    Ok(())
}

pub struct ArchiveTask {
    pub id: String,
    pub account_id: String,
    pub project_id: String,
    pub client_ref: Option<String>,
    pub generation: i32,
    pub state: String,
    pub after_seq: i64,
    pub through_seq: i64,
    pub retrieve_cold: bool,
    pub retrieval_attempted_at: Option<DateTime<Utc>>,
    pub retrieval_confirmed: bool,
    pub retrieval_submissions: i16,
    pub automatic: bool,
}

pub async fn claim(pool: &PgPool, retry_automatic: bool) -> sqlx::Result<Option<ArchiveTask>> {
    sqlx::query!(
        "UPDATE gateway_archives SET
            state = CASE WHEN state = 'sealing' THEN 'archive_failed' ELSE 'restore_failed' END,
            lease_until = NULL, error_code = 'lease_expired',
            automatic_retry_at = CASE WHEN automatic AND state = 'sealing'
                THEN clock_timestamp() + interval '5 minutes' END
         WHERE state IN ('sealing','restoring') AND lease_until <= clock_timestamp() AND tries = 3",
    )
    .execute(pool)
    .await?;
    sqlx::query_as!(
        ArchiveTask,
        "WITH ready AS (
            SELECT id FROM gateway_archives
            WHERE state = 'queued' OR
                (state = 'restore_queued' AND (restore_check_at IS NULL OR restore_check_at <= clock_timestamp())) OR
                (state IN ('sealing','restoring') AND tries < 3 AND lease_until <= clock_timestamp()) OR
                ($1 AND automatic AND state = 'archive_failed' AND automatic_retry_at <= clock_timestamp())
            ORDER BY created_at, id LIMIT 1 FOR UPDATE SKIP LOCKED
         )
         UPDATE gateway_archives a SET
            state = CASE WHEN a.state IN ('queued','sealing','archive_failed') THEN 'sealing' ELSE 'restoring' END,
            generation = generation + 1,
            tries = CASE WHEN a.state = 'archive_failed' THEN 1 ELSE tries + 1 END,
            lease_until = clock_timestamp() + interval '45 seconds',
            restore_check_at = NULL, automatic_retry_at = NULL
         FROM ready WHERE a.id = ready.id
         RETURNING a.id, a.account_id, a.project_id, a.client_ref, a.generation, a.state, a.after_seq, a.through_seq,
                   a.retrieve_cold, a.retrieval_attempted_at, a.retrieval_confirmed, a.retrieval_submissions, a.automatic",
        retry_automatic,
    ).fetch_optional(pool).await
}

pub struct Manifest<'a> {
    pub event_count: i32,
    pub first_event_id: &'a str,
    pub last_event_id: &'a str,
    pub has_more: bool,
    pub content_sha256: &'a str,
    pub object_sha256: &'a str,
    pub storage_target: &'a str,
}

pub async fn store(
    pool: &PgPool,
    task: &ArchiveTask,
    manifest: Manifest<'_>,
) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "UPDATE gateway_archives SET state = 'stored', storage_generation = generation, lease_until = NULL, tries = 0,
           event_count = $3, first_event_id = $4, last_event_id = $5, has_more = $6,
           content_sha256 = $7, object_sha256 = $8, storage_target = $9,
           stored_at = clock_timestamp(), error_code = NULL
         WHERE id = $1 AND generation = $2 AND state = 'sealing' AND lease_until > clock_timestamp()",
        task.id, task.generation, manifest.event_count, manifest.first_event_id, manifest.last_event_id,
        manifest.has_more, manifest.content_sha256, manifest.object_sha256, manifest.storage_target,
    ).execute(pool).await?;
    Ok(done.rows_affected() == 1)
}

pub async fn restored(pool: &PgPool, task: &ArchiveTask) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE gateway_archives SET state = 'available', lease_until = NULL, tries = 0,
            restored_until = clock_timestamp() + interval '1 hour', error_code = NULL
         WHERE id = $1 AND generation = $2 AND state = 'restoring' AND lease_until > clock_timestamp()",
        task.id, task.generation,
    ).execute(pool).await?;
    Ok(())
}

pub const MAX_RETRIEVAL_SUBMISSIONS: i16 = 3;

pub async fn begin_retrieval(pool: &PgPool, task: &ArchiveTask) -> sqlx::Result<bool> {
    let updated = sqlx::query!(
        "UPDATE gateway_archives SET
            retrieval_submissions = greatest(retrieval_submissions, (retrieval_attempted_at IS NOT NULL)::int) + 1,
            retrieval_attempted_at = coalesce(retrieval_attempted_at, clock_timestamp()),
            lease_until = clock_timestamp() + interval '45 seconds'
         WHERE id = $1 AND generation = $2 AND state = 'restoring' AND lease_until > clock_timestamp()
           AND retrieve_cold AND NOT retrieval_confirmed AND retrieval_submissions < $3
           AND (retrieval_attempted_at IS NULL OR retrieval_attempted_at > statement_timestamp() - interval '48 hours')",
        task.id, task.generation, MAX_RETRIEVAL_SUBMISSIONS,
    ).execute(pool).await?;
    Ok(updated.rows_affected() == 1)
}

pub async fn wait_for_retrieval(pool: &PgPool, task: &ArchiveTask, confirmed: bool) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE gateway_archives SET
            state = CASE WHEN retrieval_attempted_at <= statement_timestamp() - interval '48 hours'
                         THEN 'restore_failed' ELSE 'restore_queued' END,
            lease_until = NULL, tries = 0, retrieval_confirmed = retrieval_confirmed OR $3,
            restore_check_at = CASE WHEN retrieval_attempted_at > statement_timestamp() - interval '48 hours'
                                    THEN statement_timestamp() + interval '1 minute' END,
            error_code = CASE WHEN retrieval_attempted_at <= statement_timestamp() - interval '48 hours'
                              THEN 'archive_retrieval_timed_out' END
         WHERE id = $1 AND generation = $2 AND state = 'restoring' AND lease_until > clock_timestamp()
           AND retrieval_attempted_at IS NOT NULL",
        task.id, task.generation, confirmed,
    ).execute(pool).await?;
    Ok(())
}

pub async fn fail(pool: &PgPool, task: &ArchiveTask, code: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE gateway_archives SET
           state = CASE WHEN state = 'sealing' THEN 'archive_failed' ELSE 'restore_failed' END,
           lease_until = NULL, error_code = $3,
           automatic_retry_at = CASE WHEN automatic AND state = 'sealing'
                THEN clock_timestamp() + interval '5 minutes' END
         WHERE id = $1 AND generation = $2 AND state IN ('sealing','restoring') AND lease_until > clock_timestamp()",
        task.id, task.generation, code,
    ).execute(pool).await?;
    Ok(())
}
