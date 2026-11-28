use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Extension, Json, Router,
};
use db_queries::{
    gateway_requests::RequestScope,
    gateway_webhooks::{self, NewSubscription, RotationReceipt},
};
use enums::AuditEvent;
use ids::{AccountId, GatewayProjectId, GatewayWebhookDeliveryId, GatewayWebhookSubscriptionId, WorkflowAppId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use super::super::{
    access::parse_id,
    projects::{audit, managed_project, validate_name, OrganizationQuery},
    handlers::idempotency_key,
    requests::{valid_idempotency_key, validate_client_ref},
    RequestErrorResponse,
};
use super::views::{WebhookDelivery, WebhookDeliveryAttempt, WebhookSubscription};
use super::WebhookConfig;
use crate::{
    error::{ApiError, ApiResult, GatewayError},
    middleware::AuthContext,
    state::AppState,
};

const PAGE_SIZE: usize = 50;

pub fn router(config: Arc<WebhookConfig>) -> Router<AppState> {
    Router::new()
        .route(
            "/v1/projects/:project/webhook-destinations",
            get(destination_ids),
        )
        .route(
            "/v1/projects/:project/webhooks",
            get(list_subscriptions).post(create),
        )
        .route(
            "/v1/projects/:project/webhooks/creations",
            post(create_with_receipt),
        )
        .route(
            "/v1/projects/:project/webhooks/:subscription",
            get(get_subscription).delete(revoke),
        )
        .route(
            "/v1/projects/:project/webhooks/:subscription/rotate",
            post(rotate),
        )
        .route(
            "/v1/projects/:project/webhooks/:subscription/rotations",
            post(rotate_with_receipt),
        )
        .route(
            "/v1/projects/:project/webhook-deliveries",
            get(list_deliveries),
        )
        .route(
            "/v1/projects/:project/webhook-deliveries/:delivery",
            get(detail),
        )
        .route(
            "/v1/projects/:project/webhook-deliveries/:delivery/redrive",
            post(redrive),
        )
        .layer(Extension(config))
}

struct Project {
    account: AccountId,
    id: GatewayProjectId,
}

impl Project {
    async fn authorized(
        state: &AppState,
        auth: &AuthContext,
        query: &OrganizationQuery,
        raw: &str,
    ) -> ApiResult<Self> {
        let (account, project) = managed_project(state, auth, query, raw).await?;
        Ok(Self {
            account,
            id: project.id,
        })
    }

    fn scope(&self) -> RequestScope<'_> {
        RequestScope {
            account: &self.account,
            project: &self.id,
            client_ref: None,
        }
    }
}

#[utoipa::path(
    get, path = "/v1/projects/{project}/webhook-destinations",
    operation_id = "listProjectWebhookDestinations", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    responses(
        (status = 200, description = "Operator-configured destination IDs.", body = [String]),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn destination_ids(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path(raw): Path<String>,
    Extension(config): Extension<Arc<WebhookConfig>>,
) -> ApiResult<Json<Vec<String>>> {
    Project::authorized(&state, &auth, &query, &raw).await?;
    Ok(Json(config.destination_ids().map(str::to_owned).collect()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::gateway_demo) struct PageQuery {
    account_id: String,
    before: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub(in crate::gateway_demo) struct Page<T> {
    items: Vec<T>,
    /// Cursor for the next page. Null when there are no more rows.
    #[schema(required = true)]
    next_before: Option<String>,
}

impl<T> Page<T> {
    fn from_rows(mut items: Vec<T>, id: impl Fn(&T) -> &str) -> Self {
        let next_before = (items.len() > PAGE_SIZE).then(|| id(&items[PAGE_SIZE - 1]).to_owned());
        items.truncate(PAGE_SIZE);
        Self { items, next_before }
    }
}

#[utoipa::path(
    get, path = "/v1/projects/{project}/webhooks",
    operation_id = "listProjectWebhookSubscriptions", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
        ("before" = Option<String>, Query, description = "Use the previous page's next_before value."),
    ),
    responses(
        (status = 200, description = "Up to 50 subscriptions in descending ID order.", body = Page<WebhookSubscription>),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn list_subscriptions(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<PageQuery>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Page<WebhookSubscription>>> {
    let project = Project::authorized(
        &state,
        &auth,
        &OrganizationQuery::new(query.account_id),
        &raw,
    )
    .await?;
    let before = query
        .before
        .as_deref()
        .map(|value| parse_id(value, "before"))
        .transpose()?;
    let subscriptions = gateway_webhooks::subscriptions(
        &state.pool,
        project.scope(),
        before.as_ref(),
        PAGE_SIZE as i64 + 1,
    )
    .await?;
    let items = subscriptions.into_iter().map(WebhookSubscription::from).collect();
    Ok(Json(Page::from_rows(items, |row| &row.id)))
}

#[utoipa::path(
    get, path = "/v1/projects/{project}/webhooks/{subscription}",
    operation_id = "getProjectWebhookSubscription", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("subscription" = String, Path, description = "Subscription ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    responses(
        (status = 200, description = "Subscription metadata, including its client scope and revocation status.", body = WebhookSubscription),
        (status = 404, description = "Subscription unavailable in this project.", body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn get_subscription(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path((raw, subscription)): Path<(String, String)>,
) -> ApiResult<Json<WebhookSubscription>> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    let subscription = parse_id(&subscription, "subscription")?;
    let record = gateway_webhooks::find_subscription(&state.pool, project.scope(), &subscription)
        .await?
        .ok_or_else(|| ApiError::NotFound("webhook subscription not found".into()))?;
    Ok(Json(record.into()))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::gateway_demo) struct CreateWebhookSubscription {
    /// Display name with 1 to 80 characters after trimming.
    name: String,
    /// An ID from the operator's destination registry.
    destination_id: String,
    /// Limit deliveries to requests with this client reference.
    client_ref: Option<String>,
    /// Deliver terminal runs for one workflow application in this project.
    workflow_app_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub(in crate::gateway_demo) struct IssuedWebhookSubscription {
    #[serde(flatten)]
    subscription: WebhookSubscription,
    /// Store this signing secret securely. It is returned when issued.
    signing_secret: String,
}

#[utoipa::path(
    post, path = "/v1/projects/{project}/webhooks",
    operation_id = "createProjectWebhookSubscription", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    request_body = CreateWebhookSubscription,
    responses(
        (status = 201, description = "Subscription and newly issued signing secret. The response is never cached.",
         body = IssuedWebhookSubscription),
        (status = 400, description = "Invalid input or unknown destination.",
         content((GatewayError = "application/json"), (String = "text/plain"))),
        (status = 404, description = "Workflow app unavailable in this project.", body = GatewayError),
        (status = 409, description = "The workflow app already has an active subscription.", body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn create(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path(raw): Path<String>,
    Extension(config): Extension<Arc<WebhookConfig>>,
    Json(input): Json<CreateWebhookSubscription>,
) -> ApiResult<(StatusCode, Json<IssuedWebhookSubscription>)> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    issue_creation(&state, &auth, &project, &config, input, None).await
}

#[utoipa::path(
    post, path = "/v1/projects/{project}/webhooks/creations",
    operation_id = "createProjectWebhookWithReceipt", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
        ("Idempotency-Key" = String, Header, description = "A fresh random key of 32 to 128 allowed characters. Repeat with the original input to recover the response."),
    ),
    request_body = CreateWebhookSubscription,
    responses(
        (status = 201, description = "The new subscription and signing key, or the same result for a matching administrator replay within 24 hours. Never cached.",
         body = IssuedWebhookSubscription),
        (status = 400, description = "Invalid input, receipt key, or unknown destination.", body = GatewayError),
        (status = 404, description = "Workflow app unavailable in this project.", body = GatewayError),
        (status = 409, description = "A conflicting active workflow subscription or an unavailable creation receipt.", body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn create_with_receipt(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path(raw): Path<String>,
    Extension(config): Extension<Arc<WebhookConfig>>,
    headers: HeaderMap,
    Json(input): Json<CreateWebhookSubscription>,
) -> ApiResult<(StatusCode, Json<IssuedWebhookSubscription>)> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    let key = receipt_key(&headers)?;
    let intent = serde_json::to_vec(&(
        "tokencraft-webhook-creation-v1", auth.user.id.as_str(),
        project.account.as_str(), project.id.as_str(), key,
    )).expect("creation receipt key serializes");
    let digest: [u8; 32] = Sha256::digest(intent).into();
    issue_creation(&state, &auth, &project, &config, input, Some(&digest)).await
}

async fn issue_creation(
    state: &AppState,
    auth: &AuthContext,
    project: &Project,
    config: &WebhookConfig,
    input: CreateWebhookSubscription,
    receipt: Option<&[u8]>,
) -> ApiResult<(StatusCode, Json<IssuedWebhookSubscription>)> {
    let name = validate_name(&input.name)?;
    let client_ref = input.client_ref.as_deref();
    validate_client_ref(client_ref)?;
    let workflow_app = input.workflow_app_id.as_deref()
        .map(|value| parse_id::<WorkflowAppId>(value, "workflow app ID"))
        .transpose()?;
    if workflow_app.is_some() && client_ref.is_some() {
        return Err(ApiError::BadRequest("choose either a workflow app or a request client reference".into()));
    }
    let destination = config
        .destination(&input.destination_id)
        .ok_or_else(|| ApiError::BadRequest("unknown webhook destination".into()))?;
    let id = GatewayWebhookSubscriptionId::generate();
    let mut tx = state.pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    if let Some(app) = &workflow_app {
        if !gateway_webhooks::workflow_app_exists(&mut *tx, project.scope(), app).await? {
            return Err(ApiError::NotFound("workflow app not found in this project".into()));
        }
    }
    let result = gateway_webhooks::create_subscription(
        &mut tx,
        project.scope(),
        NewSubscription {
            id: &id,
            name,
            destination_id: &input.destination_id,
            destination_url: destination.as_str(),
            client_ref,
            workflow_app_id: workflow_app.as_ref(),
        },
        receipt,
    ).await?.ok_or_else(|| ApiError::Conflict(if receipt.is_some() {
        "the creation receipt is unavailable or this workflow app already has an active webhook subscription".into()
    } else {
        "this workflow app already has an active webhook subscription".into()
    }))?;
    if result.created {
        audit(
            &mut tx,
            &project.account,
            &auth.user.id,
            AuditEvent::GatewayWebhookChanged,
            "gateway_webhook_subscription",
            id.as_str(),
        )
        .await?;
    }
    tx.commit().await?;
    let subscription = result.subscription;
    let signing_secret = config.signing_secret(&subscription.id, subscription.signing_version);
    Ok((
        StatusCode::CREATED,
        Json(IssuedWebhookSubscription {
            subscription: subscription.into(),
            signing_secret,
        }),
    ))
}

#[utoipa::path(
    post, path = "/v1/projects/{project}/webhooks/{subscription}/rotate",
    operation_id = "rotateProjectWebhookSecret", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("subscription" = String, Path, description = "Webhook subscription ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    responses(
        (status = 200, description = "New signing secret. The previous version overlaps for 24 hours.",
         body = IssuedWebhookSubscription),
        (status = 409, description = "The active subscription is unavailable or has rotated within 24 hours.",
         body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn rotate(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path((raw, subscription)): Path<(String, String)>,
    Extension(config): Extension<Arc<WebhookConfig>>,
) -> ApiResult<Json<IssuedWebhookSubscription>> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    let id = parse_id(&subscription, "subscription_id")?;
    issue_rotation(&state, &auth, &project, &id, &config, None).await
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::gateway_demo) struct WebhookRotationInput {
    /// The signing version the caller intends to replace.
    #[schema(minimum = 1, maximum = 2147483646)]
    expected_signing_version: i32,
}

#[utoipa::path(
    post, path = "/v1/projects/{project}/webhooks/{subscription}/rotations",
    operation_id = "rotateProjectWebhookWithReceipt", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("subscription" = String, Path, description = "Webhook subscription ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
        ("Idempotency-Key" = String, Header, description = "A fresh random key for this rotation. Repeat it with the original signing version to recover the response."),
    ),
    request_body = WebhookRotationInput,
    responses(
        (status = 200, description = "The new signing key, or the same key for a matching administrator replay during the 24-hour overlap.", body = IssuedWebhookSubscription),
        (status = 409, description = "The signing version changed, the rotation is in its overlap window, or the matching receipt is unavailable.", body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn rotate_with_receipt(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path((raw, subscription)): Path<(String, String)>,
    Extension(config): Extension<Arc<WebhookConfig>>,
    headers: HeaderMap,
    Json(input): Json<WebhookRotationInput>,
) -> ApiResult<Json<IssuedWebhookSubscription>> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    let id: GatewayWebhookSubscriptionId = parse_id(&subscription, "subscription_id")?;
    let expected_version = input.expected_signing_version;
    if !(1..i32::MAX).contains(&expected_version) {
        return Err(ApiError::BadRequest("expected_signing_version must be a positive version below the maximum".into()));
    }
    let key = receipt_key(&headers)?;
    let digest = config.rotation_receipt_digest(auth.user.id.as_str(), id.as_str(), expected_version, key);
    let receipt = RotationReceipt { expected_version, digest: &digest };
    issue_rotation(&state, &auth, &project, &id, &config, Some(receipt)).await
}

fn receipt_key(headers: &HeaderMap) -> ApiResult<&str> {
    let key = idempotency_key(headers)?;
    if key.len() < 32 || !valid_idempotency_key(key) {
        return Err(ApiError::BadRequest("use 32 to 128 ASCII letters, numbers or . _ : - in Idempotency-Key".into()));
    }
    Ok(key)
}

async fn issue_rotation(
    state: &AppState,
    auth: &AuthContext,
    project: &Project,
    id: &GatewayWebhookSubscriptionId,
    config: &WebhookConfig,
    receipt: Option<RotationReceipt<'_>>,
) -> ApiResult<Json<IssuedWebhookSubscription>> {
    let mut tx = state.pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    let result = gateway_webhooks::rotate_subscription(&mut tx, project.scope(), id, receipt)
        .await?
        .ok_or_else(|| ApiError::Conflict(
            "subscription is missing, revoked, or the signing version or rotation window has changed".into(),
        ))?;
    if result.changed {
        audit(
            &mut tx,
            &project.account,
            &auth.user.id,
            AuditEvent::GatewayWebhookChanged,
            "gateway_webhook_subscription",
            id.as_str(),
        )
        .await?;
    }
    tx.commit().await?;
    let signing_secret = config.signing_secret(id.as_str(), result.subscription.signing_version);
    Ok(Json(IssuedWebhookSubscription {
        subscription: result.subscription.into(),
        signing_secret,
    }))
}

#[utoipa::path(
    delete, path = "/v1/projects/{project}/webhooks/{subscription}",
    operation_id = "revokeProjectWebhookSubscription", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("subscription" = String, Path, description = "Webhook subscription ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    responses(
        (status = 204, description = "Revoked. A delivery already in progress may finish."),
        (status = 404, description = "Active subscription unavailable.", body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn revoke(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path((raw, subscription)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    let id: GatewayWebhookSubscriptionId = parse_id(&subscription, "subscription_id")?;
    let mut tx = state.pool.begin().await?;
    if !gateway_webhooks::revoke_subscription(&mut *tx, project.scope(), &id).await? {
        return Err(ApiError::NotFound("active subscription not found".into()));
    }
    audit(
        &mut tx,
        &project.account,
        &auth.user.id,
        AuditEvent::GatewayWebhookChanged,
        "gateway_webhook_subscription",
        id.as_str(),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get, path = "/v1/projects/{project}/webhook-deliveries",
    operation_id = "listProjectWebhookDeliveries", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
        ("before" = Option<String>, Query, description = "Use the previous page's next_before value."),
    ),
    responses(
        (status = 200, description = "Up to 50 deliveries in descending ID order.", body = Page<WebhookDelivery>),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn list_deliveries(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<PageQuery>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Page<WebhookDelivery>>> {
    let project = Project::authorized(
        &state,
        &auth,
        &OrganizationQuery::new(query.account_id),
        &raw,
    )
    .await?;
    let before = query
        .before
        .as_deref()
        .map(|value| parse_id(value, "before"))
        .transpose()?;
    let deliveries = gateway_webhooks::deliveries(
        &state.pool,
        project.scope(),
        None,
        before.as_ref(),
        PAGE_SIZE as i64 + 1,
    )
    .await?;
    let items = deliveries.into_iter().map(WebhookDelivery::from).collect();
    Ok(Json(Page::from_rows(items, |row| &row.id)))
}

#[derive(Serialize, ToSchema)]
pub(in crate::gateway_demo) struct WebhookDeliveryDetail {
    delivery: WebhookDelivery,
    /// Up to 50 attempts, newest first.
    attempts: Vec<WebhookDeliveryAttempt>,
}

#[utoipa::path(
    get, path = "/v1/projects/{project}/webhook-deliveries/{delivery}",
    operation_id = "getProjectWebhookDelivery", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("delivery" = String, Path, description = "Webhook delivery ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    responses(
        (status = 200, description = "Delivery status and recent attempts.", body = WebhookDeliveryDetail),
        (status = 404, description = "Delivery unavailable in this project.", body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn detail(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path((raw, delivery)): Path<(String, String)>,
) -> ApiResult<Json<WebhookDeliveryDetail>> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    let id: GatewayWebhookDeliveryId = parse_id(&delivery, "delivery_id")?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let mut deliveries =
        gateway_webhooks::deliveries(&mut *tx, project.scope(), Some(&id), None, 1).await?;
    let delivery = deliveries
        .pop()
        .ok_or_else(|| ApiError::NotFound("delivery not found".into()))?;
    let attempts = gateway_webhooks::attempts(&mut *tx, project.scope(), &id).await?;
    tx.commit().await?;
    Ok(Json(WebhookDeliveryDetail {
        delivery: delivery.into(),
        attempts: attempts.into_iter().map(Into::into).collect(),
    }))
}

#[utoipa::path(
    post, path = "/v1/projects/{project}/webhook-deliveries/{delivery}/redrive",
    operation_id = "redriveProjectWebhookDelivery", tag = "webhooks",
    security(("session_cookie" = [])),
    params(
        ("project" = String, Path, description = "Project ID."),
        ("delivery" = String, Path, description = "Webhook delivery ID."),
        ("account_id" = String, Query, description = "Organization account ID."),
    ),
    responses(
        (status = 204, description = "The original event was queued for another delivery."),
        (status = 409, description = "Redrive requires a failed delivery with an active subscription.",
         body = GatewayError),
        (status = "default", response = RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn redrive(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<OrganizationQuery>,
    Path((raw, delivery)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let project = Project::authorized(&state, &auth, &query, &raw).await?;
    let id: GatewayWebhookDeliveryId = parse_id(&delivery, "delivery_id")?;
    let mut tx = state.pool.begin().await?;
    if !gateway_webhooks::redrive(&mut *tx, project.scope(), &id).await? {
        return Err(ApiError::Conflict(
            "redrive requires a failed delivery with an active subscription".into(),
        ));
    }
    audit(
        &mut tx,
        &project.account,
        &auth.user.id,
        AuditEvent::GatewayWebhookChanged,
        "gateway_webhook_delivery",
        id.as_str(),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
