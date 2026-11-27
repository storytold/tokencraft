pub(super) mod ndjson;
mod page;

use std::sync::Arc;

use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE},
    },
};
use db_queries::{
    gateway_exports::{self, ExportEvent, Snapshot},
    gateway_requests::RequestScope,
};
use ids::GatewayExportId;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    access::{self, GatewayCaller, Permission, ScopeQuery, parse_id},
    archive_blobs::ArchiveStorage,
    archive_events,
};
use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

pub(crate) const NEXT_AFTER_HEADER: &str = "x-tokencraft-next-after";
pub(crate) const HAS_MORE_HEADER: &str = "x-tokencraft-has-more";
pub(crate) const SHA256_HEADER: &str = "x-tokencraft-content-sha256";
pub(crate) const FEED_AS_OF_HEADER: &str = "x-tokencraft-feed-as-of";
pub(crate) const PUBLICATION_PENDING_HEADER: &str = "x-tokencraft-publication-pending";
pub(crate) const EXPORT_PROJECT_HEADER: &str = "x-tokencraft-project";
pub(crate) const EXPORT_CLIENT_HEADER: &str = "x-tokencraft-client-ref";
pub(crate) const EXPORT_VERSION_HEADER: &str = "x-tokencraft-schema-version";
pub(super) const SCHEMA_VERSION: u8 = 1;
pub(super) const MAX_EXPORTS_IN_FLIGHT: usize = 4;

#[derive(Clone)]
pub(super) struct Capacity(Arc<Semaphore>);

impl Capacity {
    pub(super) fn new(limit: usize) -> Self {
        Self(Arc::new(Semaphore::new(limit)))
    }

    pub(super) fn acquire(&self) -> ApiResult<OwnedSemaphorePermit> {
        Arc::clone(&self.0).try_acquire_owned().map_err(|_| ApiError::ExportCapacity)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Create {
    account_id: Option<String>,
    project_id: Option<String>,
    client_ref: Option<String>,
    after: Option<String>,
}

#[derive(Serialize)]
pub(super) struct Export {
    #[serde(flatten)]
    snapshot: Snapshot,
    format: &'static str,
    schema_version: u8,
}

impl From<Snapshot> for Export {
    fn from(snapshot: Snapshot) -> Self {
        Self {
            snapshot,
            format: "ndjson",
            schema_version: SCHEMA_VERSION,
        }
    }
}

pub(super) async fn create(
    State(state): State<AppState>,
    Extension(capacity): Extension<Capacity>,
    caller: GatewayCaller,
    Json(input): Json<Create>,
) -> ApiResult<(StatusCode, Json<Export>)> {
    let access = access::resolve(
        &state,
        caller,
        ScopeQuery {
            account_id: input.account_id,
            project_id: input.project_id,
        },
        Permission::ReadExports,
    )
    .await?;
    let scope = access.for_client(input.client_ref.as_deref())?;
    let after = cursor(input.after.as_deref())?;
    let _permit = capacity.acquire()?;
    gateway_exports::publish(&state.pool, Some(scope.project)).await?;
    let id = GatewayExportId::generate();
    let snapshot = gateway_exports::create(&state.pool, scope, &id, &access.actor_id, after)
        .await?
        .ok_or_else(missing_cursor)?;
    Ok((StatusCode::CREATED, Json(snapshot.into())))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListQuery {
    account_id: Option<String>,
    project_id: Option<String>,
    client_ref: Option<String>,
    before: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ExportPage {
    exports: Vec<Export>,
    next_before: Option<String>,
}

pub(super) async fn list(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(input): Query<ListQuery>,
) -> ApiResult<Json<ExportPage>> {
    let access = access::resolve(
        &state,
        caller,
        ScopeQuery {
            account_id: input.account_id,
            project_id: input.project_id,
        },
        Permission::ReadExports,
    )
    .await?;
    let scope = access.for_client(input.client_ref.as_deref())?;
    let before = input
        .before
        .as_deref()
        .map(|id| parse_id(id, "before"))
        .transpose()?;
    let mut snapshots =
        gateway_exports::snapshots(&state.pool, scope, None, before.as_ref(), 21).await?;
    let next_before = (snapshots.len() > 20).then(|| snapshots[19].id.clone());
    snapshots.truncate(20);
    Ok(Json(ExportPage {
        exports: snapshots.into_iter().map(Into::into).collect(),
        next_before,
    }))
}

pub(super) async fn detail(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(query): Query<ScopeQuery>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Export>> {
    let access = access::resolve(&state, caller, query, Permission::ReadExports).await?;
    let id = parse_id(&raw, "export_id")?;
    let snapshot = scoped_snapshot(&state.pool, access.scope(), &id).await?;
    Ok(Json(snapshot.into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DataQuery {
    account_id: Option<String>,
    project_id: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}

pub(super) async fn data(
    State(state): State<AppState>,
    Extension(storage): Extension<Option<Arc<ArchiveStorage>>>,
    Extension(capacity): Extension<Capacity>,
    caller: GatewayCaller,
    Query(query): Query<DataQuery>,
    Path(raw): Path<String>,
) -> ApiResult<(HeaderMap, Bytes)> {
    let access = access::resolve(
        &state,
        caller,
        ScopeQuery {
            account_id: query.account_id,
            project_id: query.project_id,
        },
        Permission::ReadExports,
    )
    .await?;
    let id = parse_id(&raw, "export_id")?;
    let scope = access.scope();
    let after = cursor(query.after.as_deref())?;
    let limit = page_limit(query.limit)?;
    let permit = capacity.acquire()?;
    let snapshot = scoped_snapshot(&state.pool, scope, &id).await?;
    let events = gateway_exports::events(&state.pool, scope, &snapshot, after, limit as i64 + 1)
        .await?
        .ok_or_else(missing_cursor)?;
    let (mut headers, body) =
        encode_page(&state.pool, storage.as_ref(), scope, events, limit, permit).await?;
    headers.insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"tokencraft-{id}.ndjson\""))
            .expect("export IDs are header-safe"),
    );
    Ok((headers, body))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(super) struct FeedQuery {
    account_id: Option<String>,
    project_id: Option<String>,
    /// Restrict a project-wide credential to one client.
    client_ref: Option<String>,
    /// The last consumed event ID in this scope.
    after: Option<String>,
    /// Defaults to 200 events. Maximum 500.
    limit: Option<u16>,
}

#[utoipa::path(
    get, path = "/v1/exports/events", operation_id = "pollExportEvents", tag = "native",
    summary = "Poll published export events",
    description = "Read versioned events in publication order without creating a snapshot. Requires exports:read or equivalent session access. Resume using the last consumed cursor within the same scope.",
    security(("project_api_key" = []), ("session_cookie" = [])),
    params(FeedQuery),
    responses(
        (status = 200, description = "Page of newline-delimited JSON events.", body = String, content_type = "application/x-ndjson",
         headers(
            ("X-TokenCraft-Content-Sha256" = String, description = "SHA-256 hex digest of the exact response bytes."),
            ("X-TokenCraft-Next-After" = String, description = "Last event ID. Omitted on an empty page."),
            ("X-TokenCraft-Has-More" = bool, description = "More published events exist after this page."),
            ("X-TokenCraft-Feed-As-Of" = String, description = "UTC timestamp of the observed feed."),
            ("X-TokenCraft-Publication-Pending" = bool, description = "The scope has events awaiting publication."),
            ("X-TokenCraft-Project" = String, description = "Effective project ID."),
            ("X-TokenCraft-Client-Ref" = String, description = "Effective client reference or an empty value for project-wide access."),
            ("X-TokenCraft-Schema-Version" = u8, description = "Event schema version."),
         )),
        (status = 503, description = "Export capacity or storage is temporarily unavailable. Resume with the same cursor.", body = crate::error::GatewayError,
         headers(("Retry-After" = u8, description = "Seconds to wait when the API replica's export slots are busy."))),
        (status = "default", response = super::RequestErrorResponse),
    )
)]
pub(super) async fn feed(
    State(state): State<AppState>,
    Extension(storage): Extension<Option<Arc<ArchiveStorage>>>,
    Extension(capacity): Extension<Capacity>,
    caller: GatewayCaller,
    Query(query): Query<FeedQuery>,
) -> ApiResult<(HeaderMap, Bytes)> {
    let access = access::resolve(
        &state,
        caller,
        ScopeQuery {
            account_id: query.account_id,
            project_id: query.project_id,
        },
        Permission::ReadExports,
    )
    .await?;
    let scope = access.for_client(query.client_ref.as_deref())?;
    let after = cursor(query.after.as_deref())?;
    let limit = page_limit(query.limit)?;
    let permit = capacity.acquire()?;
    let page = gateway_exports::feed_page(&state.pool, scope, after, limit as i64 + 1)
        .await?
        .ok_or_else(|| ApiError::NotFound("event cursor unavailable for this feed".into()))?;
    let (mut headers, body) =
        encode_page(&state.pool, storage.as_ref(), scope, page.events, limit, permit).await?;
    headers.insert(FEED_AS_OF_HEADER, header_value(&page.as_of.to_rfc3339()));
    headers.insert(
        PUBLICATION_PENDING_HEADER,
        bool_header(page.publication_pending),
    );
    headers.insert(EXPORT_PROJECT_HEADER, header_value(scope.project.as_str()));
    headers.insert(
        EXPORT_CLIENT_HEADER,
        header_value(scope.client_ref.unwrap_or_default()),
    );
    headers.insert(
        EXPORT_VERSION_HEADER,
        header_value(&SCHEMA_VERSION.to_string()),
    );
    Ok((headers, body))
}

fn page_limit(limit: Option<u16>) -> ApiResult<usize> {
    let limit = usize::from(limit.unwrap_or(200));
    if !(1..=500).contains(&limit) {
        return Err(ApiError::BadRequest("limit must be 1 to 500".into()));
    }
    Ok(limit)
}

fn header_value(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).expect("validated export metadata is header-safe")
}

fn bool_header(value: bool) -> HeaderValue {
    HeaderValue::from_static(if value { "true" } else { "false" })
}

async fn encode_page(
    pool: &db::PgPool,
    storage: Option<&Arc<ArchiveStorage>>,
    scope: RequestScope<'_>,
    mut events: Vec<ExportEvent>,
    limit: usize,
    permit: OwnedSemaphorePermit,
) -> ApiResult<(HeaderMap, Bytes)> {
    let has_more = archive_events::limit_page(&mut events, limit);
    let page = page::encode(pool, storage, scope, events, has_more, permit).await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(HAS_MORE_HEADER, bool_header(page.has_more));
    if let Some(last_id) = page.last_id {
        headers.insert(NEXT_AFTER_HEADER, header_value(&last_id));
    }
    headers.insert(SHA256_HEADER, header_value(&page.checksum));
    Ok((headers, page.body))
}

pub(super) async fn scoped_snapshot(
    pool: &db::PgPool,
    scope: RequestScope<'_>,
    id: &GatewayExportId,
) -> ApiResult<Snapshot> {
    gateway_exports::snapshots(pool, scope, Some(id), None, 1)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::NotFound("export unavailable or expired".into()))
}

pub(super) fn cursor(after: Option<&str>) -> ApiResult<Option<&str>> {
    if after.is_some_and(|cursor| {
        !cursor
            .strip_prefix("evt_")
            .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
    }) {
        return Err(ApiError::BadRequest("invalid event cursor".into()));
    }
    Ok(after)
}

fn missing_cursor() -> ApiError {
    ApiError::NotFound("event cursor unavailable for this export".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::response_bytes;

    #[test]
    fn response_bytes_retain_the_export_slot_until_the_last_view_is_released() {
        let capacity = Capacity::new(1);
        let bytes = response_bytes::from_vec(b"event".to_vec(), capacity.acquire().unwrap());
        let slice = bytes.slice(..1);
        drop(bytes);
        assert!(matches!(capacity.acquire(), Err(ApiError::ExportCapacity)));
        drop(slice);
        assert!(capacity.acquire().is_ok());
    }
}
