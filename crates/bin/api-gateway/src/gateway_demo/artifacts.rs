use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{
    header::{CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS},
    HeaderMap, HeaderValue,
};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use db_queries::gateway_requests::artifacts::{self as db_artifacts, ArtifactData, NewArtifact};
use ids::GatewayRequestId;
use serde::{Deserialize, Serialize};

use super::access::{self, GatewayCaller, Permission, ScopeQuery};
use super::archive_blobs::{BAD_OBJECT, IO_ERROR};
use super::artifact_storage::ArtifactStorage;
use super::catalog::digest;
use super::handlers::parse_request_id;
use super::providers::ProviderRuntime;
use super::sealed::SealedBytes;
use super::SHA256_HEADER;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(super) const PNG: &str = "image/png";
pub(super) const MP4: &str = "video/mp4";
pub(super) const WAV: &str = "audio/wav";
pub(super) const GLB: &str = "model/gltf-binary";
pub(super) const MAX_BYTES: usize = 2 * 1024 * 1024;
pub(super) const MAX_VIDEO_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_MESH_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct FetchedImage {
    pub slot: i16,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

pub(super) struct FetchedVideo {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    pub bytes: Vec<u8>,
}

pub(super) struct FetchedAudio {
    pub duration_ms: u32,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub bytes: Vec<u8>,
}

pub(super) struct FetchedMesh {
    pub mesh_count: usize,
    pub primitive_count: usize,
    pub bytes: Vec<u8>,
}

pub(super) enum FetchedMedia {
    Images(Vec<FetchedImage>),
    Video(FetchedVideo),
    Audio(FetchedAudio),
    Mesh(FetchedMesh),
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum MediaResult {
    Images { images: Vec<MediaOutput> },
    Video { video: MediaOutput },
    Audio { audio: AudioOutput },
    Mesh { mesh: MeshOutput },
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct MediaOutput {
    #[serde(flatten)]
    file: ArtifactOutput,
    width: u32,
    height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u32>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct AudioOutput {
    #[serde(flatten)]
    file: ArtifactOutput,
    duration_ms: u32,
    sample_rate_hz: u32,
    channels: u16,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct MeshOutput {
    #[serde(flatten)]
    file: ArtifactOutput,
    mesh_count: usize,
    primitive_count: usize,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
struct ArtifactOutput {
    url: String,
    content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<ArtifactRef>,
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
struct ArtifactRef {
    slot: i16,
    byte_length: i32,
    sha256: String,
}

fn binding(request: &str, slot: i16, content_type: &str, sha256: &str) -> String {
    format!("{request}/{slot}/{content_type}/{sha256}")
}

pub(super) async fn prepare(
    pool: &db::PgPool,
    storage: Option<&ArtifactStorage>,
    cipher: &SealedBytes,
    request: &GatewayRequestId,
    media: FetchedMedia,
) -> Option<(MediaResult, Vec<NewArtifact>)> {
    match media {
        FetchedMedia::Images(images) => {
            let mut outputs = Vec::with_capacity(images.len());
            let mut stored = Vec::with_capacity(images.len());
            for image in images {
                let (file, row) =
                    store_file(pool, storage, cipher, request, image.slot, PNG, image.bytes).await?;
                outputs.push(MediaOutput {
                    file,
                    width: image.width,
                    height: image.height,
                    duration_ms: None,
                });
                stored.push(row);
            }
            Some((MediaResult::Images { images: outputs }, stored))
        }
        FetchedMedia::Video(video) => {
            let (file, row) = store_file(pool, storage, cipher, request, 0, MP4, video.bytes).await?;
            let video = MediaOutput {
                file,
                width: video.width,
                height: video.height,
                duration_ms: Some(video.duration_ms),
            };
            Some((MediaResult::Video { video }, vec![row]))
        }
        FetchedMedia::Audio(audio) => {
            let (file, row) = store_file(pool, storage, cipher, request, 0, WAV, audio.bytes).await?;
            let audio = AudioOutput {
                file,
                duration_ms: audio.duration_ms,
                sample_rate_hz: audio.sample_rate_hz,
                channels: audio.channels,
            };
            Some((MediaResult::Audio { audio }, vec![row]))
        }
        FetchedMedia::Mesh(mesh) => {
            let (file, row) = store_file(pool, storage, cipher, request, 0, GLB, mesh.bytes).await?;
            let mesh = MeshOutput {
                file,
                mesh_count: mesh.mesh_count,
                primitive_count: mesh.primitive_count,
            };
            Some((MediaResult::Mesh { mesh }, vec![row]))
        }
    }
}

async fn store_file(
    pool: &db::PgPool,
    storage: Option<&ArtifactStorage>,
    cipher: &SealedBytes,
    request: &GatewayRequestId,
    slot: i16,
    content_type: &'static str,
    bytes: Vec<u8>,
) -> Option<(ArtifactOutput, NewArtifact)> {
    let sha256 = digest(&bytes);
    let byte_length = i32::try_from(bytes.len()).ok()?;
    let payload = cipher.seal(
        &binding(request.as_str(), slot, content_type, &sha256),
        bytes,
    )?;
    let data = match storage {
        Some(storage) => match storage.write(pool, request, payload).await {
            Ok(data) => data,
            Err(_) => {
                tracing::warn!("artifact storage write deferred");
                return None;
            }
        },
        None => ArtifactData::Inline(payload),
    };
    let file = ArtifactOutput {
        url: format!("/v1/requests/{request}/artifacts/{slot}"),
        content_type: content_type.into(),
        artifact: Some(ArtifactRef {
            slot,
            byte_length,
            sha256: sha256.clone(),
        }),
    };
    Some((
        file,
        NewArtifact {
            slot,
            content_type,
            byte_length,
            sha256,
            data,
        },
    ))
}

struct ArtifactBytes;

impl utoipa::PartialSchema for ArtifactBytes {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, SchemaFormat, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}

impl utoipa::ToSchema for ArtifactBytes {}

#[utoipa::path(
    get, path = "/v1/requests/{id}/artifacts/{slot}", operation_id = "downloadGatewayRequestArtifact", tag = "native",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Durable request ID."), ("slot" = i16, Path, description = "Zero-based artifact slot."), ScopeQuery),
    responses(
        (status = 200, description = "Verified artifact bytes.",
            content(
                (ArtifactBytes = "image/png"),
                (ArtifactBytes = "video/mp4"),
                (ArtifactBytes = "audio/wav"),
                (ArtifactBytes = "model/gltf-binary"),
            ),
            headers(
                ("Content-Disposition" = String, description = "Suggested download filename."),
                ("X-TokenCraft-Content-Sha256" = String, description = "SHA-256 hash of the returned bytes."),
            )
        ),
        (status = 404, description = "Artifact unavailable, expired, removed, or outside scope.", body = crate::error::GatewayError),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub(super) async fn download(
    State(state): State<AppState>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path((raw, slot)): Path<(String, i16)>,
) -> ApiResult<Response> {
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let request = parse_request_id(&raw)?;
    download_for_scope(ScopedDownload {
        pool: &state.pool,
        scope: access.scope(),
        runtime: runtime.as_deref(),
        request: &request,
        slot,
    }).await
}

pub(super) struct ScopedDownload<'a> {
    pub pool: &'a db::PgPool,
    pub scope: db_queries::gateway_requests::RequestScope<'a>,
    pub runtime: Option<&'a ProviderRuntime>,
    pub request: &'a GatewayRequestId,
    pub slot: i16,
}

pub(super) async fn download_for_scope(input: ScopedDownload<'_>) -> ApiResult<Response> {
    let (bytes, content_type, sha256) = read_verified(input).await?;
    respond(content_type, sha256, bytes)
}

pub(super) async fn read_verified(input: ScopedDownload<'_>) -> ApiResult<(Vec<u8>, String, String)> {
    let ScopedDownload { pool, scope, runtime, request, slot } = input;
    let missing = || ApiError::NotFound("request artifact is unavailable or expired".into());
    let runtime = runtime.ok_or_else(missing)?;
    let mut artifact = db_artifacts::read(pool, scope, request, slot, true)
        .await?
        .ok_or_else(missing)?;
    let invalid = || ApiError::Internal("could not verify stored artifact".into());
    let plain = warm_payload(pool, request, runtime.artifact_storage.as_deref(), &mut artifact)
        .await
        .map_err(|code| if code == BAD_OBJECT {
            invalid()
        } else {
            ApiError::ServiceUnavailable("stored artifact is unavailable".into())
        })
        .and_then(|payload| {
            verify_payload(
                runtime, request, slot, &artifact.content_type, artifact.byte_length, &artifact.sha256, &payload,
            ).ok_or_else(invalid)
        });
    // The storage read can overlap expiry or an administrator removing content.
    if db_artifacts::read(pool, scope, request, slot, false).await?.is_none() {
        return Err(missing());
    }
    Ok((plain?, artifact.content_type, artifact.sha256))
}

pub(super) async fn warm_payload(
    pool: &db::PgPool,
    request: &GatewayRequestId,
    storage: Option<&ArtifactStorage>,
    artifact: &mut db_artifacts::Artifact,
) -> Result<Vec<u8>, &'static str> {
    let size = artifact.byte_length as usize + 28;
    match (artifact.payload.take(), &artifact.object_key, &artifact.bucket_key, &artifact.bucket_target) {
        (Some(payload), None, None, None) => Ok(payload),
        (None, Some(key), None, None) => storage.ok_or(BAD_OBJECT)?.read_file(pool, request, key, size).await,
        (None, None, Some(key), Some(target)) => storage.ok_or(IO_ERROR)?.read_bucket(pool, request, key, target, size).await,
        _ => Err(BAD_OBJECT),
    }
}

pub(super) fn verify_payload(
    runtime: &ProviderRuntime,
    request: &GatewayRequestId,
    slot: i16,
    content_type: &str,
    byte_length: i32,
    sha256: &str,
    payload: &[u8],
) -> Option<Vec<u8>> {
    runtime
        .payloads
        .media
        .open(
            &binding(request.as_str(), slot, content_type, sha256),
            payload,
        )
        .filter(|bytes| bytes.len() == byte_length as usize && digest(bytes) == sha256)
}

#[utoipa::path(
    get, path = "/v1/requests/{id}/content/artifacts/{slot}", operation_id = "downloadRetainedArtifact", tag = "native",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Request ID"), ("slot" = i16, Path, description = "Artifact slot"), ScopeQuery),
    responses((status = 200, description = "Verified archived media", body = ArtifactBytes),
              (status = 409, description = "Object requires storage-side retrieval", body = crate::error::GatewayError),
              (status = "default", response = super::RequestErrorResponse)),
)]
pub(crate) async fn download_archived(
    State(state): State<AppState>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path((raw, slot)): Path<(String, i16)>,
) -> ApiResult<Response> {
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let request = parse_request_id(&raw)?;
    let missing = || ApiError::NotFound("archived artifact is unavailable or expired".into());
    let invalid = || ApiError::Internal("could not verify archived artifact".into());
    let runtime = runtime.ok_or_else(missing)?;
    let record = db_artifacts::read_archived(&state.pool, access.scope(), &request, slot)
        .await?
        .ok_or_else(missing)?;
    let store = runtime.content_archive.as_ref().ok_or_else(|| {
        ApiError::ServiceUnavailable("retained content storage is unavailable".into())
    })?;
    let plain = store
        .read_artifact(&record.target, &request, slot, record.byte_length as usize + 28)
        .await
        .map_err(super::content_history::archive_error)
        .and_then(|payload| {
            if digest(&payload) != record.archive_sha256 {
                return Err(invalid());
            }
            verify_payload(
                &runtime,
                &request,
                slot,
                &record.content_type,
                record.byte_length,
                &record.sha256,
                &payload,
            )
            .ok_or_else(invalid)
        });
    if db_artifacts::read_archived(&state.pool, access.scope(), &request, slot)
        .await?
        .is_none()
    {
        return Err(missing());
    }
    respond(record.content_type, record.sha256, plain?)
}

fn respond(content_type: String, sha256: String, plain: Vec<u8>) -> ApiResult<Response> {
    let invalid = || ApiError::Internal("could not verify stored artifact".into());
    let mut headers = HeaderMap::new();
    let (content_type, disposition) = match content_type.as_str() {
        PNG => (PNG, "attachment; filename=\"tokencraft-image.png\""),
        MP4 => (MP4, "attachment; filename=\"tokencraft-video.mp4\""),
        WAV => (WAV, "attachment; filename=\"tokencraft-audio.wav\""),
        GLB => (GLB, "attachment; filename=\"tokencraft-mesh.glb\""),
        _ => return Err(invalid()),
    };
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(CONTENT_DISPOSITION, HeaderValue::from_static(disposition));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        SHA256_HEADER,
        HeaderValue::from_str(&sha256).map_err(|_| invalid())?,
    );
    Ok((headers, Bytes::from(plain)).into_response())
}

impl MediaResult {
    pub(super) fn archived_urls(&mut self, request: &GatewayRequestId) {
        match self {
            Self::Images { images } => images
                .iter_mut()
                .for_each(|image| image.file.archive_url(request)),
            Self::Video { video } => video.file.archive_url(request),
            Self::Audio { audio } => audio.file.archive_url(request),
            Self::Mesh { mesh } => mesh.file.archive_url(request),
        }
    }
}

impl ArtifactOutput {
    fn archive_url(&mut self, request: &GatewayRequestId) {
        if let Some(file) = &self.artifact {
            self.url = format!("/v1/requests/{request}/content/artifacts/{}", file.slot);
        }
    }
}
