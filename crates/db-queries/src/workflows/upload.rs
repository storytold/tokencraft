use chrono::{DateTime, Utc};
use ids::WorkflowUploadId;

use super::Scope;

#[derive(Clone, Copy)]
pub struct UploadKey<'a> {
    pub scope: Scope<'a>,
    pub actor: &'a str,
    pub id: &'a WorkflowUploadId,
}

pub struct Upload<'a> {
    pub key: UploadKey<'a>,
    pub content_type: &'a str,
    pub sha256: &'a str,
    pub size_bytes: i32,
    pub ciphertext: &'a [u8],
}

pub async fn upload_quota<'e, E: sqlx::PgExecutor<'e>>(exec: E, scope: Scope<'_>) -> sqlx::Result<(i64, i64)> {
    let quota = sqlx::query!(
        r#"SELECT count(*) AS "count!", coalesce(sum(size_bytes),0)::bigint AS "bytes!"
        FROM workflow_uploads WHERE account_id=$1 AND project_id=$2 AND workflow_id=$3 AND expires_at>now()"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(),
    ).fetch_one(exec).await?;
    Ok((quota.count, quota.bytes))
}

pub async fn prior_upload<'e, E: sqlx::PgExecutor<'e>>(
    exec: E, scope: Scope<'_>, actor: &str, content_type: &str, sha256: &str, size_bytes: i32,
) -> sqlx::Result<Option<(WorkflowUploadId, DateTime<Utc>)>> {
    sqlx::query!(
        r#"SELECT id AS "id: WorkflowUploadId",expires_at FROM workflow_uploads
         WHERE account_id=$1 AND project_id=$2 AND workflow_id=$3 AND actor_id=$4
           AND content_type=$5 AND sha256=$6 AND size_bytes=$7 AND expires_at>=now() + interval '30 minutes'
         ORDER BY expires_at DESC LIMIT 1"#,
        scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(), actor, content_type, sha256, size_bytes,
    ).fetch_optional(exec).await.map(|row| row.map(|upload| (upload.id, upload.expires_at)))
}

pub async fn insert_upload<'e, E: sqlx::PgExecutor<'e>>(exec: E, upload: Upload<'_>) -> sqlx::Result<DateTime<Utc>> {
    let Upload { key: UploadKey { scope, actor, id }, content_type, sha256, size_bytes, ciphertext } = upload;
    sqlx::query_scalar!(
        "INSERT INTO workflow_uploads (id,account_id,project_id,workflow_id,actor_id,content_type,sha256,size_bytes,ciphertext)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING expires_at",
        id.as_str(), scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(),
        actor, content_type, sha256, size_bytes, ciphertext,
    ).fetch_one(exec).await
}

pub async fn eligible_upload<'e, E: sqlx::PgExecutor<'e>>(
    exec: E, key: UploadKey<'_>,
) -> sqlx::Result<Option<String>> {
    let UploadKey { scope, actor, id } = key;
    sqlx::query!(
        "SELECT content_type FROM workflow_uploads
         WHERE id=$1 AND account_id=$2 AND project_id=$3 AND workflow_id=$4 AND actor_id=$5
           AND expires_at>=now() + interval '30 minutes'",
        id.as_str(), scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(), actor,
    ).fetch_optional(exec).await.map(|row| row.map(|row| row.content_type))
}

pub async fn read_upload<'e, E: sqlx::PgExecutor<'e>>(
    exec: E, key: UploadKey<'_>,
) -> sqlx::Result<Option<super::ToolArtifact>> {
    let UploadKey { scope, actor, id } = key;
    sqlx::query_as!(super::ToolArtifact,
        "SELECT content_type,sha256,ciphertext FROM workflow_uploads
         WHERE id=$1 AND account_id=$2 AND project_id=$3 AND workflow_id=$4 AND actor_id=$5
           AND expires_at>now()",
        id.as_str(), scope.account.as_str(), scope.project.as_str(), scope.workflow.as_str(), actor,
    ).fetch_optional(exec).await
}

pub async fn forget_expired_uploads<'e, E: sqlx::PgExecutor<'e>>(exec: E) -> sqlx::Result<()> {
    sqlx::query!(
        "DELETE FROM workflow_uploads WHERE id IN (
            SELECT id FROM workflow_uploads WHERE expires_at<=now()
            ORDER BY expires_at,id LIMIT 128 FOR UPDATE SKIP LOCKED
        )",
    ).execute(exec).await.map(|_| ())
}
