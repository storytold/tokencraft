use std::{
    sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Query, Request, State},
    http::{header::{CACHE_CONTROL, RETRY_AFTER}, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension, Json,
};
use enums::MemberRole;
use serde::Serialize;
use utoipa::ToSchema;

use super::{process::ProcessRole, projects::OrganizationQuery};
use crate::{
    authz::require_account_role, error::{ApiError, ApiResult}, middleware::AuthContext, state::AppState,
};

const CHECK_INTERVAL: Duration = Duration::from_secs(5);
const EXPIRE_AFTER: Duration = Duration::from_secs(15);
const DB_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Checking,
    Ready,
    Unavailable,
    NotConfigured,
    External,
}

impl From<bool> for CheckStatus {
    fn from(ready: bool) -> Self {
        if ready {
            Self::Ready
        } else {
            Self::Unavailable
        }
    }
}

struct Observation(Mutex<(Instant, CheckStatus)>);

impl Observation {
    fn new() -> Self {
        Self(Mutex::new((Instant::now(), CheckStatus::Checking)))
    }

    fn record(&self, ready: bool) {
        if let Ok(mut state) = self.0.lock() {
            *state = (Instant::now(), ready.into());
        }
    }

    fn status(&self) -> CheckStatus {
        match self.0.lock().ok().map(|state| *state) {
            Some((observed, status)) if observed.elapsed() < EXPIRE_AFTER => status,
            _ => CheckStatus::Unavailable,
        }
    }
}

pub(super) struct Readiness {
    role: ProcessRole,
    database: Observation,
    schema: Observation,
    queue: Observation,
    provider: Option<Observation>,
    provider_tasks: AtomicUsize,
    artifact: Option<Observation>,
    artifact_backend: &'static str,
}

impl Readiness {
    pub(super) fn new(role: ProcessRole, provider_enabled: bool, artifact_backend: &'static str) -> Self {
        Self {
            role,
            database: Observation::new(),
            schema: Observation::new(),
            queue: Observation::new(),
            provider: provider_enabled.then(Observation::new),
            provider_tasks: AtomicUsize::new(0),
            artifact: (artifact_backend != "postgresql").then(Observation::new),
            artifact_backend,
        }
    }

    pub(super) fn role(&self) -> ProcessRole {
        self.role
    }

    pub(super) fn set_provider_tasks(&self, active: usize) {
        self.provider_tasks.store(active, Ordering::Relaxed);
    }

    pub(super) fn provider_tasks(&self) -> usize {
        self.provider_tasks.load(Ordering::Relaxed)
    }

    pub(super) fn queue_observed(&self, success: bool) {
        self.queue.record(success);
    }

    pub(super) fn provider_observed(&self, success: bool) {
        if let Some(provider) = &self.provider {
            provider.record(success);
        }
    }

    pub(super) fn artifact_observed(&self, success: bool) {
        if let Some(artifact) = &self.artifact {
            artifact.record(success);
        }
    }

    pub(super) fn database_schema_ready(&self) -> bool {
        self.database.status() == CheckStatus::Ready && self.schema.status() == CheckStatus::Ready
    }

    pub(super) async fn work_tick(&self, timer: &mut tokio::time::Interval) {
        loop {
            timer.tick().await;
            if self.database_schema_ready() {
                return;
            }
        }
    }

    pub(super) fn observe_database(self: Arc<Self>, pool: db::PgPool) {
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(CHECK_INTERVAL);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let conn = tokio::time::timeout(DB_TIMEOUT, async {
                    let mut conn = pool.acquire().await?;
                    sqlx::query("SELECT 1").execute(&mut *conn).await?;
                    Ok::<_, sqlx::Error>(conn)
                })
                .await;
                let Ok(Ok(mut conn)) = conn else {
                    self.database.record(false);
                    self.schema.record(false);
                    continue;
                };
                self.database.record(true);
                let current =
                    tokio::time::timeout(DB_TIMEOUT, db::migrations_current(&mut conn)).await;
                self.schema.record(matches!(current, Ok(Ok(true))));
            }
        });
    }

    fn worker_status(&self, worker: &Observation) -> CheckStatus {
        if self.role.runs_workers() {
            worker.status()
        } else {
            CheckStatus::External
        }
    }

    pub(super) fn traffic_ready(&self) -> bool {
        // Every replica depends on the same bucket. Keep API routes that only
        // need PostgreSQL reachable during a shared storage outage.
        let require_artifact_storage = self.artifact_backend != "s3" || !self.role.serves_api();
        self.snapshot(require_artifact_storage).ready
    }

    pub(super) fn snapshot(&self, require_artifact_storage: bool) -> SetupChecks {
        let database = self.database.status();
        let migrations = self.schema.status();
        let worker = self.worker_status(&self.queue);
        let provider_worker = self
            .provider
            .as_ref()
            .map_or(CheckStatus::NotConfigured, |provider| {
                self.worker_status(provider)
            });
        let artifact_storage = self.artifact.as_ref().map_or(migrations, Observation::status);
        let ready = [database, migrations, worker, provider_worker]
            .iter()
            .all(|status| {
                matches!(
                    status,
                    CheckStatus::Ready | CheckStatus::NotConfigured | CheckStatus::External
                )
            })
            && (!require_artifact_storage || artifact_storage == CheckStatus::Ready);
        SetupChecks {
            ready,
            database,
            migrations,
            worker,
            provider_worker,
            artifact_storage,
            artifact_backend: self.artifact_backend,
        }
    }
}

pub(super) async fn require_database_schema(
    State(readiness): State<Arc<Readiness>>,
    request: Request,
    next: Next,
) -> Response {
    if readiness.database_schema_ready() {
        return next.run(request).await;
    }
    let mut response = ApiError::ServiceUnavailable("gateway database or schema is not ready".into())
        .into_response();
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(RETRY_AFTER, HeaderValue::from(CHECK_INTERVAL.as_secs()));
    response
}

#[derive(Serialize, ToSchema)]
pub(super) struct ReadyResponse {
    ready: bool,
}

#[derive(Serialize, ToSchema)]
pub(super) struct SetupChecks {
    ready: bool,
    database: CheckStatus,
    migrations: CheckStatus,
    worker: CheckStatus,
    provider_worker: CheckStatus,
    artifact_storage: CheckStatus,
    artifact_backend: &'static str,
}

#[utoipa::path(
    get, path = "/ready", tag = "meta",
    responses(
        (status = 200, description = "The scoped gateway is ready.", body = ReadyResponse),
        (status = 503, description = "One or more gateway readiness checks have not passed.", body = ReadyResponse),
    ),
)]
pub(super) async fn ready(
    Extension(readiness): Extension<Arc<Readiness>>,
) -> (StatusCode, HeaderMap, Json<ReadyResponse>) {
    let ready = readiness.traffic_ready();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(revision) = option_env!("TOKENCRAFT_SOURCE_REVISION").filter(|value| {
        value.len() == 40 && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }) {
        headers.insert("tokencraft-source-revision", HeaderValue::from_static(revision));
    }
    (status, headers, Json(ReadyResponse { ready }))
}

#[utoipa::path(
    get, path = "/v1/setup/status", tag = "meta", security(("session_cookie" = [])),
    params(("account_id" = String, Query, description = "Organization whose administrator is requesting setup status.")),
    responses((status = 200, description = "Recent setup checks for this gateway instance.", body = SetupChecks)),
)]
pub(super) async fn setup(
    State(state): State<AppState>,
    Extension(readiness): Extension<Arc<Readiness>>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
) -> ApiResult<Json<SetupChecks>> {
    let account = query.account()?;
    require_account_role(&state.pool, &auth.user.id, &account, MemberRole::ADMINS).await?;
    Ok(Json(readiness.snapshot(true)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialized(role: ProcessRole, backend: &'static str) -> Readiness {
        let readiness = Readiness::new(role, true, backend);
        readiness.database.record(true);
        readiness.schema.record(true);
        readiness.queue_observed(true);
        readiness.provider_observed(true);
        readiness.artifact_observed(true);
        readiness
    }

    #[test]
    fn api_traffic_survives_shared_bucket_failure_and_setup_reports_it() {
        for role in [ProcessRole::Api, ProcessRole::Combined] {
            let readiness = initialized(role, "s3");
            assert!(readiness.snapshot(true).ready);
            readiness.artifact_observed(false);
            assert!(readiness.traffic_ready());
            let setup = readiness.snapshot(true);
            assert!(!setup.ready);
            assert!(matches!(setup.artifact_storage, CheckStatus::Unavailable));

            readiness.database.record(false);
            assert!(!readiness.traffic_ready());
        }
    }

    #[test]
    fn worker_and_replica_local_storage_keep_their_readiness_gate() {
        for (role, backend) in [(ProcessRole::Worker, "s3"), (ProcessRole::Api, "filesystem")] {
            let readiness = initialized(role, backend);
            assert!(readiness.traffic_ready());
            readiness.artifact_observed(false);
            assert!(!readiness.traffic_ready());
        }
    }

    #[test]
    fn background_work_requires_database_and_schema_checks() {
        let readiness = Readiness::new(ProcessRole::Worker, true, "s3");
        assert!(!readiness.database_schema_ready());
        readiness.database.record(true);
        readiness.schema.record(true);
        readiness.artifact_observed(false);
        assert!(readiness.database_schema_ready());
        readiness.schema.record(false);
        assert!(!readiness.database_schema_ready());
        readiness.schema.record(true);
        readiness.database.record(false);
        assert!(!readiness.database_schema_ready());
    }

    #[tokio::test]
    async fn operational_routes_wait_for_schema_compatibility() {
        use axum::{body::Body, routing::post, Router};
        use tower::ServiceExt;

        let readiness = Arc::new(initialized(ProcessRole::Api, "s3"));
        readiness.schema.record(false);
        let router = Router::new()
            .route("/submit", post(|| async { StatusCode::CREATED }))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&readiness), require_database_schema,
            ));
        let request = || axum::http::Request::post("/submit").body(Body::empty()).unwrap();
        let blocked = router.clone().oneshot(request()).await.unwrap();
        assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(blocked.headers()[CACHE_CONTROL], "no-store");
        assert_eq!(blocked.headers()[RETRY_AFTER], "5");

        readiness.schema.record(true);
        readiness.artifact_observed(false);
        let admitted = router.oneshot(request()).await.unwrap();
        assert_eq!(admitted.status(), StatusCode::CREATED);
    }
}
