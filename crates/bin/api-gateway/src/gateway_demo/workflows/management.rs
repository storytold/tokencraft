use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use db_queries::{gateway_requests::RequestScope, gateway_routing_policies, workflows as db};
use ids::{AccountId, GatewayProjectId, WorkflowId};
use serde::{Deserialize, Serialize};

use super::super::{
    access, catalog::Catalog, catalog_refresh::CatalogStore, providers::ProviderRuntime, routing_policy::Policies,
};
use super::graph::Graph;
use super::planner::{self, Model, PlanInput, Primitive};
use crate::error::{ApiError, ApiResult};
use crate::middleware::AuthContext;
use crate::state::AppState;

#[derive(Deserialize)]
pub(super) struct ConsoleQuery {
    pub account_id: AccountId,
}

pub(super) struct Project<'a> {
    pub pool: &'a sqlx::PgPool,
    pub auth: &'a AuthContext,
    pub query: &'a ConsoleQuery,
    pub project: &'a GatewayProjectId,
}

impl Project<'_> {
    pub async fn authorize(&self, admin: bool) -> ApiResult<()> {
        let entries = access::session_projects(
            self.pool,
            self.auth,
            &self.query.account_id,
            Some(self.project),
            false,
        )
        .await?;
        let project = entries
            .first()
            .ok_or_else(|| ApiError::NotFound("project not found".into()))?;
        if project.role != "admin" && (admin || project.role != "developer") {
            return Err(ApiError::Forbidden(
                "this project role cannot manage workflows".into(),
            ));
        }
        Ok(())
    }

    pub fn scope<'a>(&'a self, workflow: &'a WorkflowId) -> db::Scope<'a> {
        db::Scope {
            account: &self.query.account_id,
            project: self.project,
            workflow,
        }
    }

    fn request_scope(&self) -> RequestScope<'_> {
        RequestScope { account: &self.query.account_id, project: self.project, client_ref: None }
    }
}

#[derive(Serialize)]
pub(super) struct Capabilities {
    primitives: Vec<Primitive>,
    models: Vec<Model>,
}

#[derive(Deserialize)]
pub(super) struct ModelQuery {
    account_id: Option<AccountId>,
    project_id: Option<GatewayProjectId>,
}

impl ModelQuery {
    async fn models(
        self,
        pool: &sqlx::PgPool,
        auth: &AuthContext,
        catalog: &Catalog,
        providers: Option<&ProviderRuntime>,
    ) -> ApiResult<Vec<Model>> {
        match (self.account_id, self.project_id) {
            (None, None) => Ok(planner::models(catalog, providers, None)),
            (Some(account_id), Some(project_id)) => {
                let query = ConsoleQuery { account_id };
                let project = Project { pool, auth, query: &query, project: &project_id };
                project.authorize(false).await?;
                let scope = project.request_scope();
                let policies = Policies::load(pool, scope).await?;
                Ok(planner::models(catalog, providers, Some((&policies, scope))))
            }
            _ => Err(ApiError::BadRequest("supply both account_id and project_id for a project catalog".into())),
        }
    }
}

pub(super) async fn capabilities(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ModelQuery>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
) -> ApiResult<Json<Capabilities>> {
    let catalog = catalog.current();
    Ok(Json(Capabilities {
        primitives: planner::primitives(),
        models: query.models(&state.pool, &auth, &catalog, providers.as_deref()).await?,
    }))
}

pub(super) async fn plan(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ModelQuery>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    Json(input): Json<PlanInput>,
) -> ApiResult<Json<Graph>> {
    if input.intent.trim().is_empty() || input.intent.len() > 2000 {
        return Err(ApiError::BadRequest(
            "describe your workflow in up to 2000 bytes".into(),
        ));
    }
    let catalog = catalog.current();
    let models = query.models(&state.pool, &auth, &catalog, providers.as_deref()).await?;
    Ok(Json(planner::plan(input, &models)))
}

#[derive(Serialize)]
pub(super) struct WorkflowView {
    pub id: WorkflowId,
    pub title: String,
    pub draft: Graph,
    pub revision: i64,
    pub active_version: Option<i32>,
    pub paused: bool,
}

impl TryFrom<db::Workflow> for WorkflowView {
    type Error = ApiError;

    fn try_from(row: db::Workflow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            title: row.title,
            draft: decode(row.draft)?,
            revision: row.revision,
            active_version: row.active_version,
            paused: row.paused,
        })
    }
}

pub(super) fn decode(graph: serde_json::Value) -> ApiResult<Graph> {
    serde_json::from_value(graph)
        .map_err(|_| ApiError::Internal("could not read stored workflow".into()))
}

pub(super) fn missing() -> ApiError {
    ApiError::NotFound("workflow not found".into())
}

fn draft_json(graph: &Graph) -> ApiResult<serde_json::Value> {
    if graph.schema != 1
        || graph.title.trim().is_empty()
        || graph.title.len() > 100
        || graph.nodes.len() > super::graph::MAX_NODES
        || graph.inputs.len() > 24
        || graph.outputs.len() > 20
    {
        return Err(ApiError::BadRequest(
            "check the workflow name, schema version and limits".into(),
        ));
    }
    serde_json::to_value(graph).map_err(|_| ApiError::Internal("could not encode workflow".into()))
}

pub(super) async fn list(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path(project): Path<GatewayProjectId>,
) -> ApiResult<Json<Vec<db::Summary>>> {
    Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    }
    .authorize(false)
    .await?;
    Ok(Json(
        db::list_workflows(&state.pool, &query.account_id, &project).await?,
    ))
}

pub(super) async fn create(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path(project): Path<GatewayProjectId>,
    Json(graph): Json<Graph>,
) -> ApiResult<(StatusCode, Json<WorkflowView>)> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(false).await?;
    let id = WorkflowId::generate();
    let row = db::create_workflow(
        &state.pool,
        db::NewWorkflow {
            scope: project.scope(&id),
            title: graph.title.trim(),
            draft: &draft_json(&graph)?,
            actor: auth.user.id.as_str(),
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(row.try_into()?)))
}

pub(super) async fn read(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, workflow)): Path<(GatewayProjectId, WorkflowId)>,
) -> ApiResult<Json<WorkflowView>> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(false).await?;
    let row = db::find_workflow(&state.pool, project.scope(&workflow))
        .await?
        .ok_or_else(missing)?;
    Ok(Json(row.try_into()?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveInput {
    revision: i64,
    graph: Graph,
}

pub(super) async fn save(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, workflow)): Path<(GatewayProjectId, WorkflowId)>,
    Json(input): Json<SaveInput>,
) -> ApiResult<Json<WorkflowView>> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(false).await?;
    let scope = project.scope(&workflow);
    let row = db::save_draft(
        &state.pool,
        db::SaveDraft {
            scope,
            revision: input.revision,
            title: input.graph.title.trim(),
            draft: &draft_json(&input.graph)?,
        },
    )
    .await?;
    match row {
        Some(row) => Ok(Json(row.try_into()?)),
        None if db::find_workflow(&state.pool, scope).await?.is_none() => Err(missing()),
        None => Err(ApiError::Conflict(
            "the draft changed; reopen it before saving".into(),
        )),
    }
}

#[derive(Serialize)]
pub(super) struct Release {
    version: i32,
    draft_revision: i64,
    published_at: DateTime<Utc>,
}

pub(super) async fn versions(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, workflow)): Path<(GatewayProjectId, WorkflowId)>,
) -> ApiResult<Json<Vec<Release>>> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(false).await?;
    let scope = project.scope(&workflow);
    db::find_workflow(&state.pool, scope)
        .await?
        .ok_or_else(missing)?;
    let versions = db::list_versions(&state.pool, scope)
        .await?
        .into_iter()
        .map(|row| Release {
            version: row.version,
            draft_revision: row.draft_revision,
            published_at: row.published_at,
        })
        .collect();
    Ok(Json(versions))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublishInput {
    revision: i64,
    active_version: Option<i32>,
}

pub(super) async fn publish(
    State(state): State<AppState>,
    auth: AuthContext,
    Extension(security): Extension<Arc<super::Security>>,
    Query(query): Query<ConsoleQuery>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    Path((project, workflow)): Path<(GatewayProjectId, WorkflowId)>,
    Json(input): Json<PublishInput>,
) -> ApiResult<Json<WorkflowView>> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(true).await?;
    let scope = project.scope(&workflow);
    let mut tx = state.pool.begin().await?;
    gateway_routing_policies::lock_for_admission(&mut *tx, scope.account).await?;
    let row = db::lock_workflow(&mut *tx, scope)
        .await?
        .ok_or_else(missing)?;
    if row.revision != input.revision || row.active_version != input.active_version {
        return Err(ApiError::Conflict(
            "the draft or endpoint changed; reopen the workflow".into(),
        ));
    }
    let graph = decode(row.draft.clone())?;
    graph.compile().map_err(ApiError::BadRequest)?;
    security.tools.validate(&graph, scope.account, scope.project)?;
    let request_scope = project.request_scope();
    let policies = Policies::load(&mut *tx, request_scope).await?;
    let models = planner::models(&catalog.current(), providers.as_deref(), Some((&policies, request_scope)));
    planner::validate_models(&graph, &models).map_err(ApiError::BadRequest)?;
    if !db::has_passed_test(&mut *tx, scope, row.revision).await? {
        return Err(ApiError::Conflict(
            "complete a successful test for this draft revision".into(),
        ));
    }
    if let Some(active) = row.active_version {
        let previous = db::find_version(&mut *tx, scope, active)
            .await?
            .ok_or_else(missing)?;
        if !graph.contract_matches(&decode(previous.graph)?) {
            return Err(ApiError::Conflict(
                "the request or response contract changed; publish this graph as a new workflow"
                    .into(),
            ));
        }
        if previous.draft_revision == row.revision {
            return Ok(Json(row.try_into()?));
        }
    }
    let version = db::next_version(&mut *tx, scope).await?;
    db::insert_version(
        &mut *tx,
        db::NewVersion {
            scope,
            version,
            revision: row.revision,
            graph: &row.draft,
            actor: auth.user.id.as_str(),
        },
    )
    .await?;
    let row = db::configure_endpoint(
        &mut *tx,
        db::EndpointChange {
            scope,
            version: Some(version),
            paused: row.paused,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(row.try_into()?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Activation {
    expected_version: Option<i32>,
    version: i32,
    paused: bool,
}

pub(super) async fn activation(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, workflow)): Path<(GatewayProjectId, WorkflowId)>,
    Json(input): Json<Activation>,
) -> ApiResult<Json<WorkflowView>> {
    let project = Project {
        pool: &state.pool,
        auth: &auth,
        query: &query,
        project: &project,
    };
    project.authorize(true).await?;
    let scope = project.scope(&workflow);
    let mut tx = state.pool.begin().await?;
    let row = db::lock_workflow(&mut *tx, scope)
        .await?
        .ok_or_else(missing)?;
    if row.active_version != input.expected_version {
        return Err(ApiError::Conflict("the active version changed".into()));
    }
    db::find_version(&mut *tx, scope, input.version)
        .await?
        .ok_or_else(missing)?;
    let row = db::configure_endpoint(
        &mut *tx,
        db::EndpointChange {
            scope,
            version: Some(input.version),
            paused: input.paused,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(row.try_into()?))
}
