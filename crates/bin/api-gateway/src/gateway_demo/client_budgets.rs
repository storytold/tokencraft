use std::collections::BTreeMap;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use chrono::{DateTime, Utc};
use db_queries::{
    account_audit_logs::{self, NewAccountAuditLog},
    gateway_requests::{
        client_budgets::{self, ClientBudget},
        monthly_budget, RequestScope,
    },
    gateway_routing_policies,
};
use enums::AuditEvent;
use ids::{AccountId, GatewayProjectId};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

use super::{
    projects::{managed_project, OrganizationQuery},
    requests::validate_client_ref,
    routing_policy::MAX_MONTHLY_BUDGET_NANOS,
};
use crate::{
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    state::AppState,
};

const PAGE_SIZE: usize = 25;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ListQuery {
    account_id: String,
    after: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub(super) struct BudgetPage {
    budgets: Vec<BudgetSetting>,
    next_after: Option<String>,
}

#[derive(Serialize, ToSchema)]
struct BudgetSetting {
    client_ref: String,
    revision: i32,
    limit_usd_nanos: Option<i64>,
}

impl From<ClientBudget> for BudgetSetting {
    fn from(row: ClientBudget) -> Self {
        Self {
            client_ref: row.client_ref,
            revision: row.revision,
            limit_usd_nanos: row.limit_usd_nanos,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub(super) struct BudgetView {
    #[serde(flatten)]
    setting: BudgetSetting,
    reserved_usd_nanos: String,
    resets_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BudgetUpdate {
    expected_revision: i32,
    /// Send null to clear the limit.
    #[serde(deserialize_with = "required_limit")]
    #[schema(required = true)]
    limit_usd_nanos: Option<i64>,
}

fn required_limit<'de, D: Deserializer<'de>>(value: D) -> Result<Option<i64>, D::Error> {
    Option::deserialize(value)
}

#[utoipa::path(
    get, path = "/v1/projects/{project}/client-budgets", operation_id = "listProjectClientBudgets", tag = "native",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
        ("after" = Option<String>, Query, description = "Use the previous page's next_after value."),
    ),
    responses((status = 200, description = "Configured client estimate budgets.", body = BudgetPage)),
)]
pub(super) async fn list(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(project): Path<String>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<BudgetPage>> {
    let organization = OrganizationQuery::new(query.account_id);
    let (account, project) = managed_project(&state, &auth, &organization, &project).await?;
    validate_client_ref(query.after.as_deref())?;
    let rows = client_budgets::list(
        &state.pool, &account, &project.id, query.after.as_deref(), (PAGE_SIZE + 1) as i64,
    )
    .await?;
    let next_after = (rows.len() > PAGE_SIZE).then(|| rows[PAGE_SIZE - 1].client_ref.clone());
    Ok(Json(BudgetPage {
        budgets: rows.into_iter().take(PAGE_SIZE).map(Into::into).collect(),
        next_after,
    }))
}

#[utoipa::path(
    get, path = "/v1/projects/{project}/client-budgets/{client_ref}", operation_id = "getProjectClientBudget", tag = "native",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("client_ref" = String, Path, description = "Exact client reference."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    responses((status = 200, description = "Client estimate budget and current UTC month reservation.", body = BudgetView)),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((project, client_ref)): Path<(String, String)>,
    Query(query): Query<OrganizationQuery>,
) -> ApiResult<Json<BudgetView>> {
    let (account, project) = managed_project(&state, &auth, &query, &project).await?;
    let mut tx = state.pool.begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").await?;
    let budget = current(&mut tx, &account, &project.id, client_ref).await?;
    let report = view(&mut tx, &account, &project.id, budget).await?;
    tx.commit().await?;
    Ok(Json(report))
}

#[utoipa::path(
    put, path = "/v1/projects/{project}/client-budgets/{client_ref}", operation_id = "setProjectClientBudget", tag = "native",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("client_ref" = String, Path, description = "Exact client reference."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    request_body = BudgetUpdate,
    responses(
        (status = 200, description = "Current saved estimate budget and month reservation.", body = BudgetView),
        (status = 409, description = "The client budget revision changed."),
    ),
)]
pub(super) async fn update(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((project, client_ref)): Path<(String, String)>,
    Query(query): Query<OrganizationQuery>,
    Json(input): Json<BudgetUpdate>,
) -> ApiResult<Json<BudgetView>> {
    let (account, project) = managed_project(&state, &auth, &query, &project).await?;
    if !(0..i32::MAX).contains(&input.expected_revision) {
        return Err(ApiError::BadRequest("invalid client budget revision".into()));
    }
    if input.limit_usd_nanos.is_some_and(|limit| !(0..=MAX_MONTHLY_BUDGET_NANOS).contains(&limit)) {
        return Err(ApiError::BadRequest("monthly estimated budget must be between $0 and $1,000,000".into()));
    }
    let mut tx = state.pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    gateway_routing_policies::lock_for_update(&mut *tx, &account).await?;
    let mut budget = current(&mut tx, &account, &project.id, client_ref).await?;
    if budget.revision != input.expected_revision {
        return Err(ApiError::Conflict("client budget changed; reload its current revision".into()));
    }
    if budget.limit_usd_nanos != input.limit_usd_nanos {
        budget.revision += 1;
        budget.limit_usd_nanos = input.limit_usd_nanos;
        client_budgets::save(&mut *tx, &account, &project.id, &budget).await?;
        account_audit_logs::create_account_audit_log(
            &mut *tx,
            NewAccountAuditLog {
                account_id: &account,
                actor_user_id: Some(&auth.user.id),
                user_id: None,
                event: AuditEvent::GatewayClientBudgetChanged,
                target_type: Some("gateway_project"),
                target_id: Some(project.id.as_str()),
                data: Some(serde_json::json!({
                    "client_ref": budget.client_ref,
                    "revision": budget.revision,
                    "limit_usd_nanos": budget.limit_usd_nanos,
                })),
                ip: None,
            },
        )
        .await?;
    }
    let report = view(&mut tx, &account, &project.id, budget).await?;
    tx.commit().await?;
    Ok(Json(report))
}

pub(super) struct AdmissionLimits<'a> {
    pub evidence: Option<(i32, i64)>,
    pub clients: BTreeMap<&'a str, i64>,
}

pub(super) async fn admission_limits<'a>(
    conn: &mut sqlx::PgConnection,
    scope: RequestScope<'a>,
    application: Option<&'a str>,
) -> sqlx::Result<AdmissionLimits<'a>> {
    let mut limits = AdmissionLimits { evidence: None, clients: BTreeMap::new() };
    let application = application.filter(|reference| Some(*reference) != scope.client_ref);
    for reference in scope.client_ref.into_iter().chain(application) {
        let budget = client_budgets::find(&mut *conn, scope.account, scope.project, reference).await?;
        if let Some((revision, limit)) = budget.and_then(|row| row.limit_usd_nanos.map(|cap| (row.revision, cap))) {
            limits.clients.insert(reference, limit);
            if Some(reference) == scope.client_ref {
                limits.evidence = Some((revision, limit));
            }
        }
    }
    Ok(limits)
}

async fn current(
    conn: &mut sqlx::PgConnection,
    account: &AccountId,
    project: &GatewayProjectId,
    client_ref: String,
) -> ApiResult<ClientBudget> {
    validate_client_ref(Some(&client_ref))?;
    Ok(client_budgets::find(conn, account, project, &client_ref)
        .await?
        .unwrap_or(ClientBudget { client_ref, revision: 0, limit_usd_nanos: None }))
}

async fn view(
    conn: &mut sqlx::PgConnection,
    account: &AccountId,
    project: &GatewayProjectId,
    budget: ClientBudget,
) -> ApiResult<BudgetView> {
    let usage = monthly_budget::usage(conn, account, Some(project), Some(&budget.client_ref), budget.limit_usd_nanos).await?;
    Ok(BudgetView {
        setting: budget.into(),
        reserved_usd_nanos: usage.reserved_usd_nanos,
        resets_at: usage.resets_at,
    })
}
