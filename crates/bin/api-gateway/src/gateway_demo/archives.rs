use std::{sync::Arc, time::Duration};

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
        HeaderMap, HeaderValue, StatusCode,
    },
    routing::{get, post},
    Extension, Json, Router,
};
use db_queries::{
    gateway_archive_schedule,
    gateway_archives::{self, Archive, ArchiveTask, Manifest, NewArchive},
    gateway_exports,
    gateway_requests::RequestScope,
};
use ids::{AccountId, GatewayArchiveId, GatewayProjectId};
use serde::{Deserialize, Serialize};

use super::{
    access::{self, parse_id, GatewayCaller, Permission, ScopeQuery},
    archive_blobs::{ArchiveStorage, Expected, ObjectKey, COLD_OBJECT, IO_ERROR},
    archive_bucket::RestoreDispatch,
    archive_events, archive_retirement,
    catalog::digest,
    exports,
    readiness::Readiness,
    response_bytes,
};
use crate::{
    error::{ApiError, ApiResult},
    middleware::Caller,
    state::AppState,
};

const PARTITION_EVENTS: i64 = 1000;
const MAX_DOWNLOADS_IN_FLIGHT: usize = 2;

#[derive(Clone)]
struct DownloadCapacity(exports::Capacity);

pub(super) fn router(storage: Arc<ArchiveStorage>) -> Router<AppState> {
    Router::new()
        .route("/v1/archives", get(list).post(create))
        .route("/v1/archives/:id", get(detail))
        .route("/v1/archives/:id/restore", post(restore))
        .route(
            "/v1/archives/:id/data",
            get(data).layer(Extension(DownloadCapacity(exports::Capacity::new(
                MAX_DOWNLOADS_IN_FLIGHT,
            )))),
        )
        .layer(Extension(storage))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    account_id: Option<String>,
    project_id: Option<String>,
    export_id: String,
    after: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Json(input): Json<Create>,
) -> ApiResult<(StatusCode, Json<Archive>)> {
    if !matches!(&caller, GatewayCaller::Account(Caller::Session(_))) {
        return Err(ApiError::Forbidden(
            "archiving requires a project administrator session".into(),
        ));
    }
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
    let export = parse_id(&input.export_id, "export_id")?;
    let scope = access.scope();
    let snapshot = exports::scoped_snapshot(&state.pool, scope, &export).await?;
    let after = exports::cursor(input.after.as_deref())?;
    let start = gateway_exports::resolve_cursor(&state.pool, scope, &snapshot, after)
        .await?
        .ok_or_else(|| ApiError::NotFound("event cursor unavailable for this export".into()))?;
    if start == snapshot.through_seq {
        return Err(ApiError::BadRequest(
            "the export contains no additional events".into(),
        ));
    }
    let id = GatewayArchiveId::generate();
    let archive_scope = RequestScope {
        client_ref: snapshot.client_ref.as_deref(),
        ..scope
    };
    let id = gateway_archives::create(
        &state.pool,
        archive_scope,
        NewArchive {
            id: &id,
            export: &export,
            actor_id: &access.actor_id,
            after: start,
            through: snapshot.through_seq,
        },
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(scoped_archive(&state.pool, scope, &id).await?),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    account_id: Option<String>,
    project_id: Option<String>,
    before: Option<String>,
}

#[derive(Serialize)]
struct ArchiveList {
    archives: Vec<ListedArchive>,
    next_before: Option<String>,
}

#[derive(Serialize)]
struct ListedArchive {
    #[serde(flatten)]
    archive: Archive,
    can_request_retrieval: bool,
}

async fn list(
    State(state): State<AppState>,
    Extension(storage): Extension<Arc<ArchiveStorage>>,
    caller: GatewayCaller,
    Query(input): Query<ListQuery>,
) -> ApiResult<Json<ArchiveList>> {
    let admin_session = matches!(&caller, GatewayCaller::Account(Caller::Session(_)));
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
    let before = input
        .before
        .as_deref()
        .map(|id| parse_id(id, "before"))
        .transpose()?;
    let mut archives =
        gateway_archives::list(&state.pool, access.scope(), None, before.as_ref(), 21).await?;
    let next_before = (archives.len() > 20).then(|| archives[19].id.clone());
    archives.truncate(20);
    Ok(Json(ArchiveList {
        archives: archives
            .into_iter()
            .map(|archive| ListedArchive {
                can_request_retrieval: admin_session
                    && storage.permits_retrieval(&archive.storage_target),
                archive,
            })
            .collect(),
        next_before,
    }))
}

async fn detail(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(query): Query<ScopeQuery>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Archive>> {
    let access = access::resolve(&state, caller, query, Permission::ReadExports).await?;
    let id = parse_id(&raw, "archive_id")?;
    Ok(Json(
        scoped_archive(&state.pool, access.scope(), &id).await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreQuery {
    account_id: Option<String>,
    project_id: Option<String>,
    #[serde(default)]
    retrieve: bool,
}

async fn restore(
    State(state): State<AppState>,
    Extension(storage): Extension<Arc<ArchiveStorage>>,
    caller: GatewayCaller,
    headers: HeaderMap,
    Query(input): Query<RestoreQuery>,
    Path(raw): Path<String>,
) -> ApiResult<(StatusCode, Json<Archive>)> {
    if input.retrieve {
        if !matches!(&caller, GatewayCaller::Account(Caller::Session(_))) {
            return Err(ApiError::Forbidden(
                "cold retrieval requires a project administrator session".into(),
            ));
        }
        // Cross-origin browser requests must preflight this billable action.
        if headers.get(CONTENT_TYPE) != Some(&HeaderValue::from_static("application/json")) {
            return Err(ApiError::BadRequest(
                "cold retrieval requires Content-Type application/json".into(),
            ));
        }
    }
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
    let id = parse_id(&raw, "archive_id")?;
    let scope = access.scope();
    let archive = scoped_archive(&state.pool, scope, &id).await?;
    if archive.stored_at.is_none() {
        return Err(ApiError::Conflict("archive has not been sealed".into()));
    }
    if input.retrieve && !storage.permits_retrieval(&archive.storage_target) {
        return Err(ApiError::Conflict(
            "cold retrieval is disabled for this archive storage target".into(),
        ));
    }
    gateway_archives::restore(&state.pool, scope, &id, input.retrieve).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(scoped_archive(&state.pool, scope, &id).await?),
    ))
}

async fn data(
    State(state): State<AppState>,
    Extension(storage): Extension<Arc<ArchiveStorage>>,
    Extension(DownloadCapacity(capacity)): Extension<DownloadCapacity>,
    caller: GatewayCaller,
    Query(query): Query<ScopeQuery>,
    Path(raw): Path<String>,
) -> ApiResult<(HeaderMap, Bytes)> {
    let access = access::resolve(&state, caller, query, Permission::ReadExports).await?;
    let id = parse_id(&raw, "archive_id")?;
    let scope = access.scope();
    let archive = scoped_archive(&state.pool, scope, &id).await?;
    let unavailable = || ApiError::Conflict("restore the archive to download it".into());
    let until = archive
        .restored_until
        .filter(|_| archive.state == "available")
        .ok_or_else(unavailable)?;
    let expected = Expected::from_manifest(scope.account, &archive)
        .ok_or_else(|| ApiError::Internal("archive is missing integrity metadata".into()))?;
    let checksum = expected.content_sha256.clone();
    let permit = capacity.acquire()?;
    let body = storage.read(expected).await.map_err(|code| {
        if code == COLD_OBJECT {
            ApiError::Conflict(
                "retrieve the cold object in storage before downloading this archive".into(),
            )
        } else {
            ApiError::Internal(code.into())
        }
    })?;
    if until <= chrono::Utc::now() {
        return Err(unavailable());
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"tokencraft-{id}.ndjson\""))
            .expect("archive IDs are header-safe"),
    );
    headers.insert(
        exports::SHA256_HEADER,
        HeaderValue::from_str(&checksum).expect("verified SHA-256 hex is header-safe"),
    );
    Ok((headers, response_bytes::from_vec(body, permit)))
}

async fn scoped_archive(
    pool: &db::PgPool,
    scope: RequestScope<'_>,
    id: &GatewayArchiveId,
) -> ApiResult<Archive> {
    gateway_archives::list(pool, scope, Some(id), None, 1)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::NotFound("archive not found".into()))
}

pub(super) fn spawn(pool: db::PgPool, storage: Arc<ArchiveStorage>, readiness: Arc<Readiness>) {
    if let Some(days) = storage.retire_after_days {
        archive_retirement::spawn(pool.clone(), Arc::clone(&storage), days, Arc::clone(&readiness));
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        let mut schedule = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        schedule.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let result = tokio::select! {
                _ = readiness.work_tick(&mut tick) => work(&pool, &storage).await,
                _ = readiness.work_tick(&mut schedule) => match storage.automatic_after_days {
                    Some(days) => gateway_archive_schedule::queue(&pool, days, PARTITION_EVENTS).await,
                    None => Ok(()),
                },
            };
            if let Err(error) = result {
                tracing::warn!(sqlstate = ?error.as_database_error().and_then(|error| error.code()), "archive worker database operation failed");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    });
}

async fn work(pool: &db::PgPool, storage: &Arc<ArchiveStorage>) -> sqlx::Result<()> {
    let Some(task) = gateway_archives::claim(pool, storage.automatic_after_days.is_some()).await? else {
        return Ok(());
    };
    let (Ok(account), Ok(project), Ok(id)) = (
        task.account_id.parse::<AccountId>(),
        task.project_id.parse::<GatewayProjectId>(),
        task.id.parse::<GatewayArchiveId>(),
    ) else {
        return gateway_archives::fail(pool, &task, "archive_scope_invalid").await;
    };
    let scope = RequestScope {
        account: &account,
        project: &project,
        client_ref: task.client_ref.as_deref(),
    };
    if task.state == "restoring" {
        let archive = gateway_archives::list(pool, scope, Some(&id), None, 1)
            .await?
            .pop();
        let Some(expected) = archive.and_then(|archive| Expected::from_manifest(&account, &archive)) else {
            return gateway_archives::fail(pool, &task, "archive_manifest_missing").await;
        };
        let key = expected.key.clone();
        return match storage.read(expected).await {
            Ok(_) => gateway_archives::restored(pool, &task).await,
            Err(COLD_OBJECT)
                if task.retrieve_cold && storage.permits_retrieval(&key.storage_target) =>
            {
                retrieve_archive(pool, storage, &task, &key).await
            }
            Err(COLD_OBJECT | IO_ERROR) if task.retrieval_attempted_at.is_some() => {
                gateway_archives::wait_for_retrieval(pool, &task, false).await
            }
            Err(code) => gateway_archives::fail(pool, &task, code).await,
        };
    }

    let mut events = gateway_exports::events_between(
        pool,
        scope,
        task.after_seq,
        task.through_seq,
        PARTITION_EVENTS + 1,
    )
    .await?;
    let has_more = archive_events::limit_page(&mut events, PARTITION_EVENTS as usize);
    if task.automatic && (has_more || events.len() as i64 != task.through_seq - task.after_seq) {
        return gateway_archives::fail(pool, &task, "archive_partition_incomplete").await;
    }
    if let Err(error) = archive_events::hydrate(pool, Some(storage), scope, &mut events).await {
        let code = if matches!(error, ApiError::ArchiveRetrievalRequired(_)) {
            "archive_source_retrieval_required"
        } else {
            "archive_source_unavailable"
        };
        return gateway_archives::fail(pool, &task, code).await;
    }
    let (Some(first), Some(last)) = (events.first(), events.last()) else {
        return gateway_archives::fail(pool, &task, "archive_empty").await;
    };
    let first = first.id.clone();
    let last = last.id.clone();
    let count = events.len() as i32;
    let Ok(body) = exports::encode_events(events) else {
        return gateway_archives::fail(pool, &task, "archive_encoding_failed").await;
    };
    let content_sha256 = digest(&body);
    let key = ObjectKey {
        id,
        account_id: task.account_id.clone(),
        project_id: task.project_id.clone(),
        client_ref: task.client_ref.clone(),
        generation: task.generation,
        storage_target: storage.write_target().to_owned(),
    };
    let object_sha256 = match storage.write(key.clone(), body).await {
        Ok(hash) => hash,
        Err(code) => return gateway_archives::fail(pool, &task, code).await,
    };
    let verification = Expected {
        key: key.clone(),
        schema_version: i32::from(exports::SCHEMA_VERSION),
        event_count: count,
        first_event_id: first.clone(),
        last_event_id: last.clone(),
        content_sha256: content_sha256.clone(),
        object_sha256: object_sha256.clone(),
    };
    if let Err(code) = storage.read(verification).await {
        storage.discard(&key).await;
        return gateway_archives::fail(pool, &task, code).await;
    }
    if !gateway_archives::store(
        pool,
        &task,
        Manifest {
            event_count: count,
            first_event_id: &first,
            last_event_id: &last,
            has_more,
            content_sha256: &content_sha256,
            object_sha256: &object_sha256,
            storage_target: &key.storage_target,
        },
    )
    .await?
    {
        storage.discard(&key).await;
    }
    Ok(())
}

async fn retrieve_archive(
    pool: &db::PgPool,
    storage: &ArchiveStorage,
    task: &ArchiveTask,
    key: &ObjectKey,
) -> sqlx::Result<()> {
    if task.retrieval_confirmed
        || task.retrieval_submissions >= gateway_archives::MAX_RETRIEVAL_SUBMISSIONS
    {
        return gateway_archives::wait_for_retrieval(pool, task, false).await;
    }
    if task.retrieval_attempted_at.is_some() {
        match storage.restoration_exists(key).await {
            Ok(true) => return gateway_archives::wait_for_retrieval(pool, task, true).await,
            Err(_) => return gateway_archives::wait_for_retrieval(pool, task, false).await,
            Ok(false) => {}
        }
    }
    if !gateway_archives::begin_retrieval(pool, task).await? {
        return gateway_archives::wait_for_retrieval(pool, task, false).await;
    }
    match storage.request_retrieval(key).await {
        Ok(result) => gateway_archives::wait_for_retrieval(pool, task, result == RestoreDispatch::Confirmed).await,
        Err(code) => gateway_archives::fail(pool, task, code).await,
    }
}
