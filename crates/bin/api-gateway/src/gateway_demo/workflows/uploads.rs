use axum::{body::{to_bytes, Body}, http::HeaderMap};
use chrono::{DateTime, Utc};
use db_queries::workflows as db;
use ids::WorkflowUploadId;
use serde::Serialize;

use super::super::handlers;
use super::graph::{Graph, InputValues, Value};
use super::runner::{self, Runtime};
use super::tool_execution::{matches_type, sha, MAX_ARTIFACT};
use super::tool_plugins::upload_kind;
use super::Security;
use crate::error::{ApiError, ApiResult};

const CHECKSUM: &str = "x-tokencraft-content-sha256";
const MAX_FILES: i64 = 32;
const MAX_BYTES: i64 = 128 * 1024 * 1024;

#[derive(Serialize)]
pub(super) struct Receipt {
    upload_id: WorkflowUploadId,
    expires_at: DateTime<Utc>,
}

pub(super) struct Pending {
    content_type: String,
    sha256: String,
    bytes: Vec<u8>,
}

fn label(key: &db::UploadKey<'_>, content_type: &str, sha256: &str) -> String {
    let db::Scope { account, project, workflow } = key.scope;
    format!("tool-upload/{account}/{project}/{workflow}/{}/{}/{content_type}/{sha256}", key.actor, key.id)
}

pub(super) async fn receive(headers: &HeaderMap, body: Body) -> ApiResult<Pending> {
    let invalid = || ApiError::BadRequest("upload requires a supported media type and matching bytes, size and SHA-256".into());
    let content_type = handlers::single_header(headers, "content-type")?
        .filter(|mime| upload_kind(mime).is_some())
        .ok_or_else(invalid)?;
    let length = handlers::single_header(headers, "content-length")?
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|size| (1..=MAX_ARTIFACT).contains(size))
        .ok_or_else(invalid)?;
    let checksum = handlers::single_header(headers, CHECKSUM)?
        .filter(|value| value.len() == 64
            && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        .ok_or_else(invalid)?;
    let bytes = tokio::time::timeout(std::time::Duration::from_secs(30), to_bytes(body, length))
        .await.map_err(|_| invalid())?.map_err(|_| invalid())?;
    if bytes.len() != length || sha(&bytes) != checksum || !matches_type(content_type, &bytes) {
        return Err(invalid());
    }
    Ok(Pending { content_type: content_type.into(), sha256: checksum.into(), bytes: bytes.to_vec() })
}

impl Pending {
    pub(super) fn compatible(&self, graph: &Graph) -> bool {
        upload_kind(&self.content_type)
            .is_some_and(|source| graph.inputs.values().any(|field| field.kind.accepts(source)))
    }

    pub(super) async fn store(
        self, tx: &mut sqlx::PgConnection, security: &Security, scope: db::Scope<'_>, actor: &str,
    ) -> ApiResult<Receipt> {
        // The caller holds the workflow lock while admitting new uploads against this quota.
        let size_bytes = self.bytes.len() as i32;
        if let Some((upload_id, expires_at)) = db::prior_upload(
            &mut *tx, scope, actor, &self.content_type, &self.sha256, size_bytes,
        ).await? {
            return Ok(Receipt { upload_id, expires_at });
        }
        let (count, bytes) = db::upload_quota(&mut *tx, scope).await?;
        if count >= MAX_FILES || bytes + i64::from(size_bytes) > MAX_BYTES {
            return Err(ApiError::ActiveRequestLimit);
        }
        let upload_id = WorkflowUploadId::generate();
        let key = db::UploadKey { scope, actor, id: &upload_id };
        let ciphertext = security.payloads
            .seal(&label(&key, &self.content_type, &self.sha256), self.bytes)
            .ok_or_else(runner::unavailable)?;
        let expires_at = db::insert_upload(tx, db::Upload {
            key, content_type: &self.content_type, sha256: &self.sha256, size_bytes, ciphertext: &ciphertext,
        }).await?;
        Ok(Receipt { upload_id, expires_at })
    }
}

pub(super) async fn validate(
    tx: &mut sqlx::PgConnection, scope: db::Scope<'_>, actor: &str, graph: &Graph, inputs: &InputValues,
) -> ApiResult<()> {
    for (name, value) in inputs {
        let Value::Upload(reference) = value else { continue; };
        let kind = graph.inputs[name].kind;
        let key = db::UploadKey { scope, actor, id: &reference.upload_id };
        let valid = db::eligible_upload(&mut *tx, key).await?
            .and_then(|content_type| upload_kind(&content_type)).is_some_and(|source| kind.accepts(source));
        if !valid {
            return Err(ApiError::BadRequest(format!("Upload a usable file for {name} before submitting this run.")));
        }
    }
    Ok(())
}

pub(super) async fn read(
    runtime: &Runtime, row: &db::Run, id: &WorkflowUploadId,
) -> ApiResult<(Vec<u8>, String, String)> {
    let scope = db::Scope { account: &row.account_id, project: &row.project_id, workflow: &row.workflow_id };
    let key = db::UploadKey { scope, actor: &row.actor_id, id };
    let artifact = db::read_upload(&runtime.pool, key).await?
        .ok_or_else(|| ApiError::NotFound("workflow upload has expired or is unavailable".into()))?;
    let bytes = runtime.security.payloads
        .open(&label(&key, &artifact.content_type, &artifact.sha256), &artifact.ciphertext)
        .ok_or_else(runner::unavailable)?;
    if sha(&bytes) != artifact.sha256 {
        return Err(runner::unavailable());
    }
    Ok((bytes, artifact.content_type, artifact.sha256))
}
