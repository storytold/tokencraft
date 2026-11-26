use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use db_queries::gateway_operations::{
    publication_snapshot, queue_snapshot, workflow_snapshot, PublicationSample, QueueSample,
    PUBLICATION_SAMPLE_LIMIT,
};
use enums::MemberRole;
use ids::{AccountId, GatewayProjectId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod workflows;
use workflows::WorkflowOperations;

use super::projects::{managed_project, OrganizationQuery};
use crate::{
    authz::require_account_role,
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    state::AppState,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OperationsQuery {
    account_id: String,
    project_id: Option<String>,
}

impl OperationsQuery {
    async fn authorize(self, state: &AppState, auth: &AuthContext) -> ApiResult<(AccountId, Option<GatewayProjectId>)> {
        let organization = OrganizationQuery::new(self.account_id);
        match self.project_id {
            Some(raw) => {
                let (account, project) = managed_project(state, auth, &organization, &raw).await?;
                Ok((account, Some(project.id)))
            }
            None => {
                let account = organization.account()?;
                require_account_role(&state.pool, &auth.user.id, &account, MemberRole::ADMINS).await?;
                Ok((account, None))
            }
        }
    }
}

#[derive(Serialize, ToSchema)]
pub(super) struct OperationsReport {
    observed_at: DateTime<Utc>,
    queues: Vec<QueueDepth>,
    workflows: WorkflowOperations,
}

#[derive(Serialize, ToSchema)]
struct QueueDepth {
    kind: QueueKind,
    provider: Option<String>,
    workload: Option<String>,
    count: i64,
    dispatch_paused: bool,
    oldest_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum QueueKind {
    Queued,
    Running,
    AwaitingReconciliation,
    WebhookPending,
    WebhookDead,
    ExportPending,
}

impl TryFrom<QueueSample> for QueueDepth {
    type Error = ApiError;

    fn try_from(row: QueueSample) -> ApiResult<Self> {
        let kind = match row.kind.as_str() {
            "queued" => QueueKind::Queued,
            "running" => QueueKind::Running,
            "awaiting_reconciliation" => QueueKind::AwaitingReconciliation,
            "webhook_pending" => QueueKind::WebhookPending,
            "webhook_dead" => QueueKind::WebhookDead,
            "export_pending" => QueueKind::ExportPending,
            _ => return Err(ApiError::Internal("unexpected queue kind".into())),
        };
        Ok(Self {
            kind,
            provider: row.provider,
            workload: row.workload,
            count: row.count,
            dispatch_paused: row.dispatch_paused,
            oldest_at: row.oldest_at,
        })
    }
}

#[utoipa::path(
    get, path = "/v1/operations/queues", tag = "meta", security(("session_cookie" = [])),
    params(
        ("account_id" = String, Query, description = "Organization to inspect."),
        ("project_id" = Option<String>, Query, description = "Limit the snapshot to one project."),
    ),
    responses((status = 200, description = "Current request, workflow, webhook, and export backlogs and recent workflow outcomes for the authorized scope.", body = OperationsReport)),
)]
pub(super) async fn queues(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OperationsQuery>,
) -> ApiResult<Json<OperationsReport>> {
    let (account, project) = query.authorize(&state, &auth).await?;
    let mut tx = state.pool.begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").await?;
    let rows = queue_snapshot(&mut *tx, &account, project.as_ref()).await?;
    let workflows = workflow_snapshot(&mut *tx, &account, project.as_ref()).await?;
    tx.commit().await?;
    let observed_at = rows
        .first()
        .map(|row| row.observed_at)
        .ok_or_else(|| ApiError::Internal("missing queue snapshot".into()))?;
    let queues = rows
        .into_iter()
        .filter(|row| row.count > 0)
        .map(QueueDepth::try_from)
        .collect::<ApiResult<_>>()?;
    Ok(Json(OperationsReport {
        observed_at,
        queues,
        workflows: workflows.into(),
    }))
}

#[derive(Serialize, ToSchema)]
pub(super) struct PublicationReport {
    observed_at: DateTime<Utc>,
    sampled_admissions: i64,
    sample_limit: i64,
    totals: PublicationTiming,
    groups: Vec<PublicationGroup>,
}

#[derive(Serialize, ToSchema)]
struct PublicationTiming {
    observed: i64,
    published: i64,
    pending: i64,
    unconfirmed: i64,
    timing_samples: i64,
    p95_ms: Option<i64>,
    oldest_pending_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
struct PublicationGroup {
    provider: String,
    workload: String,
    timing: PublicationTiming,
}

impl From<&PublicationSample> for PublicationTiming {
    fn from(row: &PublicationSample) -> Self {
        Self {
            observed: row.observed,
            published: row.published,
            pending: row.pending,
            unconfirmed: row.unconfirmed,
            timing_samples: row.timing_samples,
            p95_ms: row.p95_ms,
            oldest_pending_at: row.oldest_pending_at,
        }
    }
}

#[utoipa::path(
    get, path = "/v1/operations/artifact-publication", tag = "meta", security(("session_cookie" = [])),
    params(
        ("account_id" = String, Query, description = "Organization to inspect."),
        ("project_id" = Option<String>, Query, description = "Limit the snapshot to one project."),
    ),
    responses((status = 200, description = "Timing and outcome evidence for the recent fal and Kinovi queue admissions.", body = PublicationReport)),
)]
pub(super) async fn publication(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OperationsQuery>,
) -> ApiResult<Json<PublicationReport>> {
    let (account, project) = query.authorize(&state, &auth).await?;
    let rows = publication_snapshot(&state.pool, &account, project.as_ref()).await?;
    let total = rows.first().filter(|row| row.total)
        .ok_or_else(|| ApiError::Internal("missing publication snapshot".into()))?;
    let groups = rows.iter().skip(1).map(|row| PublicationGroup {
        provider: row.provider.clone().unwrap_or_else(|| "other".to_owned()),
        workload: row.workload.clone().unwrap_or_else(|| "other".to_owned()),
        timing: row.into(),
    }).collect();
    Ok(Json(PublicationReport {
        observed_at: total.observed_at,
        sampled_admissions: total.sampled_admissions,
        sample_limit: PUBLICATION_SAMPLE_LIMIT,
        totals: total.into(),
        groups,
    }))
}
