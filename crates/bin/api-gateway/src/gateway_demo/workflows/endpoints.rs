use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use db_queries::{
    gateway_requests::{submission_limits, RequestScope},
    gateway_routing_policies, gateway_webhooks, workflows as db,
};
use ids::{AccountId, GatewayProjectId, WorkflowAppId, WorkflowId, WorkflowKeyId, WorkflowRunId};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use super::super::{
    artifacts, catalog_refresh::CatalogStore, handlers, providers::ProviderRuntime, requests,
};
use super::graph::{Graph, InputValues, Output, ValueType};
use super::management::{self, ConsoleQuery, Project};
use super::{
    budget, execution, planner, runner, uploads, Security, MAX_ACTIVE_RUNS_PER_APP,
    MAX_ACTIVE_RUNS_PER_WORKFLOW,
};
use crate::{
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    state::AppState,
};

#[derive(Deserialize)]
pub(super) struct WorkflowPath {
    project: GatewayProjectId,
    workflow: WorkflowId,
}

#[derive(Deserialize)]
pub(super) struct RunPath {
    project: GatewayProjectId,
    workflow: WorkflowId,
    run: WorkflowRunId,
}

#[derive(Deserialize)]
pub(super) struct AssetPath {
    project: GatewayProjectId,
    workflow: WorkflowId,
    run: WorkflowRunId,
    name: String,
}

#[derive(Deserialize)]
pub(super) struct ReadQuery {
    account_id: Option<AccountId>,
    #[serde(default)]
    include_execution: bool,
}

#[derive(Clone, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
enum Permission {
    #[serde(rename = "runs:submit")]
    Submit,
    #[serde(rename = "runs:read")]
    Read,
    #[serde(rename = "runs:cancel")]
    Cancel,
    #[serde(rename = "outputs:read")]
    Outputs,
}

impl Permission {
    fn value(&self) -> &'static str {
        match self {
            Self::Submit => "runs:submit",
            Self::Read => "runs:read",
            Self::Cancel => "runs:cancel",
            Self::Outputs => "outputs:read",
        }
    }
}

const KEY_PREFIX: &str = "tc_workflow_";

struct App {
    account: AccountId,
    principal: WorkflowAppId,
    read_outputs: bool,
    digest: String,
}

struct AccessInput<'a> {
    pool: &'a sqlx::PgPool,
    security: &'a Security,
    headers: &'a HeaderMap,
    project: &'a GatewayProjectId,
    workflow: &'a WorkflowId,
}

impl AccessInput<'_> {
    async fn app(&self, permission: Permission) -> ApiResult<App> {
        let invalid = || ApiError::Unauthorized("invalid workflow credential".into());
        let value = handlers::single_header(self.headers, "authorization")?.ok_or_else(invalid)?;
        let (scheme, secret) = value.split_once(' ').ok_or_else(invalid)?;
        let raw = secret
            .strip_prefix(KEY_PREFIX)
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(invalid)?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(invalid());
        }
        let digest = self.security.digest(raw.as_bytes());
        let app = db::find_credential(self.pool, &digest)
            .await?
            .ok_or_else(invalid)?;
        if &app.project_id != self.project || &app.workflow_id != self.workflow {
            return Err(management::missing());
        }
        if !app.scopes.iter().any(|value| value == permission.value()) {
            return Err(ApiError::Forbidden(format!(
                "requires {}",
                permission.value()
            )));
        }
        Ok(App {
            account: app.account_id,
            principal: app.app_id,
            read_outputs: app
                .scopes
                .iter()
                .any(|value| value == Permission::Outputs.value()),
            digest,
        })
    }

    async fn reader(
        &self,
        session: Option<&AuthContext>,
        query: ReadQuery,
        permission: Permission,
    ) -> ApiResult<Reader> {
        if self.headers.contains_key(header::AUTHORIZATION) {
            let app = self.app(permission).await?;
            return Ok(Reader {
                account: app.account,
                actor: Some(app.principal),
                read_outputs: app.read_outputs,
            });
        }
        let auth = session
            .ok_or_else(|| ApiError::Unauthorized("sign in or provide a workflow key".into()))?;
        let query = ConsoleQuery {
            account_id: query
                .account_id
                .ok_or_else(|| ApiError::BadRequest("account_id is required".into()))?,
        };
        Project {
            pool: self.pool,
            auth,
            query: &query,
            project: self.project,
        }
        .authorize(false)
        .await?;
        Ok(Reader {
            account: query.account_id,
            actor: None,
            read_outputs: true,
        })
    }

    fn scope<'a>(&'a self, account: &'a AccountId) -> db::Scope<'a> {
        db::Scope {
            account,
            project: self.project,
            workflow: self.workflow,
        }
    }
}

struct Reader {
    account: AccountId,
    actor: Option<WorkflowAppId>,
    read_outputs: bool,
}

impl Reader {
    fn can_read(&self, row: &db::Run) -> bool {
        self.actor
            .as_ref()
            .is_none_or(|actor| row.version.is_some() && row.actor_id == actor.as_str())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Invocation {
    inputs: InputValues,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Trial {
    revision: i64,
    inputs: InputValues,
}

enum Origin<'a> {
    Test { revision: i64, actor: &'a str },
    App(&'a App),
}

impl Origin<'_> {
    fn actor(&self) -> &str {
        match self {
            Self::Test { actor, .. } => actor,
            Self::App(app) => app.principal.as_str(),
        }
    }

    fn revision(&self) -> Option<i64> {
        match self {
            Self::Test { revision, .. } => Some(*revision),
            Self::App(_) => None,
        }
    }
}

struct Admit<'a> {
    scope: db::Scope<'a>,
    pool: &'a sqlx::PgPool,
    security: &'a Security,
    origin: Origin<'a>,
    idempotency_key: &'a str,
    catalog: &'a CatalogStore,
    providers: Option<&'a ProviderRuntime>,
}

#[derive(Serialize)]
pub(super) struct Receipt {
    id: WorkflowRunId,
    workflow_version: Option<i32>,
    status_url: String,
    replayed: bool,
}

fn run_url(path: &WorkflowPath, run: &WorkflowRunId) -> String {
    format!(
        "/v1/projects/{}/workflow-endpoints/{}/runs/{run}",
        path.project, path.workflow
    )
}

fn receipt(
    args: &Admit<'_>,
    id: WorkflowRunId,
    workflow_version: Option<i32>,
    replayed: bool,
) -> ApiResult<Response> {
    let mut status_url = run_url(
        &WorkflowPath {
            project: args.scope.project.clone(),
            workflow: args.scope.workflow.clone(),
        },
        &id,
    );
    if matches!(args.origin, Origin::Test { .. }) {
        status_url.push_str(&format!("?account_id={}", args.scope.account));
    }
    let location = HeaderValue::from_str(&status_url).map_err(|_| runner::unavailable())?;
    let response = Receipt {
        id,
        workflow_version,
        status_url,
        replayed,
    };
    let status = if replayed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((
        status,
        [
            (header::LOCATION, location),
            (header::RETRY_AFTER, HeaderValue::from_static("2")),
        ],
        Json(response),
    )
        .into_response())
}

async fn admit(args: Admit<'_>, inputs: InputValues) -> ApiResult<Response> {
    if !requests::valid_idempotency_key(args.idempotency_key) {
        return Err(ApiError::BadRequest("invalid Idempotency-Key".into()));
    }
    let key_digest = args.security.digest(args.idempotency_key.as_bytes());
    let intent = serde_json::to_vec(&(args.origin.revision(), &inputs))
        .map_err(|_| runner::unavailable())?;
    let intent_digest = args.security.digest(&intent);
    let actor = args.origin.actor();
    let mut tx = args.pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    gateway_routing_policies::lock_for_admission(&mut *tx, args.scope.account).await?;
    let row = db::lock_workflow(&mut *tx, args.scope)
        .await?
        .ok_or_else(management::missing)?;
    let application = if let Origin::App(app) = &args.origin {
        db::find_credential(&mut *tx, &app.digest)
            .await?
            .filter(|key| key.app_id == app.principal)
            .ok_or_else(|| ApiError::Unauthorized("invalid workflow credential".into()))?;
        Some(&app.principal)
    } else {
        None
    };
    if let Some(replay) = db::find_replay(
        &mut *tx,
        db::ReplayLookup {
            scope: args.scope,
            actor,
            digest: &key_digest,
        },
    )
    .await?
    {
        if replay.intent_digest != intent_digest {
            return Err(ApiError::Conflict(
                "the idempotency key belongs to a different invocation".into(),
            ));
        }
        return receipt(&args, replay.id, replay.version, true);
    }

    let (version, revision, definition) = match args.origin.revision() {
        Some(revision) if revision == row.revision => (None, revision, row.draft),
        Some(_) => {
            return Err(ApiError::Conflict(
                "the draft changed before the test was submitted".into(),
            ))
        }
        None => {
            if row.paused {
                return Err(ApiError::Conflict("the workflow endpoint is paused".into()));
            }
            let version = row
                .active_version
                .ok_or_else(|| ApiError::Conflict("publish a workflow version first".into()))?;
            let release = db::find_version(&mut *tx, args.scope, version)
                .await?
                .ok_or_else(management::missing)?;
            (Some(version), release.draft_revision, release.graph)
        }
    };
    let graph = management::decode(definition.clone())?;
    let inputs = graph.resolve_inputs(inputs).map_err(ApiError::BadRequest)?;
    let plan = graph.compile().map_err(ApiError::BadRequest)?;
    args.security.tools.validate(&graph, args.scope.account, args.scope.project)?;
    uploads::validate(&mut tx, args.scope, actor, &graph, &inputs).await?;
    planner::validate_models(&graph, &args.catalog.current(), args.providers)
        .map_err(ApiError::BadRequest)?;
    let active = db::count_active_runs(&mut *tx, args.scope, application, MAX_ACTIVE_RUNS_PER_WORKFLOW)
        .await?;
    if active.workflow >= MAX_ACTIVE_RUNS_PER_WORKFLOW || active.application >= MAX_ACTIVE_RUNS_PER_APP {
        return Ok((
            [(header::RETRY_AFTER, HeaderValue::from_static("2"))],
            ApiError::ActiveRequestLimit,
        ).into_response());
    }
    let id = WorkflowRunId::generate();
    let content = runner::State::new(inputs).seal(args.security, &id)?;
    let webhook_subscription_id = match application {
        Some(app) => gateway_webhooks::active_workflow_subscription(&mut *tx, args.scope, app).await?,
        None => None,
    };
    db::create_run(
        &mut *tx,
        db::NewRun {
            scope: args.scope,
            id: &id,
            actor,
            version,
            webhook_subscription_id: webhook_subscription_id.as_deref(),
            budget_reserved_usd_nanos: plan.budget_reserved_usd_nanos,
            draft_revision: revision,
            idempotency_digest: &key_digest,
            intent_digest: &intent_digest,
            graph: &definition,
            content: &content,
        },
    )
    .await?;
    if plan.budget_reserved_usd_nanos > 0 {
        budget::verify_reservation(&mut tx, RequestScope {
            account: args.scope.account,
            project: args.scope.project,
            client_ref: application.map(WorkflowAppId::as_str),
        }).await?;
    }
    let retry_after = submission_limits::consume(&mut tx, args.scope.account).await?;
    if retry_after > 0 {
        return Err(ApiError::SubmissionRateLimit(retry_after));
    }
    tx.commit().await?;
    receipt(&args, id, version, false)
}

pub(super) async fn upload(
    State(state): State<AppState>,
    auth: Option<AuthContext>,
    Query(query): Query<ReadQuery>,
    Path(path): Path<WorkflowPath>,
    Extension(security): Extension<Arc<Security>>,
    request: Request,
) -> ApiResult<Response> {
    let (parts, body) = request.into_parts();
    let access = AccessInput {
        pool: &state.pool, security: &security, headers: &parts.headers,
        project: &path.project, workflow: &path.workflow,
    };
    let app = if parts.headers.contains_key(header::AUTHORIZATION) {
        Some(access.app(Permission::Submit).await?)
    } else {
        None
    };
    let (account, actor) = if let Some(app) = &app {
        (app.account.clone(), app.principal.as_str())
    } else {
        let reader = access.reader(auth.as_ref(), query, Permission::Submit).await?;
        let actor = auth.as_ref().ok_or_else(runner::unavailable)?.user.id.as_str();
        (reader.account, actor)
    };
    let scope = access.scope(&account);
    // Hold the slot through encryption and commit as those operations also retain upload bytes.
    let Ok(_slot) = security.upload_slots.try_acquire() else {
        let error = ApiError::ServiceUnavailable("workflow uploads are busy; retry this file".into());
        return Ok(([(header::RETRY_AFTER, HeaderValue::from_static("2"))], error).into_response());
    };
    let pending = uploads::receive(&parts.headers, body).await?;
    let mut tx = state.pool.begin().await?;
    let workflow = db::lock_workflow(&mut *tx, scope).await?.ok_or_else(management::missing)?;
    let definition = if let Some(app) = &app {
        db::find_credential(&mut *tx, &app.digest).await?
            .filter(|key| key.app_id == app.principal)
            .ok_or_else(|| ApiError::Unauthorized("invalid workflow credential".into()))?;
        let version = workflow.active_version.filter(|_| !workflow.paused)
            .ok_or_else(|| ApiError::Conflict("the workflow endpoint is unavailable".into()))?;
        db::find_version(&mut *tx, scope, version).await?.ok_or_else(management::missing)?.graph
    } else {
        workflow.draft
    };
    if !pending.compatible(&management::decode(definition)?) {
        return Err(ApiError::BadRequest("the workflow has no request input for this media type".into()));
    }
    let receipt = pending.store(&mut tx, &security, scope, actor).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(receipt)).into_response())
}

pub(super) async fn invoke(
    State(state): State<AppState>,
    Path(path): Path<WorkflowPath>,
    headers: HeaderMap,
    Extension(security): Extension<Arc<Security>>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    Json(input): Json<Invocation>,
) -> ApiResult<Response> {
    let access = AccessInput {
        pool: &state.pool,
        security: &security,
        headers: &headers,
        project: &path.project,
        workflow: &path.workflow,
    };
    let app = access.app(Permission::Submit).await?;
    admit(
        Admit {
            scope: access.scope(&app.account),
            pool: &state.pool,
            security: &security,
            origin: Origin::App(&app),
            idempotency_key: handlers::idempotency_key(&headers)?,
            catalog: &catalog,
            providers: providers.as_deref(),
        },
        input.inputs,
    )
    .await
}

pub(super) async fn test(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path(path): Path<WorkflowPath>,
    headers: HeaderMap,
    Extension(security): Extension<Arc<Security>>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    Json(input): Json<Trial>,
) -> ApiResult<Response> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &path.project,
    };
    project.authorize(false).await?;
    admit(
        Admit {
            scope: project.scope(&path.workflow),
            pool: &state.pool,
            security: &security,
            catalog: &catalog,
            providers: providers.as_deref(),
            origin: Origin::Test {
                revision: input.revision,
                actor: auth.user.id.as_str(),
            },
            idempotency_key: handlers::idempotency_key(&headers)?,
        },
        input.inputs,
    )
    .await
}

pub(super) async fn list_runs(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path(path): Path<WorkflowPath>,
) -> ApiResult<Json<Vec<db::RunSummary>>> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &path.project,
    };
    project.authorize(false).await?;
    Ok(Json(
        db::list_runs(&state.pool, project.scope(&path.workflow)).await?,
    ))
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PublicOutput {
    Text {
        text: String,
    },
    Media {
        kind: ValueType,
        download_url: String,
    },
    Fixture {
        kind: ValueType,
    },
}

#[derive(Serialize)]
struct RunView {
    id: WorkflowRunId,
    status: String,
    workflow_version: Option<i32>,
    draft_revision: i64,
    created_at: DateTime<Utc>,
    steps: Vec<runner::Step>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    tools: BTreeMap<String, super::tool_execution::StepStatus>,
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outputs: Option<BTreeMap<String, PublicOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<execution::Execution>,
}

fn published_results(
    graph: &Graph,
    progress: &runner::State,
    location: &str,
) -> BTreeMap<String, PublicOutput> {
    graph
        .outputs
        .iter()
        .filter_map(|(name, source)| {
            let url = || format!("{location}/outputs/{name}");
            let output = match progress.outputs.get(source)? {
                Output::Text { text } => PublicOutput::Text { text: text.clone() },
                Output::Image { .. } => PublicOutput::Media {
                    kind: ValueType::Image,
                    download_url: url(),
                },
                Output::Video { .. } => PublicOutput::Media {
                    kind: ValueType::Video,
                    download_url: url(),
                },
                Output::Fixture { kind } => PublicOutput::Fixture { kind: *kind },
                Output::Tool { kind, .. } => PublicOutput::Media { kind: *kind, download_url: url() },
            };
            Some((name.clone(), output))
        })
        .collect()
}

pub(super) async fn status(
    State(state): State<AppState>,
    auth: Option<AuthContext>,
    Query(query): Query<ReadQuery>,
    Path(path): Path<RunPath>,
    headers: HeaderMap,
    Extension(security): Extension<Arc<Security>>,
) -> ApiResult<Response> {
    let console_account = query.account_id.clone();
    let include_execution = query.include_execution;
    let access = AccessInput {
        pool: &state.pool,
        security: &security,
        headers: &headers,
        project: &path.project,
        workflow: &path.workflow,
    };
    let reader = access
        .reader(auth.as_ref(), query, Permission::Read)
        .await?;
    let row = db::find_run(&state.pool, access.scope(&reader.account), &path.run)
        .await?
        .filter(|row| reader.can_read(row))
        .ok_or_else(management::missing)?;
    let include_execution = include_execution && reader.actor.is_none();
    let etag = format!(
        "\"{}-{}-{}-{}\"",
        row.id,
        row.revision,
        reader.read_outputs,
        row.content.is_some()
    );
    let tag = HeaderValue::from_str(&etag).map_err(|_| runner::unavailable())?;
    if !include_execution && headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|value| value == &tag)
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, tag)]).into_response());
    }
    let expired = row.status == "expired";
    let progress = match runner::State::read(&security, &row) {
        Err(_) if expired => None,
        result => result?,
    };
    let graph = management::decode(row.graph.clone())?;
    let execution = if include_execution {
        Some(execution::read(&state.pool, &row, &graph, progress.as_ref()).await?)
    } else {
        None
    };
    let location = run_url(
        &WorkflowPath {
            project: path.project,
            workflow: path.workflow,
        },
        &row.id,
    );
    let outputs = if row.status == "succeeded" && reader.read_outputs {
        progress
            .as_ref()
            .map(|progress| published_results(&graph, progress, &location))
    } else {
        None
    };
    let mut view = RunView {
        id: row.id,
        workflow_version: row.version,
        draft_revision: row.draft_revision,
        status: row.status,
        created_at: row.created_at,
        steps: Vec::new(),
        tools: BTreeMap::new(),
        error: None,
        outputs,
        execution,
    };
    if let Some(progress) = progress {
        view.steps = progress.steps;
        view.tools = progress.tools.into_iter().map(|(node, step)| {
            let status = step.into_status(&view.id, &node);
            (node, status)
        }).collect();
        view.error = progress.error;
    }
    if expired {
        view.error = Some("run_deadline_exceeded".into());
    }
    if reader.actor.is_none() {
        if let (Some(account), Some(outputs)) = (console_account, &mut view.outputs) {
            for output in outputs.values_mut() {
                if let PublicOutput::Media { download_url, .. } = output {
                    download_url.push_str(&format!("?account_id={account}"));
                }
            }
        }
    }
    if include_execution {
        Ok(([(header::CACHE_CONTROL, "private, no-store")], Json(view)).into_response())
    } else {
        Ok(([(header::ETAG, tag)], Json(view)).into_response())
    }
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    auth: Option<AuthContext>,
    Query(query): Query<ReadQuery>,
    Path(path): Path<RunPath>,
    headers: HeaderMap,
    Extension(security): Extension<Arc<Security>>,
) -> ApiResult<StatusCode> {
    let access = AccessInput {
        pool: &state.pool,
        security: &security,
        headers: &headers,
        project: &path.project,
        workflow: &path.workflow,
    };
    let reader = access
        .reader(auth.as_ref(), query, Permission::Cancel)
        .await?;
    let scope = access.scope(&reader.account);
    let row = db::find_run(&state.pool, scope, &path.run)
        .await?
        .filter(|row| reader.can_read(row))
        .ok_or_else(management::missing)?;
    let canceled = db::cancel_run(
        &state.pool,
        db::CancelRun {
            scope,
            id: &path.run,
            actor: reader.actor.as_ref().map(WorkflowAppId::as_str),
        },
    )
    .await?;
    if canceled {
        let content = runner::State::read(&security, &row)?;
        if let Some(step) = content.as_ref().and_then(|value| value.steps.last()) {
            let _ = requests::cancel(&state.pool, runner::scope(&row), &step.request).await;
        }
    }
    if canceled || row.status == "canceled" {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::Conflict("the run has already finished".into()))
    }
}

pub(super) async fn asset(
    State(state): State<AppState>,
    auth: Option<AuthContext>,
    Query(query): Query<ReadQuery>,
    Path(path): Path<AssetPath>,
    headers: HeaderMap,
    Extension(security): Extension<Arc<Security>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
) -> ApiResult<Response> {
    let access = AccessInput {
        pool: &state.pool,
        security: &security,
        headers: &headers,
        project: &path.project,
        workflow: &path.workflow,
    };
    let reader = access
        .reader(auth.as_ref(), query, Permission::Outputs)
        .await?;
    let row = db::find_run(&state.pool, access.scope(&reader.account), &path.run)
        .await?
        .filter(|row| row.status == "succeeded" && reader.can_read(row))
        .ok_or_else(management::missing)?;
    let graph = management::decode(row.graph.clone())?;
    let progress = runner::State::read(&security, &row)?.ok_or_else(management::missing)?;
    let request = match graph
        .outputs
        .get(&path.name)
        .and_then(|source| progress.outputs.get(source))
    {
        Some(Output::Image { request_id } | Output::Video { request_id }) => request_id,
        Some(Output::Tool { node, .. }) => {
            return super::tool_execution::download(&state.pool, &security, &row, node, &path.name).await;
        }
        _ => return Err(management::missing()),
    };
    artifacts::download_for_scope(artifacts::ScopedDownload {
        pool: &state.pool,
        scope: runner::scope(&row),
        runtime: providers.as_deref(),
        request,
        slot: 0,
    })
    .await
}

pub(super) async fn contract(
    State(state): State<AppState>,
    auth: Option<AuthContext>,
    Query(query): Query<ReadQuery>,
    Path(path): Path<WorkflowPath>,
    headers: HeaderMap,
    Extension(security): Extension<Arc<Security>>,
) -> ApiResult<Response> {
    let access = AccessInput {
        pool: &state.pool,
        security: &security,
        headers: &headers,
        project: &path.project,
        workflow: &path.workflow,
    };
    let reader = access
        .reader(auth.as_ref(), query, Permission::Submit)
        .await?;
    let scope = access.scope(&reader.account);
    let row = db::find_workflow(&state.pool, scope)
        .await?
        .ok_or_else(management::missing)?;
    let version = row.active_version.ok_or_else(management::missing)?;
    let graph = management::decode(
        db::find_version(&state.pool, scope, version)
            .await?
            .ok_or_else(management::missing)?
            .graph,
    )?;
    let endpoint = format!("/v1/projects/{}/workflow-endpoints/{}", path.project, path.workflow);
    Ok(Json(super::contract::document(&graph, &endpoint, version)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateApp {
    name: String,
    scopes: Vec<Permission>,
    days: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rotate {
    days: i32,
}

#[derive(Serialize)]
pub(super) struct IssuedKey {
    app_id: WorkflowAppId,
    key_id: WorkflowKeyId,
    secret: String,
    expires_at: DateTime<Utc>,
}

struct Issue<'a> {
    scope: db::Scope<'a>,
    security: &'a Security,
    actor: &'a str,
    app: WorkflowAppId,
    days: i32,
}

async fn issue(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    args: Issue<'_>,
) -> ApiResult<IssuedKey> {
    if !(1..=90).contains(&args.days) {
        return Err(ApiError::BadRequest(
            "set an expiration between 1 and 90 days".into(),
        ));
    }
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| runner::unavailable())?;
    let encoded = hex::encode(random);
    let secret = format!("{KEY_PREFIX}{encoded}");
    let key_id = WorkflowKeyId::generate();
    let expires_at = db::insert_key(
        &mut **tx,
        db::NewKey {
            scope: args.scope,
            id: &key_id,
            app: &args.app,
            hash: &args.security.digest(encoded.as_bytes()),
            prefix: &secret[..KEY_PREFIX.len() + 8],
            days: args.days,
            actor: args.actor,
        },
    )
    .await?
    .ok_or_else(management::missing)?;
    Ok(IssuedKey {
        app_id: args.app,
        key_id,
        secret,
        expires_at,
    })
}

pub(super) async fn keys(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path(path): Path<WorkflowPath>,
) -> ApiResult<Json<Vec<db::AppKey>>> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &path.project,
    };
    project.authorize(true).await?;
    Ok(Json(
        db::list_keys(&state.pool, project.scope(&path.workflow)).await?,
    ))
}

pub(super) async fn create_app(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path(path): Path<WorkflowPath>,
    Extension(security): Extension<Arc<Security>>,
    Json(mut input): Json<CreateApp>,
) -> ApiResult<(StatusCode, Json<IssuedKey>)> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &path.project,
    };
    project.authorize(true).await?;
    let name = input.name.trim();
    input.scopes.sort();
    input.scopes.dedup();
    if name.is_empty() || name.len() > 80 || input.scopes.is_empty() {
        return Err(ApiError::BadRequest(
            "supply a name and at least one scope".into(),
        ));
    }
    let mut tx = state.pool.begin().await?;
    let scope = project.scope(&path.workflow);
    db::lock_workflow(&mut *tx, scope)
        .await?
        .ok_or_else(management::missing)?;
    let app = WorkflowAppId::generate();
    let actor = auth.user.id.as_str();
    let scopes = input
        .scopes
        .into_iter()
        .map(|permission| permission.value().to_owned())
        .collect::<Vec<_>>();
    db::insert_app(
        &mut *tx,
        db::NewApp {
            scope,
            id: &app,
            name,
            scopes: &scopes,
            actor,
        },
    )
    .await?;
    let key = issue(
        &mut tx,
        Issue {
            scope,
            security: &security,
            actor,
            app,
            days: input.days,
        },
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(key)))
}

pub(super) async fn rotate(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, workflow, app)): Path<(GatewayProjectId, WorkflowId, WorkflowAppId)>,
    Extension(security): Extension<Arc<Security>>,
    Json(input): Json<Rotate>,
) -> ApiResult<(StatusCode, Json<IssuedKey>)> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(true).await?;
    let mut tx = state.pool.begin().await?;
    let key = issue(
        &mut tx,
        Issue {
            scope: project.scope(&workflow),
            security: &security,
            actor: auth.user.id.as_str(),
            app,
            days: input.days,
        },
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(key)))
}

pub(super) async fn revoke(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, workflow, key)): Path<(GatewayProjectId, WorkflowId, WorkflowKeyId)>,
) -> ApiResult<StatusCode> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(true).await?;
    if db::revoke_key(&state.pool, project.scope(&workflow), &key).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(management::missing())
    }
}
