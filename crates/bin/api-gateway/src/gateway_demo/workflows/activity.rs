use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use ids::{AccountId, GatewayProjectId, GatewayRequestId, WorkflowRunId};
use serde::{Deserialize, Serialize};
use sqlx::types::Json as SqlJson;

use crate::{error::{ApiError, ApiResult}, middleware::AuthContext, state::AppState};

use super::management::{ConsoleQuery, Project};

const PAGE_SIZE: usize = 25;

const WORKFLOW_REQUEST: &str = "w.id = r.actor_id
    AND w.account_id = r.account_id AND w.project_id = r.project_id AND w.workflow_id = r.app_ref
    AND r.client_ref IS NOT DISTINCT FROM CASE WHEN w.version IS NOT NULL THEN w.actor_id END";

pub(super) const REQUEST_STATUS: &str = "CASE
    WHEN r.uncertain_at IS NOT NULL THEN 'awaiting_reconciliation'
    WHEN r.failed_at IS NOT NULL THEN 'failed'
    WHEN r.canceled_at IS NOT NULL THEN 'canceled'
    WHEN r.succeeded_at IS NOT NULL THEN 'succeeded'
    WHEN (CASE WHEN r.adapter = 'mock' THEN a.submitted_at ELSE r.dispatched_at END) IS NOT NULL THEN 'running'
    ELSE 'queued'
END";

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum Kind {
    #[default]
    All,
    Workflows,
}

#[derive(Deserialize)]
pub(super) struct ActivityQuery {
    account_id: AccountId,
    before: Option<String>,
    #[serde(default)]
    kind: Kind,
}

#[derive(Serialize, sqlx::FromRow)]
struct ActivityEntry {
    kind: String,
    id: String,
    workflow_id: Option<String>,
    title: String,
    provider: Option<String>,
    version: Option<i32>,
    draft_revision: Option<i64>,
    status: String,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub(super) struct ActivityPage {
    entries: Vec<ActivityEntry>,
    next_before: Option<String>,
}

fn parse_cursor(cursor: Option<&str>) -> ApiResult<(Option<DateTime<Utc>>, Option<&str>)> {
    let Some(cursor) = cursor else { return Ok((None, None)) };
    let invalid = || ApiError::BadRequest("invalid request activity cursor".into());
    let (time, id) = cursor.split_once('|').ok_or_else(invalid)?;
    if !(1..=64).contains(&id.len())
        || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(invalid());
    }
    let time = time.parse::<DateTime<Utc>>().map_err(|_| invalid())?;
    Ok((Some(time), Some(id)))
}

pub(super) async fn list(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ActivityQuery>,
    Path(project): Path<GatewayProjectId>,
) -> ApiResult<Json<ActivityPage>> {
    Project {
        pool: &state.pool,
        auth: &auth,
        query: &ConsoleQuery { account_id: query.account_id.clone() },
        project: &project,
    }
    .authorize(false)
    .await?;

    let (before_time, before_id) = parse_cursor(query.before.as_deref())?;
    let query_sql = format!(
        "WITH models AS (
            SELECT 'request'::text AS kind, r.id, NULL::text AS workflow_id,
                   r.model AS title, r.provider, NULL::integer AS version,
                   NULL::bigint AS draft_revision, r.created_at,
                   {REQUEST_STATUS} AS status
            FROM gateway_requests r
            LEFT JOIN gateway_provider_attempts a
              ON a.account_id = r.account_id AND a.request_id = r.id AND a.attempt_number = 1
            WHERE r.account_id = $1 AND r.project_id = $2 AND $5
              AND NOT EXISTS (SELECT 1 FROM workflow_runs w WHERE {WORKFLOW_REQUEST})
              AND ($3::timestamptz IS NULL OR (r.created_at, r.id) < ($3, $4::text))
            ORDER BY r.created_at DESC, r.id DESC LIMIT $6
        ), runs AS (
            SELECT 'workflow'::text AS kind, w.id, w.workflow_id,
                   COALESCE(w.graph->>'title', 'Workflow run') AS title, NULL::text AS provider,
                   w.version, w.draft_revision, w.created_at, w.status
            FROM workflow_runs w
            WHERE w.account_id = $1 AND w.project_id = $2
              AND ($3::timestamptz IS NULL OR (w.created_at, w.id) < ($3, $4::text))
            ORDER BY w.created_at DESC, w.id DESC LIMIT $6
        )
        SELECT * FROM models UNION ALL SELECT * FROM runs
        ORDER BY created_at DESC, id DESC LIMIT $6"
    );
    let mut entries = sqlx::query_as::<_, ActivityEntry>(&query_sql)
    .bind(query.account_id.as_str())
    .bind(project.as_str())
    .bind(before_time)
    .bind(before_id)
    .bind(matches!(query.kind, Kind::All))
    .bind((PAGE_SIZE + 1) as i64)
    .fetch_all(&state.pool)
    .await?;
    let next_before = if entries.len() > PAGE_SIZE {
        entries.truncate(PAGE_SIZE);
        entries.last().map(|row| format!("{}|{}", row.created_at.to_rfc3339(), row.id))
    } else {
        None
    };
    Ok(Json(ActivityPage { entries, next_before }))
}

#[derive(Serialize, sqlx::FromRow)]
pub(super) struct RequestParent {
    workflow_id: String,
    run_id: String,
    title: String,
}

pub(super) async fn parent(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, request)): Path<(GatewayProjectId, GatewayRequestId)>,
) -> ApiResult<Json<Option<RequestParent>>> {
    Project { pool: &state.pool, auth: &auth, query: &query, project: &project }
        .authorize(false)
        .await?;
    let query_sql = format!(
        "SELECT w.workflow_id, w.id AS run_id, COALESCE(w.graph->>'title', 'Workflow run') AS title
         FROM gateway_requests r
         JOIN workflow_runs w ON {WORKFLOW_REQUEST}
         WHERE r.account_id = $1 AND r.project_id = $2 AND r.id = $3"
    );
    let parent = sqlx::query_as::<_, RequestParent>(&query_sql)
    .bind(query.account_id.as_str())
    .bind(project.as_str())
    .bind(request.as_str())
    .fetch_optional(&state.pool)
    .await?;
    Ok(Json(parent))
}


#[derive(Deserialize, Serialize)]
struct ChildRequest {
    id: String,
    title: String,
    provider: String,
    status: String,
    created_at: DateTime<Utc>,
}

#[derive(Serialize, sqlx::FromRow)]
pub(super) struct RunChildren {
    status: String,
    requests: SqlJson<Vec<ChildRequest>>,
}

pub(super) async fn children(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ConsoleQuery>,
    Path((project, run)): Path<(GatewayProjectId, WorkflowRunId)>,
) -> ApiResult<Json<RunChildren>> {
    Project { pool: &state.pool, auth: &auth, query: &query, project: &project }
        .authorize(false)
        .await?;
    let query_sql = format!(
        "SELECT w.status, (
            SELECT COALESCE(jsonb_agg(jsonb_build_object(
                'id', r.id, 'title', r.model, 'provider', r.provider,
                'status', {REQUEST_STATUS}, 'created_at', r.created_at
            ) ORDER BY r.created_at, r.sid), '[]'::jsonb)
            FROM gateway_requests r
            LEFT JOIN gateway_provider_attempts a
              ON a.account_id = r.account_id AND a.request_id = r.id AND a.attempt_number = 1
            WHERE {WORKFLOW_REQUEST}
         ) AS requests
         FROM workflow_runs w
         WHERE w.account_id = $1 AND w.project_id = $2 AND w.id = $3"
    );
    let children = sqlx::query_as::<_, RunChildren>(&query_sql)
        .bind(query.account_id.as_str())
        .bind(project.as_str())
        .bind(run.as_str())
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(super::management::missing)?;
    Ok(Json(children))
}
