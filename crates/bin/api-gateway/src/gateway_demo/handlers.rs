use std::{collections::BTreeMap, sync::Arc};

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{header::LOCATION, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use db_queries::gateway_requests::{list_requests, RequestDiagnostic, RequestFilter};
use ids::GatewayRequestId;
use serde::{Deserialize, Serialize};

use super::access::{self, GatewayCaller, Permission, ScopeQuery};
use super::catalog::{alternate_model_id, public_model_id, valid_identifier, Catalog, ModelCatalog, ModelQuote, Provider};
use super::catalog_refresh::CatalogStore;
use super::providers::ProviderRuntime;
use super::quote::{Quote, QuoteInput};
use super::routing_policy::Policies;
use super::requests::{
    self, AcceptedRequest, CostState, ModelRequestView, NativeProviderResult, RequestEvents,
    RequestStatus, RequestView, Submission,
};
use super::webhooks::WebhookConfig;
use crate::error::{ApiError, ApiResult};
use crate::middleware::Caller;
use crate::state::AppState;

pub(crate) const REQUEST_ID_HEADER: &str = "tokencraft-request-id";

pub(super) fn single_header<'a>(headers: &'a HeaderMap, name: &'static str) -> ApiResult<Option<&'a str>> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    let Some(value) = values.next() else { return Ok(None) };
    value.to_str().ok().filter(|_| values.next().is_none()).map(Some)
        .ok_or_else(|| ApiError::BadRequest(format!("supply one valid {name} header")))
}

pub(super) fn idempotency_key(headers: &HeaderMap) -> ApiResult<&str> {
    single_header(headers, "idempotency-key").ok().flatten()
        .ok_or_else(|| ApiError::BadRequest("exactly one Idempotency-Key is required".into()))
}

#[derive(Serialize, utoipa::ToSchema)]
struct CatalogOffers<'a> {
    #[serde(flatten)]
    catalog: &'a Catalog,
    base_revision: &'a str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    rate_valid_until: BTreeMap<&'a str, DateTime<Utc>>,
}

#[utoipa::path(
    get, path = "/v1/catalog/offers", operation_id = "listPublicCatalogOffers", tag = "catalog",
    summary = "List this gateway's public offers",
    description = "Includes configured routing rates and their validity windows, when present. Offers restricted to an account are excluded. The ArtCraft reference catalog is distributed separately.",
    responses((status = 200, description = "Public offers, active revision, and base revision.", body = CatalogOffers)),
)]
pub async fn catalog(Extension(catalog): Extension<Arc<CatalogStore>>) -> Response {
    let current = catalog.current().public_view();
    let expires = current.rates.iter()
        .map(|(id, rate)| (id.as_str(), rate.valid_until))
        .collect();
    Json(CatalogOffers {
        catalog: &current,
        base_revision: &catalog.reviewed().revision,
        rate_valid_until: expires,
    }).into_response()
}

#[utoipa::path(
    get, path = "/v1/catalog/models", operation_id = "listGatewayModels", tag = "native",
    responses(
        (status = 200, description = "Configured model IDs and supported workloads.", body = ModelCatalog),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub(super) async fn models(Extension(catalog): Extension<Arc<CatalogStore>>) -> Json<ModelCatalog> {
    let catalog = catalog.current();
    Json(catalog.model_catalog(None))
}

#[utoipa::path(
    get, path = "/v1/catalog/models/scoped", operation_id = "listProjectModels", tag = "native",
    summary = "List models available to a project",
    description = "Includes model and provider IDs shared with the project and applies provider allowlists and connection exclusions. A project key requires requests:submit. Request a scoped quote for a specific workload and current availability.",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(ScopeQuery),
    responses(
        (status = 200, description = "Project-visible model and provider IDs and supported workloads.", body = ModelCatalog),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub(super) async fn scoped_models(
    State(state): State<AppState>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    caller: GatewayCaller,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<ModelCatalog>> {
    let access = access::resolve(&state, caller, query, Permission::SubmitRequests).await?;
    let scope = access.scope();
    let policies = Policies::load(&state.pool, scope).await?;
    let catalog = catalog.current();
    Ok(Json(catalog.model_catalog(Some((&policies, scope)))))
}

#[utoipa::path(
    post, path = "/v1/catalog/quotes", operation_id = "quotePublicModel", tag = "catalog",
    summary = "Estimate a public model request",
    description = "Compares eligible public routes for a named model. A provider selection in the body does not constrain this view. No provider work is started.",
    request_body = QuoteInput,
    responses(
        (status = 200, description = "Model estimate without provider and offer IDs on candidates.", body = ModelQuote),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub(super) async fn model_quote(
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Json(mut input): Json<QuoteInput>,
) -> ApiResult<Json<ModelQuote>> {
    input.provider = None;
    input.model = public_model_id(&input.model).to_owned();
    let catalog = catalog.current();
    Ok(Json(catalog.model_quote(catalog.quote(input)?)))
}

#[utoipa::path(
    post, path = "/v1/quotes", operation_id = "quoteGatewayRequest", tag = "native",
    request_body = QuoteInput,
    responses(
        (status = 200, description = "Public estimate and eligible offers. No provider work is started.", body = Quote),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub async fn quote(
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Json(input): Json<QuoteInput>,
) -> ApiResult<Json<Quote>> {
    Ok(Json(catalog.current().quote(input)?))
}

#[utoipa::path(
    post, path = "/v1/requests", operation_id = "submitGatewayRequest", tag = "native",
    summary = "Submit a quoted request",
    description = "Use the quoted input and quote ID. A project key requires requests:submit. Session callers include the account and project scope in the body. Persist the idempotency key to retry the same intent.",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(
        ("Idempotency-Key" = String, Header, description = "Stable key using 8 to 128 ASCII letters, numbers, or . _ : -."),
        ("traceparent" = Option<String>, Header, description = "Optional W3C trace link to the caller."),
    ),
    request_body = Submission,
    responses(
        (status = 202, description = "A new request was accepted.", body = AcceptedRequest,
            headers(
                ("Location" = String, description = "Scope-aware URL for polling the request."),
                ("TokenCraft-Request-Id" = String, description = "Canonical request ID."),
                ("TokenCraft-Trace-Id" = String, description = "Gateway trace ID, when recorded."),
            )),
        (status = 200, description = "The original receipt was recovered from an identical replay.", body = AcceptedRequest,
            headers(
                ("Location" = String, description = "Scope-aware URL for polling the request."),
                ("TokenCraft-Request-Id" = String, description = "Canonical request ID."),
                ("TokenCraft-Trace-Id" = String, description = "Gateway trace ID, when recorded."),
            )),
        (status = 409, description = "Stale quote, changed intent, or exceeded estimated budget.", body = crate::error::GatewayError),
        (status = 429, description = "An organization, project, or provider active request limit was reached.", body = crate::error::GatewayError),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub async fn submit(
    State(state): State<AppState>,
    Extension(seals): Extension<Arc<super::quote_seals::QuoteSeals>>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(webhooks): Extension<Option<Arc<WebhookConfig>>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    caller: GatewayCaller,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(mut submission): Json<Submission>,
) -> ApiResult<(StatusCode, HeaderMap, Json<AcceptedRequest>)> {
    if submission.quote_id.is_none() && uri.path() != "/v1/model-requests" {
        return Err(ApiError::BadRequest("quote_id is required on this endpoint".into()));
    }
    if submission.pinned_offer_id.is_some() && uri.path() != "/v1/requests" {
        return Err(ApiError::BadRequest("offer pins require /v1/requests".into()));
    }
    let scope = ScopeQuery {
        account_id: submission.account_id.take(),
        project_id: submission.project_id.take(),
    };
    let session = matches!(&caller, GatewayCaller::Account(Caller::Session(_)));
    let access = access::resolve(&state, caller, scope, Permission::SubmitRequests).await?;
    let key = idempotency_key(&headers)?;
    let (mut request, replay) = requests::submit(
        &state.pool,
        &catalog.current(),
        webhooks.as_deref(),
        providers.as_deref(),
        Some(&seals),
        &access,
        key,
        submission.into(),
    )
    .await?;
    if uri.path() == "/v1/model-requests" {
        request.status_url = format!("/v1/model-requests/{}", request.id);
    }
    if session {
        let scope = access.scope();
        request.status_url = format!(
            "{}?account_id={}&project_id={}",
            request.status_url, scope.account, scope.project,
        );
    }
    receipt(request, replay)
}

pub(super) fn receipt(request: AcceptedRequest, replay: bool) -> ApiResult<(StatusCode, HeaderMap, Json<AcceptedRequest>)> {
    let status = if replay {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    let mut response = HeaderMap::new();
    response.insert(
        LOCATION,
        HeaderValue::from_str(&request.status_url)
            .map_err(|_| ApiError::Internal("invalid request location".into()))?,
    );
    response.insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(request.id.as_str())
            .map_err(|_| ApiError::Internal("invalid request id header".into()))?,
    );
    request.append_trace_header(&mut response);
    Ok((status, response, Json(request)))
}

#[utoipa::path(
    get, path = "/v1/requests/{id}", operation_id = "getGatewayRequest", tag = "native",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(
        ("id" = String, Path, description = "Durable request ID."),
        ("If-None-Match" = Option<String>, Header, description = "Validator from a previous scoped status response."),
        ScopeQuery,
    ),
    responses(
        (status = 200, description = "Current lifecycle state, stored quote, and provider evidence.", body = RequestView,
            headers(
                ("ETag" = String, description = "Validator for the returned representation."),
                ("Retry-After" = u32, description = "Suggested delay in seconds while the request is queued or running."),
            )),
        (status = 304, description = "The authorized status representation has not changed.",
            headers(
                ("ETag" = String, description = "Current representation validator."),
                ("Retry-After" = u32, description = "Suggested delay in seconds while the request is queued or running."),
            )),
        (status = 404, description = "Request is outside scope or unavailable.", body = crate::error::GatewayError),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub async fn detail(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let request = scoped_detail(&state, caller, scope, &raw).await?;
    super::polling::status_response(&headers, request.status, &request)
}

async fn scoped_detail(
    state: &AppState,
    caller: GatewayCaller,
    scope: ScopeQuery,
    raw: &str,
) -> ApiResult<RequestView> {
    let access = access::resolve(state, caller, scope, Permission::ReadRequests).await?;
    requests::get(&state.pool, access.scope(), &parse_request_id(raw)?).await
}

#[utoipa::path(
    get, path = "/v1/requests/{id}/events", operation_id = "getGatewayRequestEvents", tag = "native",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Durable request ID."), ScopeQuery),
    responses(
        (status = 200, description = "Ordered request timeline.", body = RequestEvents),
        (status = 404, description = "Request is outside scope or unavailable.", body = crate::error::GatewayError),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub async fn events(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
) -> ApiResult<Json<RequestEvents>> {
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let id = parse_request_id(&raw)?;
    Ok(Json(
        requests::get(&state.pool, access.scope(), &id)
            .await?
            .into_events(),
    ))
}

pub(super) async fn model_detail(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let request = scoped_detail(&state, caller, scope, &raw).await?;
    super::polling::status_response(&headers, request.status, &ModelRequestView::from(request))
}

#[utoipa::path(
    get, path = "/v1/requests/{id}/result", operation_id = "getGatewayRequestResult", tag = "native",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Durable request ID."), ScopeQuery),
    responses(
        (status = 200, description = "Stored provider result with its expiry.", body = NativeProviderResult),
        (status = 404, description = "Result unavailable, expired, removed, or outside scope.", body = crate::error::GatewayError),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub async fn result(
    State(state): State<AppState>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
) -> ApiResult<Json<NativeProviderResult>> {
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let id = parse_request_id(&raw)?;
    Ok(Json(
        requests::native_provider_result(&state.pool, providers.as_deref(), access.scope(), &id)
            .await?,
    ))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelBody {}

#[utoipa::path(
    post, path = "/v1/requests/{id}/cancel", operation_id = "cancelGatewayRequest", tag = "native",
    description = "A project key requires requests:cancel. Active requests can be canceled before any paid provider submission. An already-canceled request returns its current state.",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Durable request ID."), ScopeQuery),
    request_body = CancelBody,
    responses(
        (status = 200, description = "Canceled request.", body = RequestView),
        (status = 409, description = "Current request state does not allow cancellation.", body = crate::error::GatewayError),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub async fn cancel(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
    Json(_body): Json<CancelBody>,
) -> ApiResult<Json<RequestView>> {
    let access = access::resolve(&state, caller, scope, Permission::CancelRequests).await?;
    let id = parse_request_id(&raw)?;
    Ok(Json(
        requests::cancel(&state.pool, access.scope(), &id).await?,
    ))
}

pub(super) async fn model_cancel(
    state: State<AppState>,
    caller: GatewayCaller,
    scope: Query<ScopeQuery>,
    path: Path<String>,
    body: Json<CancelBody>,
) -> ApiResult<Json<ModelRequestView>> {
    let Json(request) = cancel(state, caller, scope, path, body).await?;
    Ok(Json(request.into()))
}

#[derive(Default, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct HistoryQuery {
    /// Account ID for session callers.
    account_id: Option<String>,
    /// Project ID. Project keys already identify their project.
    project_id: Option<String>,
    /// Exact client reference inside the caller’s authorized scope.
    client_ref: Option<String>,
    /// Only requests without a client reference. Omit client_ref when using this filter.
    #[serde(default)]
    client_unattributed: bool,
    /// Use the previous page’s next_after value.
    pub(super) after: Option<String>,
    /// Exact application reference.
    pub(super) app_ref: Option<String>,
    /// Only requests without an app reference. Omit app_ref when using this filter.
    #[serde(default)]
    app_unattributed: bool,
    /// Exact gateway or caller trace ID in the authorized scope.
    trace_id: Option<String>,
    /// Canonical or stored model ID.
    model: Option<String>,
    /// Match the stored model ID exactly, including when it has a catalog alias.
    #[serde(default)]
    exact_model: bool,
    /// Selected gateway provider.
    provider: Option<Provider>,
    #[param(schema_with = requests::request_status_schema)]
    status: Option<RequestStatus>,
    #[param(schema_with = requests::request_cost_state_schema)]
    cost_state: Option<CostState>,
    /// Filter for recorded warnings, or include failed requests as well.
    #[param(schema_with = request_diagnostic_schema)]
    diagnostic: Option<RequestDiagnostic>,
    /// Inclusive UTC date. Supply both dates for a window of up to 92 days.
    from: Option<NaiveDate>,
    /// Exclusive UTC date. Omit both dates to read the complete history.
    to: Option<NaiveDate>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RequestPage {
    pub(super) requests: Vec<RequestView>,
    #[schema(value_type = Option<String>)]
    pub(super) next_after: Option<GatewayRequestId>,
    /// The applied diagnostic filter, if requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = request_diagnostic_schema)]
    diagnostic: Option<RequestDiagnostic>,
}

#[derive(Serialize)]
pub(super) struct ModelRequestPage {
    requests: Vec<ModelRequestView>,
    next_after: Option<GatewayRequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<RequestDiagnostic>,
}

fn request_diagnostic_schema() -> utoipa::openapi::Object {
    requests::enum_schema([RequestDiagnostic::Warnings, RequestDiagnostic::FailedOrWarning])
}

pub(super) async fn model_list(
    state: State<AppState>,
    caller: GatewayCaller,
    query: Query<HistoryQuery>,
) -> ApiResult<Json<ModelRequestPage>> {
    let Json(page) = list(state, caller, query).await?;
    Ok(Json(ModelRequestPage {
        requests: page.requests.into_iter().map(Into::into).collect(),
        next_after: page.next_after,
        diagnostic: page.diagnostic,
    }))
}

#[utoipa::path(
    get, path = "/v1/requests", operation_id = "listGatewayRequests", tag = "native",
    summary = "List scoped requests",
    description = "Returns up to 25 requests in newest-first order. A project key requires requests:read. Use next_after with the same filters for the next page.",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(HistoryQuery),
    responses(
        (status = 200, description = "Page of stored requests with cost evidence and lifecycle status.", body = RequestPage),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub async fn list(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(mut query): Query<HistoryQuery>,
) -> ApiResult<Json<RequestPage>> {
    let scope = ScopeQuery {
        account_id: query.account_id.take(),
        project_id: query.project_id.take(),
    };
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let scope = access.for_client(query.client_ref.as_deref())?;
    Ok(Json(history(&state.pool, scope, &query).await?))
}

pub(super) async fn history(
    pool: &db::PgPool,
    scope: db_queries::gateway_requests::RequestScope<'_>,
    query: &HistoryQuery,
) -> ApiResult<RequestPage> {
    const PAGE_SIZE: usize = 25;
    let after = query.after.as_deref().map(parse_request_id).transpose()?;
    let app_ref = query.app_ref.as_deref();
    requests::validate_app_ref(app_ref)?;
    if query.client_unattributed && query.client_ref.is_some() {
        return Err(ApiError::BadRequest(
            "use client_ref or client_unattributed".into(),
        ));
    }
    if query.client_unattributed && scope.client_ref.is_some() {
        return Err(ApiError::NotFound("client not found".into()));
    }
    if query.app_unattributed && app_ref.is_some() {
        return Err(ApiError::BadRequest(
            "use app_ref or app_unattributed".into(),
        ));
    }
    let trace_id = query.trace_id.as_deref();
    if trace_id.is_some_and(|id| !crate::middleware::trace_context::valid_trace_id(id)) {
        return Err(ApiError::BadRequest("trace_id must contain 32 lowercase hex characters and be nonzero".into()));
    }
    let model = query.model.as_deref();
    if model.is_some_and(|value| !valid_identifier(value)) || (query.exact_model && model.is_none()) {
        return Err(ApiError::BadRequest("invalid model".into()));
    }
    let (from, to) = match (query.from, query.to) {
        (None, None) => (None, None),
        (Some(from), Some(to)) if (1..=92).contains(&(to - from).num_days()) => (
            Some(from.and_time(NaiveTime::MIN).and_utc()),
            Some(to.and_time(NaiveTime::MIN).and_utc()),
        ),
        _ => {
            return Err(ApiError::BadRequest(
                "use both from and to for a positive UTC date range of up to 92 days".into(),
            ))
        }
    };
    let rows = list_requests(
        pool,
        scope,
        RequestFilter {
            after: after.as_ref(),
            app_ref,
            client_unattributed: query.client_unattributed,
            app_unattributed: query.app_unattributed,
            trace_id,
            model,
            alternate_model: if query.exact_model {
                None
            } else {
                model.and_then(alternate_model_id)
            },
            provider: query.provider.as_ref().map(Provider::as_str),
            status: query.status,
            cost_state: query.cost_state,
            diagnostic: query.diagnostic,
            from,
            to,
        },
        (PAGE_SIZE + 1) as i64,
    )
    .await?;
    let next_after = (rows.len() > PAGE_SIZE).then(|| rows[PAGE_SIZE - 1].id.clone());
    let requests = rows
        .into_iter()
        .take(PAGE_SIZE)
        .map(RequestView::try_from)
        .collect::<ApiResult<_>>()?;
    Ok(RequestPage {
        requests,
        next_after,
        diagnostic: query.diagnostic,
    })
}

pub(super) fn parse_request_id(raw: &str) -> ApiResult<GatewayRequestId> {
    raw.parse()
        .map_err(|_| ApiError::BadRequest("invalid request id".into()))
}
