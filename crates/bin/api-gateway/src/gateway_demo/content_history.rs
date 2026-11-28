pub(crate) mod restore_job;
mod storage;

use std::{io, sync::Arc, time::Duration};

use axum::{
    extract::{Extension, Path as RequestPath, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use db_queries::gateway_requests::{artifacts as artifact_rows, content_history as records, content_restores};
use ids::GatewayRequestId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use tokio::sync::Semaphore;

use restore_job::RestoreProgress;
use storage::Target;

use super::{
    access::{self, GatewayCaller, Permission, ScopeQuery},
    archive_blobs::{BAD_OBJECT, COLD_OBJECT, IO_ERROR},
    artifacts::{self, MediaResult},
    catalog::digest,
    handlers::parse_request_id,
    providers::ProviderRuntime,
    readiness::Readiness,
    RequestErrorResponse,
};
use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

const MAGIC: &[u8; 4] = b"TCH1";
const MAX_INPUT: usize = 16_384;
const MAX_OUTPUT: usize = 262_144;
const MAX_BUNDLE: usize = 8 + MAX_INPUT + MAX_OUTPUT;
const ARTIFACT_SLOTS: std::ops::Range<i16> = 0..8;

pub(super) struct Store {
    primary: Arc<Target>,
    previous: Option<Arc<Target>>,
    pub(super) retain_media: bool,
    slots: Semaphore,
}

impl Store {
    pub(super) fn load() -> anyhow::Result<Option<Self>> {
        let content_dir = std::env::var_os("TOKENCRAFT_CONTENT_ARCHIVE_DIR");
        let retain_media = content_dir.is_some();
        let directory = content_dir
            .or_else(|| std::env::var_os("TOKENCRAFT_CHAT_ARCHIVE_DIR"))
            .map(|path| Target::directory(path.as_ref()))
            .transpose()?;
        let bucket = Target::bucket()?;
        let retain_media = retain_media || bucket.is_some();
        let (primary, previous) = match (bucket, directory) {
            (Some(bucket), directory) => (bucket, directory),
            (None, Some(directory)) => (directory, None),
            (None, None) => return Ok(None),
        };
        Ok(Some(Self {
            primary: Arc::new(primary),
            previous: previous.map(Arc::new),
            retain_media,
            slots: Semaphore::new(4),
        }))
    }

    pub(super) fn target(&self) -> &str {
        self.primary.id()
    }

    fn resolve(&self, target: &str) -> Result<&Target, &'static str> {
        std::iter::once(self.primary.as_ref())
            .chain(self.previous.as_deref())
            .find(|candidate| candidate.id() == target)
            .ok_or(IO_ERROR)
    }

    fn permits_restore(&self, target: &str) -> bool {
        self.resolve(target).is_ok_and(Target::permits_restore)
    }

    pub(super) fn spawn(self: Arc<Self>, pool: db::PgPool, runtime: Arc<ProviderRuntime>, readiness: Arc<Readiness>) {
        if self.primary.permits_restore() {
            restore_job::spawn(pool.clone(), Arc::clone(&self), Arc::clone(&readiness));
        }
        // Each configured target has its own bounded work loop and retry cadence.
        for target in std::iter::once(&self.primary).chain(&self.previous) {
            let (target, pool, runtime, readiness) = (
                Arc::clone(target), pool.clone(), Arc::clone(&runtime), Arc::clone(&readiness),
            );
            tokio::spawn(async move {
                let mut timer = tokio::time::interval(Duration::from_secs(2));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    readiness.work_tick(&mut timer).await;
                    if let Err(error) = Self::work(&pool, &runtime, &target).await {
                        tracing::warn!(
                            target = target.id(),
                            sqlstate = ?error.as_database_error().and_then(|e| e.code()),
                            "content archive database operation deferred"
                        );
                    }
                }
            });
        }
    }

    async fn work(
        pool: &db::PgPool,
        runtime: &ProviderRuntime,
        target: &Target,
    ) -> sqlx::Result<()> {
        let mut tx = pool.begin().await?;
        if let Some(job) = records::claim_purge(&mut tx, target.id()).await? {
            let deleted = if let Ok(id) = request_id(&job.request_id) {
                let mut names = vec![format!("{id}.tch")];
                if job.media {
                    names.extend(ARTIFACT_SLOTS.map(|slot| artifact_name(&id, slot)));
                }
                target.purge(names).await.is_ok()
            } else {
                false
            };
            if deleted {
                records::purged(&mut tx, &job.request_id).await?;
            } else {
                records::defer(&mut tx, &job.request_id).await?;
                tracing::warn!("content archive removal deferred");
            }
        } else if let Some(job) = records::claim_archive(&mut tx, target.id()).await? {
            // The request lock serializes sealing with administrator deletion and expiry.
            if let Some((manifest, files)) = Self::seal(target, pool, runtime, &mut tx, &job).await? {
                for (slot, checksum) in files {
                    artifact_rows::mark_archived(&mut tx, &job.request_id, slot, &checksum).await?;
                }
                records::archived(&mut tx, &job.request_id, &manifest).await?;
            } else {
                records::defer(&mut tx, &job.request_id).await?;
                tracing::warn!("content archive seal deferred");
            }
        }
        tx.commit().await
    }

    async fn seal(
        target: &Target,
        pool: &db::PgPool,
        runtime: &ProviderRuntime,
        tx: &mut Transaction<'_, Postgres>,
        job: &records::DueArchive,
    ) -> sqlx::Result<Option<(String, Vec<(i16, String)>)>> {
        let Ok(id) = request_id(&job.request_id) else {
            return Ok(None);
        };
        let mut checksums = Vec::new();
        if job.media {
            for slot in artifact_rows::archive_slots(tx, id.as_str()).await? {
                let mut row = artifact_rows::archive_source(tx, id.as_str(), slot).await?;
                let Ok(payload) =
                    artifacts::warm_payload(pool, &id, runtime.artifact_storage.as_deref(), &mut row)
                        .await
                else {
                    return Ok(None);
                };
                if artifacts::verify_payload(
                    runtime,
                    &id,
                    slot,
                    &row.content_type,
                    row.byte_length,
                    &row.sha256,
                    &payload,
                )
                .is_none()
                {
                    return Ok(None);
                }
                let limit = row.byte_length as usize + 28;
                let Some(checksum) = target
                    .seal(artifact_name(&id, slot), payload, limit)
                    .await
                else {
                    return Ok(None);
                };
                checksums.push((slot, checksum));
            }
        }
        let Ok(data) = pack(&job.input, job.output.as_deref()) else {
            return Ok(None);
        };
        Ok(target
            .seal(format!("{id}.tch"), data, MAX_BUNDLE)
            .await
            .map(|manifest| (manifest, checksums)))
    }

    async fn read(&self, target: &str, name: String, limit: usize) -> Result<Vec<u8>, &'static str> {
        let _slot = self.slots.try_acquire().map_err(|_| IO_ERROR)?;
        self.resolve(target)?.read(&name, limit).await
    }

    pub(super) async fn read_artifact(
        &self,
        target: &str,
        id: &GatewayRequestId,
        slot: i16,
        size: usize,
    ) -> Result<Vec<u8>, &'static str> {
        self.read(target, artifact_name(id, slot), size).await
    }
}

pub(super) fn archive_error(error: &'static str) -> ApiError {
    match error {
        COLD_OBJECT => ApiError::ArchiveRetrievalRequired(
            "Ask your storage administrator to restore this object, then try again.".into(),
        ),
        BAD_OBJECT => ApiError::Internal("retained content failed integrity verification".into()),
        _ => ApiError::ServiceUnavailable("retained content storage is unavailable".into()),
    }
}

fn request_id(raw: &str) -> io::Result<GatewayRequestId> {
    raw.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid content archive request ID",
        )
    })
}

fn artifact_name(id: &GatewayRequestId, slot: i16) -> String {
    format!("{id}-{slot}.tcm")
}

fn pack(input: &[u8], output: Option<&[u8]>) -> io::Result<Vec<u8>> {
    let output = output.unwrap_or_default();
    if input.is_empty() || input.len() > MAX_INPUT || output.len() > MAX_OUTPUT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content archive manifest exceeds bounds",
        ));
    }
    let mut bytes = Vec::with_capacity(8 + input.len() + output.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(input.len() as u32).to_be_bytes());
    bytes.extend_from_slice(input);
    bytes.extend_from_slice(output);
    Ok(bytes)
}

fn unpack(bytes: &[u8]) -> Option<(&[u8], Option<&[u8]>)> {
    if bytes.len() > MAX_BUNDLE || !bytes.starts_with(MAGIC) {
        return None;
    }
    let len = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
    if !(1..=MAX_INPUT).contains(&len) {
        return None;
    }
    let (input, output) = bytes.get(8..)?.split_at_checked(len)?;
    (output.len() <= MAX_OUTPUT).then_some((input, (!output.is_empty()).then_some(output)))
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct ContentHistory {
    #[serde(skip_serializing_if = "is_false")]
    media: bool,
    phase: String,
    archive_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "is_false")]
    can_restore: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    restore: Option<RestoreProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(crate) struct HistoryQuery {
    account_id: Option<String>,
    project_id: Option<String>,
    /// Return the phase and deadlines without the input or result.
    #[serde(default)]
    metadata_only: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Metadata,
    Warm,
    Archive,
}

#[utoipa::path(
    get, path = "/v1/requests/{id}/content", operation_id = "getRetainedChatContent", tag = "native",
    summary = "Read retained content and deadlines",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Request ID"), HistoryQuery),
    responses((status = 200, body = ContentHistory), (status = "default", response = RequestErrorResponse)),
)]
pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    caller: GatewayCaller,
    Query(query): Query<HistoryQuery>,
    RequestPath(raw): RequestPath<String>,
) -> ApiResult<Json<ContentHistory>> {
    let scope = ScopeQuery {
        account_id: query.account_id,
        project_id: query.project_id,
    };
    let view = if query.metadata_only {
        View::Metadata
    } else {
        View::Warm
    };
    read(state, runtime, caller, scope, raw, view).await
}

#[utoipa::path(
    post, path = "/v1/requests/{id}/content/retrieve", operation_id = "retrieveRetainedChatContent", tag = "native",
    summary = "Retrieve retained content from the archive",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Request ID"), ScopeQuery),
    responses((status = 200, body = ContentHistory),
              (status = 409, description = "Object requires storage-side retrieval", body = crate::error::GatewayError),
              (status = "default", response = RequestErrorResponse)),
)]
pub(crate) async fn retrieve(
    State(state): State<AppState>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    RequestPath(raw): RequestPath<String>,
) -> ApiResult<Json<ContentHistory>> {
    read(state, runtime, caller, scope, raw, View::Archive).await
}

async fn read(
    state: AppState,
    runtime: Option<Arc<ProviderRuntime>>,
    caller: GatewayCaller,
    scope: ScopeQuery,
    raw: String,
    requested: View,
) -> ApiResult<Json<ContentHistory>> {
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let id = parse_request_id(&raw)?;
    let not_found = || ApiError::NotFound("request content is unavailable".into());
    let unavailable =
        || ApiError::ServiceUnavailable("retained content storage is unavailable".into());
    let invalid = || ApiError::Internal("retained content failed integrity verification".into());
    let row = records::read(&state.pool, access.scope(), &id, requested == View::Warm)
        .await?
        .ok_or_else(not_found)?;
    let mut view = ContentHistory {
        media: row.media,
        phase: row.phase,
        archive_at: row.archive_at,
        expires_at: row.expires_at,
        can_restore: false,
        restore: None,
        input: None,
        result: None,
    };
    if view.phase == "archived" && row.target.starts_with("s3_") {
        view.restore = content_restores::progress(&state.pool, &id).await?.map(Into::into);
        view.can_restore = access.can_restore_content
            && runtime.as_ref().and_then(|service| service.content_archive.as_ref())
                .is_some_and(|store| store.permits_restore(&row.target));
    }
    if requested == View::Metadata || (requested == View::Warm && view.phase != "available") {
        return Ok(Json(view));
    }
    let runtime = runtime.as_deref().ok_or_else(unavailable)?;
    let decode = |field, payload: Option<&[u8]>| -> ApiResult<Option<Value>> {
        payload
            .map(|bytes| {
                let json = runtime
                    .payloads
                    .open(id.as_str(), field, bytes)
                    .ok_or_else(invalid)?;
                serde_json::from_slice(&json).map_err(|_| invalid())
            })
            .transpose()
    };
    if requested == View::Archive {
        let checksum = row
            .digest
            .filter(|_| view.phase == "archived")
            .ok_or_else(not_found)?;
        let store = runtime.content_archive.as_ref().ok_or_else(unavailable)?;
        let bytes = store.read(&row.target, format!("{id}.tch"), MAX_BUNDLE).await;
        let current = records::read(&state.pool, access.scope(), &id, false)
            .await?
            .ok_or_else(not_found)?;
        if current.phase != "archived" || current.digest.as_deref() != Some(checksum.as_str()) {
            return Err(not_found());
        }
        let bytes = bytes.map_err(archive_error)?;
        if digest(&bytes) != checksum {
            return Err(invalid());
        }
        let (input, output) = unpack(&bytes).ok_or_else(invalid)?;
        view.input = decode("input", Some(input))?;
        view.result = decode("result", output)?;
        if let Some(output) = view.result.as_mut().filter(|_| view.media) {
            let mut media: MediaResult =
                serde_json::from_value(output.take()).map_err(|_| invalid())?;
            media.archived_urls(&id);
            *output = serde_json::to_value(media).map_err(|_| invalid())?;
        }
    } else {
        view.input = decode("input", row.input.as_deref())?;
        view.result = decode("result", row.output.as_deref())?;
    }
    Ok(Json(view))
}

fn is_false(value: &bool) -> bool {
    !value
}
