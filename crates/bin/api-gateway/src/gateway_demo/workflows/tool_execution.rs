use std::collections::BTreeMap;

use axum::{http::header, response::{IntoResponse, Response}};
use chrono::{DateTime, Utc};
use db_queries::workflows as db;
use reqwest::{Method, StatusCode};
use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};

use super::graph::{Binding as Edge, Node, Output, Value};
use super::runner::{self, Runtime, State};
use super::tool_plugins::{Binding, Tool};
use super::{uploads, Security};
use crate::error::{ApiError, ApiResult};
use crate::gateway_demo::artifacts::{self, ScopedDownload};

pub(super) const MAX_ARTIFACT: usize = 32 * 1024 * 1024;
const MAX_INPUT_SUM: usize = 64 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
pub(super) struct Progress {
    handle: String,
    pub status: String,
    pub admitted_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub(super) struct StepStatus {
    request_id: String,
    status: String,
    admitted_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl Progress {
    pub(super) fn into_status(self, run: &ids::WorkflowRunId, node: &str) -> StepStatus {
        StepStatus {
            request_id: request_id(run, node),
            status: self.status,
            admitted_at: self.admitted_at,
            finished_at: self.finished_at,
        }
    }
}

#[derive(Deserialize)]
struct Lookup {
    version: u8,
    request_id: String,
    handle: String,
}

#[derive(Deserialize)]
struct Receipt {
    version: u8,
    request_id: String,
    outcome: Outcome,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Accepted { handle: String },
    Rejected,
    Unavailable,
    Uncertain,
}

#[derive(Deserialize)]
struct Status {
    version: u8,
    handle: String,
    status: String,
}

#[derive(Serialize)]
struct Input {
    size_bytes: usize,
    sha256: String,
}

fn retry() -> ApiError {
    ApiError::ServiceUnavailable("tool runner is unavailable".into())
}

fn valid_handle(value: &str) -> bool {
    (1..=128).contains(&value.len()) &&
        value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn sha(bytes: &[u8]) -> String {
    hex::encode(digest(&SHA256, bytes).as_ref())
}

async fn body(mut response: reqwest::Response, limit: usize) -> ApiResult<Vec<u8>> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(retry());
    }
    let mut bytes = Vec::new();
    while let Some(part) = response.chunk().await.map_err(|_| retry())? {
        if part.len() > limit - bytes.len() {
            return Err(retry());
        }
        bytes.extend_from_slice(&part);
    }
    Ok(bytes)
}

async fn json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> ApiResult<T> {
    if response.status() != StatusCode::OK {
        return Err(retry());
    }
    serde_json::from_slice(&body(response, 16 * 1024).await?).map_err(|_| retry())
}

async fn lookup(binding: &Binding, id: &str) -> ApiResult<Option<String>> {
    let response = binding.request(Method::GET, &format!("/v1/jobs/by-request/{id}"))
        .send().await.map_err(|_| retry())?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let receipt: Lookup = json(response).await?;
    if receipt.version != 1 || receipt.request_id != id || !valid_handle(&receipt.handle) {
        return Err(retry());
    }
    Ok(Some(receipt.handle))
}

fn artifact_binding(run: &db::Run, node: &str, mime: &str, sha256: &str) -> String {
    format!("tool-artifact/{}/{}/{}/{node}/{mime}/{sha256}", run.account_id, run.project_id, run.id)
}

pub(super) async fn read_artifact(
    pool: &sqlx::PgPool, security: &Security, row: &db::Run, node: &str,
) -> ApiResult<(Vec<u8>, String, String)> {
    let artifact = db::read_tool_artifact(pool, &row.id, node).await?
        .ok_or_else(|| ApiError::NotFound("workflow tool artifact is unavailable".into()))?;
    let tag = artifact_binding(row, node, &artifact.content_type, &artifact.sha256);
    let bytes = security.payloads.open(&tag, &artifact.ciphertext).ok_or_else(runner::unavailable)?;
    if sha(&bytes) != artifact.sha256 {
        return Err(runner::unavailable());
    }
    Ok((bytes, artifact.content_type, artifact.sha256))
}

pub(super) async fn download(
    pool: &sqlx::PgPool, security: &Security, row: &db::Run, node: &str,
) -> ApiResult<Response> {
    let (bytes, content_type, sha256) = read_artifact(pool, security, row, node).await?;
    Ok(([
        (header::CONTENT_TYPE.as_str(), content_type),
        (header::CONTENT_DISPOSITION.as_str(), "attachment".into()),
        (header::CACHE_CONTROL.as_str(), "private, no-store".into()),
        ("x-content-type-options", "nosniff".into()),
        ("x-tokencraft-content-sha256", sha256),
    ], bytes).into_response())
}

async fn stage(
    runtime: &Runtime, row: &db::Run, state: &State, node: &Node, tool: &Tool, binding: &Binding, id: &str,
) -> ApiResult<BTreeMap<String, Input>> {
    let mut inputs = BTreeMap::new();
    let mut total = 0;
    for (slot, edge) in &node.settings {
        let missing = || ApiError::BadRequest(format!("tool input {slot} has no usable media"));
        let (bytes, mime, sha256) = match edge {
            Edge::Step { step } => match state.outputs.get(step) {
                Some(Output::Image { request_id } | Output::Video { request_id }) => {
                    artifacts::read_verified(ScopedDownload {
                        pool: &runtime.pool, scope: runner::scope(row), runtime: runtime.providers.as_deref(),
                        request: request_id, slot: 0,
                    }).await?
                }
                Some(Output::Tool { node, .. }) => read_artifact(&runtime.pool, &runtime.security, row, node).await?,
                _ => return Err(missing()),
            },
            Edge::Request { field } => match state.inputs.get(field) {
                Some(Value::Upload(reference)) => uploads::read(runtime, row, &reference.upload_id).await?,
                _ => return Err(missing()),
            },
            _ => return Err(missing()),
        };
        total += bytes.len();
        if !tool.accepts_source(slot, &mime) || bytes.is_empty() || bytes.len() > MAX_ARTIFACT || total > MAX_INPUT_SUM {
            return Err(missing());
        }
        let size_bytes = bytes.len();
        let response = binding.request(Method::PUT, &format!("/v1/jobs/{id}/inputs/{slot}"))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header("x-tokencraft-content-sha256", &sha256)
            .body(bytes).send().await.map_err(|_| retry())?;
        match response.status() {
            StatusCode::NO_CONTENT => {}
            StatusCode::BAD_REQUEST | StatusCode::CONFLICT => return Err(missing()),
            _ => return Err(retry()),
        }
        inputs.insert(slot.clone(), Input { size_bytes, sha256 });
    }
    Ok(inputs)
}

fn request_id(run: &ids::WorkflowRunId, node: &str) -> String {
    format!("step_{}", sha(runner::step_key(run, node).as_bytes()))
}

async fn admit(runtime: &Runtime, row: &db::Run, state: &State, node: &Node, binding: &Binding, tool: &Tool) -> ApiResult<String> {
    let id = request_id(&row.id, &node.id);
    if let Some(handle) = lookup(binding, &id).await? {
        return Ok(handle);
    }
    let call = node.tool.as_ref().ok_or_else(runner::unavailable)?;
    let inputs = stage(runtime, row, state, node, tool, binding, &id).await?;
    // A cancel or a newer lease can win while bytes are staged. Check under the same row lock
    // that cancellation uses before allowing the runner to accept this new operation.
    let mut tx = runtime.pool.begin().await?;
    if db::lock_active_run(&mut *tx, &row.id, row.lease).await?.is_none() {
        return Err(ApiError::Conflict("workflow tool lease ended".into()));
    }
    let response = binding.request(Method::POST, "/v1/jobs")
        .header("idempotency-key", &id)
        .json(&serde_json::json!({
            "version": 1, "request_id": id, "model": node.model,
            "input": {"kind": "tool", "revision": call.revision, "inputs": inputs, "parameters": call.parameters},
        }))
        .send().await.map_err(|_| retry())?;
    tx.commit().await?;
    let receipt: Receipt = json(response).await?;
    if receipt.version != 1 || receipt.request_id != id {
        return Err(retry());
    }
    match receipt.outcome {
        Outcome::Accepted { handle } if valid_handle(&handle) => Ok(handle),
        Outcome::Rejected => Err(ApiError::BadRequest("tool runner rejected the step".into())),
        _ => Err(retry()),
    }
}

async fn fetch(binding: &Binding, tool: &Tool, handle: &str) -> ApiResult<(Vec<u8>, String, String)> {
    let response = binding.request(Method::GET, &format!("/v1/jobs/{handle}/artifacts/0"))
        .send().await.map_err(|_| retry())?;
    if response.status() != StatusCode::OK {
        return Err(retry());
    }
    let read_header = |name| response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
    let mime = read_header("content-type").filter(|mime| tool.accepts_result(mime))
        .ok_or_else(|| ApiError::BadRequest("tool returned an undeclared result type".into()))?;
    let checksum = read_header("x-tokencraft-content-sha256").ok_or_else(retry)?;
    let length = response.content_length();
    let bytes = body(response, MAX_ARTIFACT).await?;
    if bytes.is_empty() || length != Some(bytes.len() as u64) || sha(&bytes) != checksum || !matches_type(&mime, &bytes) {
        return Err(ApiError::BadRequest("tool result bytes failed verification".into()));
    }
    Ok((bytes, mime, checksum))
}

pub(super) fn matches_type(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "video/mp4" | "video/quicktime" => bytes.get(4..8) == Some(b"ftyp"),
        "video/webm" => bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        "audio/wav" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE"),
        "audio/mpeg" => bytes.starts_with(b"ID3") || bytes.get(0..2).is_some_and(|start| start[0] == 255 && start[1] & 224 == 224),
        "audio/flac" => bytes.starts_with(b"fLaC"),
        "audio/ogg" => bytes.starts_with(b"OggS"),
        "model/gltf-binary" => bytes.starts_with(b"glTF"),
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "application/json" => serde_json::from_slice::<serde_json::Value>(bytes).is_ok(),
        "text/plain" | "text/vtt" => std::str::from_utf8(bytes).is_ok(),
        _ => false,
    }
}

pub(super) async fn advance(runtime: &Runtime, row: &db::Run, node: &Node, state: &mut State) -> ApiResult<Option<db::ToolArtifact>> {
    let (binding, tool) = runtime.security.tools.select(node, &row.account_id, &row.project_id, true)?;
    let Some(progress) = state.tools.get_mut(&node.id) else {
        let handle = admit(runtime, row, state, node, binding, tool).await?;
        state.tools.insert(node.id.clone(), Progress {handle, status: "running".into(), admitted_at: Utc::now(), finished_at: None});
        return Ok(None);
    };
    if !valid_handle(&progress.handle) {
        return Err(runner::unavailable());
    }
    let response = binding.request(Method::GET, &format!("/v1/jobs/{}", progress.handle))
        .send().await.map_err(|_| retry())?;
    let status: Status = json(response).await?;
    if status.version != 1 || status.handle != progress.handle {
        return Err(retry());
    }
    match status.status.as_str() {
        "queued" | "running" => {
            progress.status = status.status;
            return Ok(None);
        }
        "failed" => return Err(ApiError::BadRequest("tool operation failed".into())),
        "succeeded" => {}
        _ => return Err(retry()),
    }
    let (bytes, content_type, sha256) = fetch(binding, tool, &progress.handle).await?;
    let tag = artifact_binding(row, &node.id, &content_type, &sha256);
    let ciphertext = runtime.security.payloads.seal(&tag, bytes).ok_or_else(runner::unavailable)?;
    progress.status = "succeeded".into();
    progress.finished_at = Some(Utc::now());
    state.outputs.insert(node.id.clone(), Output::Tool { node: node.id.clone(), kind: node.output() });
    Ok(Some(db::ToolArtifact { content_type, sha256, ciphertext }))
}
