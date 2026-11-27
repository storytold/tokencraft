use std::{sync::Arc, time::Duration};

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use db_queries::gateway_requests::content_restores::{self as jobs, Task, Update};
use enums::AuditEvent;
use serde::{Deserialize, Serialize};

use super::super::{
    access::{self, GatewayCaller, Permission, ScopeQuery},
    archive_blobs::{COLD_OBJECT, IO_ERROR},
    archive_bucket::RestoreDispatch,
    catalog::digest,
    handlers::parse_request_id,
    projects::audit,
    providers::ProviderRuntime,
    readiness::Readiness,
    RequestErrorResponse,
};
use super::{artifact_name, storage::Target, Store, MAX_BUNDLE};
use crate::{
    error::{ApiError, ApiResult},
    middleware::Caller,
    state::AppState,
};

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
enum RestoreState {
    Pending,
    Ready,
    Failed,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct RestoreProgress {
    state: RestoreState,
    ready_objects: i32,
    total_objects: i32,
    requested_at: DateTime<Utc>,
    retry_at: DateTime<Utc>,
}

impl From<jobs::Progress> for RestoreProgress {
    fn from(job: jobs::Progress) -> Self {
        Self {
            state: match job.state.as_str() {
                "pending" => RestoreState::Pending,
                "ready" => RestoreState::Ready,
                _ => RestoreState::Failed,
            },
            ready_objects: job.ready_objects,
            total_objects: job.total_objects,
            requested_at: job.requested_at,
            retry_at: job.retry_at,
        }
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestoreInput {}

#[utoipa::path(
    post, path = "/v1/requests/{id}/content/restore", operation_id = "queueRequestContentRestore", tag = "native",
    summary = "Queue a storage restore as a project administrator",
    security(("session_cookie" = [])),
    params(("id" = String, Path, description = "Request ID"), ScopeQuery),
    request_body = RestoreInput,
    responses((status = 202, body = RestoreProgress), (status = "default", response = RequestErrorResponse)),
)]
pub(crate) async fn enqueue(
    State(state): State<AppState>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
    Json(_input): Json<RestoreInput>,
) -> ApiResult<(StatusCode, Json<RestoreProgress>)> {
    let GatewayCaller::Account(Caller::Session(auth)) = &caller else {
        return Err(ApiError::Forbidden("archive restore requires an administrator session".into()));
    };
    let actor = auth.user.id.clone();
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    if !access.can_restore_content {
        return Err(ApiError::Forbidden("archive restore requires a project administrator".into()));
    }
    let scope = access.scope();
    let id = parse_request_id(&raw)?;
    let missing = || ApiError::NotFound("archived request content is unavailable".into());
    let mut tx = state.pool.begin().await?;
    let target = jobs::lock_archive(&mut tx, scope, &id).await?.ok_or_else(missing)?;
    if !runtime.as_ref().and_then(|runtime| runtime.content_archive.as_ref())
        .is_some_and(|store| store.permits_restore(&target))
    {
        return Err(ApiError::Conflict("gateway restore is disabled for this archive".into()));
    }
    if jobs::enqueue(&mut tx, &id, MAX_BUNDLE as i32).await? {
        audit(&mut tx, scope.account, &actor, AuditEvent::GatewayRequestContentRestoreQueued,
              "gateway_request", id.as_str()).await?;
    }
    let progress = jobs::progress(&mut *tx, &id).await?.ok_or_else(missing)?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(progress.into())))
}

pub(super) fn spawn(pool: db::PgPool, store: Arc<Store>, readiness: Arc<Readiness>) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(2));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            readiness.work_tick(&mut timer).await;
            if let Err(error) = work(&pool, &store.primary).await {
                tracing::warn!(sqlstate = ?error.as_database_error().and_then(|e| e.code()),
                    "request content restore deferred");
            }
        }
    });
}

async fn work(pool: &db::PgPool, target: &Target) -> sqlx::Result<()> {
    let Some(task) = jobs::claim(pool, target.id()).await? else {
        return Ok(());
    };
    let outcome = verify_object(target, &task).await;
    let mut tx = pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    if !jobs::lock_claim(&mut tx, target.id(), &task).await? {
        return tx.rollback().await;
    }
    let name = match outcome {
        Verification::Record(update) => {
            jobs::update(&mut tx, &task, update).await?;
            return tx.commit().await;
        }
        Verification::RequestRestore(name) => name,
    };
    let reservation = jobs::reserve(&mut tx, &task).await?;
    tx.commit().await?;
    let Some(reservation) = reservation else { return Ok(()) };

    // Serialize the mutating storage call with administrator removal.
    let mut tx = pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    if jobs::can_dispatch(&mut tx, target.id(), &task, reservation).await? {
        match target.request_restore(&name).await {
            Ok(RestoreDispatch::Confirmed) => jobs::update(&mut tx, &task, Update::Confirmed).await?,
            Ok(RestoreDispatch::Unconfirmed) => {}
            Err(_) => jobs::update(&mut tx, &task, Update::Failed).await?,
        }
    }
    tx.commit().await
}

enum Verification {
    Record(Update),
    RequestRestore(String),
}

async fn verify_object(target: &Target, task: &Task) -> Verification {
    let Ok(id) = task.request_id.parse() else {
        return Verification::Record(Update::Failed);
    };
    let name = if task.slot == -1 {
        format!("{id}.tch")
    } else {
        artifact_name(&id, task.slot)
    };
    match target.read(&name, task.byte_limit as usize).await {
        Ok(ciphertext) if digest(&ciphertext) == task.checksum => return Verification::Record(Update::Ready),
        Err(IO_ERROR) => return Verification::Record(Update::Wait),
        Err(COLD_OBJECT) => {}
        _ => return Verification::Record(Update::Failed),
    }
    if task.retrieval_confirmed || task.retrieval_submissions >= jobs::MAX_RESTORE_SUBMISSIONS
        || !target.permits_restore()
    {
        return Verification::Record(Update::Wait);
    }
    if task.attempted {
        match target.restoration_exists(&name).await {
            Ok(true) => return Verification::Record(Update::Confirmed),
            Ok(false) => {}
            Err(_) => return Verification::Record(Update::Wait),
        }
    }
    Verification::RequestRestore(name)
}
