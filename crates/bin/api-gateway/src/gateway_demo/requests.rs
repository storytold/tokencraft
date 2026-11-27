use axum::http::{HeaderMap, HeaderValue};
use chrono::{DateTime, Utc};
use db_queries::gateway_requests::{
    self, AdmissionKey, NewGatewayRequest, NewProviderRun, RequestCorrelation, RequestRow, RequestScope,
};
use db_queries::{gateway_routing_policies, gateway_webhooks};
pub(super) use db_queries::gateway_requests::{CostState, RequestStatus};
use ids::{GatewayProjectId, GatewayRequestId, GatewayWebhookSubscriptionId};
use serde::{Deserialize, Serialize};

use super::access::{parse_id, Access};
use super::artifacts::MediaResult;
use super::availability::{AvailabilityInput, RouteAvailability};
use super::catalog::{digest, valid_identifier, Adapter, Catalog};
use super::chat_input::{validate_messages, ChatInput, ChatOptions, ChatResult, Message};
use super::providers::{
    connector::{MediaInput as ConnectorInput, StoredInput, WorkflowFrame},
    video,
    ProviderRuntime,
};
use super::quote::{Quote, QuoteInput, QuoteProfile, QuoteTarget, Reason, Workload};
use super::routing_policy::Policies;
use super::webhooks::WebhookConfig;
use crate::error::{ApiError, ApiResult};
use crate::middleware::trace_context::{TraceContext, TRACE_ID_HEADER};

pub(crate) const APP_REF: &str = "x-tokencraft-app-ref";

pub(super) fn request_status_schema() -> utoipa::openapi::Object {
    use RequestStatus::*;
    enum_schema([Queued, Running, Succeeded, Canceled, Failed, AwaitingReconciliation])
}

pub(super) fn request_cost_state_schema() -> utoipa::openapi::Object {
    use CostState::*;
    enum_schema([Estimated, MockReported, ProviderReported, SupplierSettled])
}

fn enum_schema<T: Serialize, const N: usize>(values: [T; N]) -> utoipa::openapi::Object {
    use utoipa::openapi::schema::{ObjectBuilder, Type};
    let values = values.map(|value| serde_json::to_value(value).expect("request enum serializes"));
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(values))
        .build()
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = QuotedSubmission)]
pub struct Submission {
    pub account_id: Option<String>,
    pub project_id: Option<String>,
    #[schema(value_type = String, required = true)]
    pub quote_id: Option<String>,
    /// Exact offer pin returned by the scoped quote.
    pub pinned_offer_id: Option<String>,
    pub input: QuoteInput,
    pub client_ref: Option<String>,
    /// Opaque application reference, retained in history and usage.
    pub app_ref: Option<String>,
    pub delivery: Option<DeliveryInput>,
    pub messages: Option<Vec<Message>>,
    pub image_prompt: Option<String>,
    pub video_prompt: Option<String>,
    pub audio_prompt: Option<String>,
    pub mesh_prompt: Option<String>,
    #[serde(default)]
    pub chat_options: ChatOptions,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeliveryInput {
    webhook_subscription_id: String,
}

pub(super) enum QuoteSelection {
    Declared(String),
    DeclaredMedia(String),
    Automatic,
    Advised(super::adaptive::AdvisorSelection),
    WorkflowPinned,
    WorkflowFrame,
    Chat,
}

pub(super) struct AdmissionInput {
    pub selection: QuoteSelection,
    pub pinned_offer_id: Option<String>,
    pub input: QuoteInput,
    pub client_ref: Option<String>,
    pub app_ref: Option<String>,
    pub webhook_subscription_id: Option<String>,
    pub messages: Option<Vec<Message>>,
    pub image_prompt: Option<String>,
    pub video_prompt: Option<String>,
    pub audio_prompt: Option<String>,
    pub mesh_prompt: Option<String>,
    pub chat_options: ChatOptions,
    pub stream: bool,
}

impl AdmissionInput {
    pub(super) fn validate_prompts(&self) -> ApiResult<()> {
        if let Some(parameter) = self.chat_options.invalid_parameter() {
            return Err(ApiError::BadRequest(format!("invalid {parameter}")));
        }
        if !self.chat_options.is_empty() && self.messages.is_none() {
            return Err(ApiError::BadRequest("chat options require messages".into()));
        }
        if self.image_prompt.as_ref().is_some_and(|prompt| {
            self.messages.is_some() || prompt.trim().is_empty() || prompt.len() > 4000
        }) {
            return Err(ApiError::BadRequest(
                "supply one non-empty image prompt of up to 4000 bytes, without chat messages"
                    .into(),
            ));
        }
        if self.video_prompt.as_ref().is_some_and(|prompt| {
            self.messages.is_some()
                || self.image_prompt.is_some()
                || prompt.trim().is_empty()
                || prompt.len() > video::MAX_PROMPT_BYTES
                || !matches!(&self.input.workload, Workload::Video { .. })
        }) {
            return Err(ApiError::BadRequest(
                "supply one non-empty video prompt of up to 2048 bytes for a video workload".into(),
            ));
        }
        if self.audio_prompt.as_ref().is_some_and(|prompt| {
            self.messages.is_some()
                || self.image_prompt.is_some()
                || self.video_prompt.is_some()
                || prompt.trim().is_empty()
                || prompt.len() > 2048
                || !matches!(&self.input.workload, Workload::Audio { .. })
        }) {
            return Err(ApiError::BadRequest(
                "supply one non-empty audio prompt of up to 2048 bytes for an audio workload"
                    .into(),
            ));
        }
        if self.mesh_prompt.as_ref().is_some_and(|prompt| {
            self.messages.is_some()
                || self.image_prompt.is_some()
                || self.video_prompt.is_some()
                || self.audio_prompt.is_some()
                || prompt.trim().is_empty()
                || prompt.chars().count() > 600
                || !matches!(&self.input.workload, Workload::Mesh { .. })
        }) {
            return Err(ApiError::BadRequest(
                "supply one non-empty mesh prompt of up to 600 characters for a mesh workload"
                    .into(),
            ));
        }
        Ok(())
    }
}

impl From<Submission> for AdmissionInput {
    fn from(value: Submission) -> Self {
        Self {
            selection: value
                .quote_id
                .map_or(QuoteSelection::Automatic, QuoteSelection::Declared),
            input: value.input,
            pinned_offer_id: value.pinned_offer_id,
            client_ref: value.client_ref,
            app_ref: value.app_ref,
            webhook_subscription_id: value
                .delivery
                .map(|delivery| delivery.webhook_subscription_id),
            messages: value.messages,
            image_prompt: value.image_prompt,
            video_prompt: value.video_prompt,
            audio_prompt: value.audio_prompt,
            mesh_prompt: value.mesh_prompt,
            chat_options: value.chat_options,
            stream: false,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AcceptedRequest {
    #[schema(value_type = String)]
    pub id: GatewayRequestId,
    #[schema(value_type = String)]
    pub project_id: GatewayProjectId,
    pub status_url: String,
    #[serde(skip)]
    trace_id: Option<String>,
}

impl AcceptedRequest {
    pub(super) fn append_trace_header(&self, headers: &mut HeaderMap) {
        if let Some(value) = self.trace_id.as_deref().and_then(|id| HeaderValue::from_str(id).ok()) {
            headers.insert(TRACE_ID_HEADER, value);
        }
    }
}

pub(super) fn media_quote(
    catalog: &Catalog,
    input: QuoteInput,
    scope: RequestScope<'_>,
    policies: &Policies,
    providers: Option<&ProviderRuntime>,
    availability: &RouteAvailability,
) -> ApiResult<Quote> {
    catalog.scoped_media_quote(input, scope, policies, availability, |offer| {
        providers.is_some_and(|runtime| runtime.queue_offer_binding(offer).is_some())
    })
}

pub(super) async fn submit(
    pool: &db::PgPool,
    catalog: &Catalog,
    webhooks: Option<&WebhookConfig>,
    providers: Option<&ProviderRuntime>,
    seals: Option<&super::quote_seals::QuoteSeals>,
    access: &Access,
    idempotency_key: &str,
    request: AdmissionInput,
) -> ApiResult<(AcceptedRequest, bool)> {
    submit_inner(pool, catalog, webhooks, providers, seals, access, idempotency_key, request, None).await
}

pub(super) struct WorkflowAdmission<'a> {
    pub pool: &'a db::PgPool,
    pub catalog: &'a Catalog,
    pub providers: Option<&'a ProviderRuntime>,
    pub access: &'a Access,
    pub idempotency_key: &'a str,
    pub run: &'a ids::WorkflowRunId,
    pub lease: i64,
    pub first_frame: Option<Vec<u8>>,
}

pub(super) async fn submit_workflow(
    context: WorkflowAdmission<'_>,
    request: AdmissionInput,
) -> ApiResult<(AcceptedRequest, bool)> {
    let WorkflowAdmission { pool, catalog, providers, access, idempotency_key, run, lease, first_frame } = context;
    submit_inner(pool, catalog, None, providers, None, access, idempotency_key, request,
        Some((run, lease, first_frame))).await
}

async fn submit_inner(
    pool: &db::PgPool,
    catalog: &Catalog,
    webhooks: Option<&WebhookConfig>,
    providers: Option<&ProviderRuntime>,
    seals: Option<&super::quote_seals::QuoteSeals>,
    access: &Access,
    idempotency_key: &str,
    mut request: AdmissionInput,
    workflow: Option<(&ids::WorkflowRunId, i64, Option<Vec<u8>>)>,
) -> ApiResult<(AcceptedRequest, bool)> {
    if !valid_idempotency_key(idempotency_key) {
        return Err(ApiError::BadRequest(
            "use 8 to 128 ASCII letters, numbers or . _ : - in Idempotency-Key".into(),
        ));
    }
    let client = access.for_client(request.client_ref.as_deref())?.client_ref;
    let app = request.app_ref.as_deref();
    validate_app_ref(app)?;
    if let QuoteSelection::Declared(quote_id) | QuoteSelection::DeclaredMedia(quote_id) =
        &request.selection
    {
        if !super::quote_seals::valid_id(quote_id) {
            return Err(ApiError::BadRequest("invalid quote_id".into()));
        }
    }
    let pinned_offer = request.pinned_offer_id.as_deref();
    let workflow_pinned = matches!(request.selection, QuoteSelection::WorkflowPinned);
    let first_frame = workflow.as_ref().and_then(|(_, _, frame)| frame.as_ref());
    if workflow_pinned && (workflow.is_none() || pinned_offer.is_none()) {
        return Err(ApiError::BadRequest("workflow execution requires a selected offer".into()));
    }
    if matches!(request.selection, QuoteSelection::WorkflowFrame) && first_frame.is_none() {
        return Err(ApiError::BadRequest("workflow frame routing requires a private first frame".into()));
    }
    if pinned_offer.is_some_and(|id| !super::catalog::valid_identifier(id))
        || (pinned_offer.is_some() && !workflow_pinned && !matches!(request.selection, QuoteSelection::Declared(_)))
    {
        return Err(ApiError::BadRequest("supply a valid pinned_offer_id with a native quote".into()));
    }
    request.validate_prompts()?;
    request.input.normalize()?;
    let subscription: Option<GatewayWebhookSubscriptionId> = request
        .webhook_subscription_id
        .as_deref()
        .map(|id| parse_id(id, "webhook_subscription_id"))
        .transpose()?;
    let mut fingerprint = request
        .messages
        .as_ref()
        .map(|messages| {
            let runtime = providers
                .ok_or_else(|| ApiError::BadRequest("chat execution is not enabled".into()))?;
            let bytes = if request.stream {
                serde_json::to_vec(&("openrouter_stream_v1", messages, request.chat_options))
            } else if request.chat_options.is_empty() {
                serde_json::to_vec(messages)
            } else {
                serde_json::to_vec(&(messages, request.chat_options))
            }
            .map_err(|_| ApiError::Internal("could not encode chat intent".into()))?;
            Ok::<_, ApiError>(runtime.payloads.fingerprint(&bytes))
        })
        .transpose()?;
    for (kind, domain, prompt) in [
        ("image", "fal_image_v1", &request.image_prompt),
        ("video", "fal_video_v1", &request.video_prompt),
        ("audio", "fal_audio_v1", &request.audio_prompt),
        ("mesh", "fal_mesh_v1", &request.mesh_prompt),
    ] {
        if let Some(prompt) = prompt {
            let runtime = providers
                .ok_or_else(|| ApiError::BadRequest(format!("{kind} execution is not enabled")))?;
            let encoded = serde_json::to_vec(&(domain, prompt))
                .map_err(|_| ApiError::Internal(format!("could not encode {kind} intent")))?;
            fingerprint = Some(runtime.payloads.fingerprint(&encoded));
        }
    }
    if let Some(frame) = first_frame {
        let runtime = providers.ok_or_else(|| ApiError::BadRequest("workflow frames require a connector".into()))?;
        let identity = serde_json::to_vec(&(fingerprint.as_deref(), WorkflowFrame::png(frame)))
            .map_err(|_| ApiError::Internal("could not encode workflow frame intent".into()))?;
        fingerprint = Some(runtime.payloads.fingerprint(&identity));
    }
    let intent = digest(
        match &request.selection {
            QuoteSelection::WorkflowPinned | QuoteSelection::WorkflowFrame => {
                // First-frame admissions used the pinned marker before selecting eligible routes at admission.
                serde_json::to_vec(&("workflow_pinned", &request.input, &fingerprint))
            }
            QuoteSelection::Declared(quote_id) | QuoteSelection::DeclaredMedia(quote_id) => {
                match (&subscription, &fingerprint) {
                    (_, Some(fingerprint)) => serde_json::to_vec(&(
                        quote_id,
                        &request.input,
                        &client,
                        &subscription,
                        fingerprint,
                    )),
                    (Some(id), None) => {
                        serde_json::to_vec(&(quote_id, &request.input, &client, id))
                    }
                    (None, None) => serde_json::to_vec(&(quote_id, &request.input, &client)),
                }
            }
            QuoteSelection::Chat => serde_json::to_vec(&(
                "chat_completions_v1",
                &request.input,
                &client,
                &subscription,
                &fingerprint,
            )),
            QuoteSelection::Advised(selection) => serde_json::to_vec(&(
                "advisor_media_v1",
                selection.source,
                &request.input,
                &client,
                &subscription,
                &fingerprint,
            )),
            QuoteSelection::Automatic => serde_json::to_vec(&(
                "automatic_media_v1",
                &request.input,
                &client,
                &subscription,
                &fingerprint,
            )),
        }
        .map_err(|_| ApiError::Internal("could not encode request intent".into()))?,
    );
    // Requests submitted before app attribution keep their original intent digest.
    let intent = match app {
        Some(reference) => digest(format!("{intent}:{reference}")),
        None => intent,
    };
    let intent = match pinned_offer {
        Some(offer) => digest(format!("{intent}:offer:{offer}")),
        None => intent,
    };
    let idempotency_digest = digest(idempotency_key);
    let key = AdmissionKey {
        scope: access.scope(),
        actor_id: &access.actor_id,
        idempotency_digest: &idempotency_digest,
    };
    let advised = if let QuoteSelection::Advised(selection) = &request.selection {
        // Completed retries skip the Advisor. Another check under the admission lock closes the race.
        if let Some(accepted) = replay(pool, key, &intent).await? {
            return Ok((accepted, true));
        }
        let scope = RequestScope { client_ref: client, ..key.scope };
        Some(super::adaptive::select(super::adaptive::AdvisorRoute {
            pool, scope, catalog, providers, selection, input: &request.input,
        }).await?.ok_or_else(|| {
            ApiError::BadRequest("no eligible model matches the advised request".into())
        })?)
    } else {
        None
    };
    // Admission limits use fresh reads after acquiring their locks.
    let mut tx = pool
        .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
        .await?;
    gateway_routing_policies::lock_for_admission(&mut *tx, key.scope.account).await?;
    let workflow_lease = workflow.as_ref().map(|(run, lease, _)| (*run, *lease));
    let lease_ended = || ApiError::Conflict("workflow execution is no longer active".into());
    let (workflow_budget, workflow_app) = if let Some((run, lease)) = workflow_lease {
        let locked = db_queries::workflows::lock_active_run(&mut *tx, run, lease).await?
            .ok_or_else(lease_ended)?;
        (locked.budget_month_start.map(|month| (run, month)), locked.application_id)
    } else {
        (None, None)
    };
    if let Some(accepted) = replay(&mut *tx, key, &intent).await? {
        return Ok((accepted, true));
    }

    let policies = Policies::load(&mut *tx, key.scope).await?;
    if let QuoteSelection::Advised(selection) = &request.selection {
        selection.validate_policy(&policies)?;
    }
    let profile = match &request.selection {
        QuoteSelection::Chat => QuoteProfile::Chat,
        QuoteSelection::Automatic | QuoteSelection::DeclaredMedia(_) | QuoteSelection::Advised(_) => QuoteProfile::QueueMedia,
        QuoteSelection::Declared(_) | QuoteSelection::WorkflowPinned | QuoteSelection::WorkflowFrame => QuoteProfile::Native,
    };
    let chat = profile == QuoteProfile::Chat;
    let availability = RouteAvailability::load(&mut *tx, AvailabilityInput {
        scope: key.scope, catalog, providers, quote: &request.input, policies: &policies,
    }).await?;
    let quote = if matches!(request.selection, QuoteSelection::WorkflowFrame) {
        catalog.scoped_media_quote(request.input, key.scope, &policies, &availability, |offer| {
            providers.is_some_and(|runtime| runtime.accepts_workflow_frame(offer))
        })?
    } else if profile == QuoteProfile::QueueMedia {
        let has_prompt = match request.input.workload {
            Workload::Image { .. } => request.image_prompt.is_some(),
            Workload::Video { .. } => request.video_prompt.is_some(),
            Workload::Audio { .. } => request.audio_prompt.is_some(),
            Workload::Mesh { .. } => request.mesh_prompt.is_some(),
            Workload::Text { .. } => false,
        };
        if !has_prompt {
            return Err(ApiError::BadRequest(
                "supply a prompt matching the media workload".into(),
            ));
        }
        media_quote(catalog, request.input, key.scope, &policies, providers, &availability)?
    } else {
        let target = match pinned_offer {
            Some(offer) => QuoteTarget::Offer(offer),
            None if chat => QuoteTarget::Chat,
            None => QuoteTarget::Any,
        };
        catalog.scoped_quote(request.input, key.scope, &policies, &availability, target)?
    };
    let evidence_scope = RequestScope { client_ref: client, ..key.scope };
    let mut quote =
        super::route_performance::rank(&mut *tx, evidence_scope, catalog, providers, quote).await?;
    let mut legacy_quote = false;
    if let QuoteSelection::Declared(quote_id) | QuoteSelection::DeclaredMedia(quote_id) =
        &request.selection
    {
        if super::quote_seals::is_sealed(quote_id) {
            let seals = seals.ok_or_else(|| ApiError::Internal("quote signer unavailable".into()))?;
            let expires = seals.verify(quote_id, &quote, evidence_scope, Utc::now())?;
            quote.id.clone_from(quote_id);
            quote.expires_at = Some(expires);
        } else {
            if &quote.id != quote_id {
                return Err(ApiError::QuoteStale);
            }
            legacy_quote = true;
        }
    }
    if let Some(decision) = advised {
        super::adaptive::apply(&mut quote, decision)?;
    }
    quote.chat_options = request.chat_options;
    quote.profile = Some(profile);
    if quote.input.provider.as_ref().is_some_and(|provider| !policies.allows_provider(provider)) {
        return Err(ApiError::Forbidden("the requested provider is blocked by routing policy".into()));
    }
    let selected = quote
        .candidates
        .first()
        .filter(|c| Some(&c.offer_id) == quote.selected_offer_id.as_ref())
        .ok_or_else(|| {
            if quote.input.provider.as_ref().is_some_and(|provider| quote.candidates.iter().any(|candidate| {
                &candidate.provider == provider && candidate.reason == Reason::ProviderPolicyExcluded
            })) {
                return ApiError::Forbidden("the requested provider is blocked by routing policy".into());
            }
            if quote.candidates.iter().any(|candidate| candidate.reason == Reason::ProviderCredentialUnavailable) {
                return ApiError::ProviderCredentialUnavailable;
            }
            if quote.candidates.iter().any(|candidate| candidate.reason == Reason::QueueCoolingDown) {
                return ApiError::ServiceUnavailable("eligible provider queues are cooling down; request a fresh quote".into());
            }
            if quote.input.is_auto() && quote.candidates.iter().any(|candidate| matches!(
                candidate.reason, Reason::InsufficientModelRatings | Reason::BelowModelQuality | Reason::BelowModelUsage
            )) {
                return ApiError::BadRequest("no eligible model meets the community thresholds".into());
            }
            if quote.candidates.iter().any(|candidate| matches!(
                candidate.reason, Reason::ProviderRegionUnmet | Reason::ProviderRetentionUnmet
            )) {
                return ApiError::BadRequest("no offer meets the provider data handling requirements".into());
            }
            if chat
                && quote
                    .candidates
                    .iter()
                    .any(|candidate| candidate.reason == Reason::AboveCostCap)
            {
                ApiError::BadRequest("no chat offer meets the estimated cost cap".into())
            } else {
                ApiError::BadRequest("no offer matches this quote".into())
            }
        })?;
    let estimate = selected
        .estimated_usd_nanos
        .ok_or_else(|| ApiError::Internal("selected offer is missing a price".into()))?;
    let offer = catalog
        .offers
        .iter()
        .find(|offer| offer.id == selected.offer_id)
        .ok_or_else(|| ApiError::Internal("selected offer is missing from catalog".into()))?;
    let snapshot = serde_json::to_value(&quote)
        .map_err(|_| ApiError::Internal("could not encode quote snapshot".into()))?;
    let id = GatewayRequestId::generate();
    if legacy_quote && (offer.adapter != Adapter::Mock || quote.routing.is_some()) {
        return Err(ApiError::QuoteStale);
    }
    let admission_deadline = quote
        .expires_at
        .into_iter()
        .chain(selected.rate_evidence.as_ref().map(|rate| rate.valid_until))
        .min();
    if first_frame.is_some() && !providers.is_some_and(|runtime| runtime.accepts_workflow_frame(offer)) {
        return Err(ApiError::Conflict("the selected connector cannot use a workflow frame".into()));
    }
    let staged_frame = workflow.and_then(|(_, _, frame)| frame).map(|png| {
        providers.and_then(|runtime| runtime.payloads.seal(id.as_str(), "workflow-frame", png))
            .ok_or_else(|| ApiError::Internal("could not seal the workflow frame".into()))
    }).transpose()?;
    let mut archived_input = None;
    let provider_payload = match offer.adapter {
        Adapter::Mock
            if request.messages.is_none()
                && request.image_prompt.is_none()
                && request.video_prompt.is_none()
                && request.audio_prompt.is_none()
                && request.mesh_prompt.is_none() =>
        {
            None
        }
        Adapter::Mock => {
            return Err(ApiError::BadRequest(
                "the selected fixture does not accept prompt content".into(),
            ))
        }
        Adapter::OpenAiCompatibleChat => {
            let runtime = providers
                .ok_or_else(|| ApiError::BadRequest("chat execution is not enabled".into()))?;
            let (_, binding) = runtime
                .chat_offer(offer)
                .ok_or_else(|| ApiError::BadRequest("chat execution is not enabled".into()))?;
            let messages = request.messages.ok_or_else(|| {
                ApiError::BadRequest("messages are required for this chat offer".into())
            })?;
            let Workload::Text {
                max_input_tokens,
                max_output_tokens,
            } = quote.input.workload
            else {
                return Err(ApiError::Internal(
                    "chat offer has an incompatible workload".into(),
                ));
            };
            if !validate_messages(&messages, max_input_tokens) {
                return Err(ApiError::BadRequest("supply up to 16 non-empty text messages including a user role, within the quoted input limit".into()));
            }
            let body = serde_json::to_vec(&ChatInput {
                messages,
                max_tokens: max_output_tokens,
                options: request.chat_options,
                stream: request.stream,
            })
            .map_err(|_| ApiError::Internal("could not encode provider payload".into()))?;
            let encrypted = runtime
                .payloads
                .seal(id.as_str(), "input", body)
                .ok_or_else(|| ApiError::Internal("could not seal provider payload".into()))?;
            let model = offer.upstream_model.as_deref().ok_or_else(|| {
                ApiError::Internal("chat offer is missing a model mapping".into())
            })?;
            Some((binding, model, encrypted))
        }
        Adapter::ConnectorQueue => {
            let (runtime, binding) = providers
                .and_then(|runtime| runtime.queue_offer_binding(offer).map(|binding| (runtime, binding)))
                .ok_or_else(|| ApiError::BadRequest("connector execution is not enabled".into()))?;
            let ratio = || selected.applied_aspect_ratio.clone()
                .ok_or_else(|| ApiError::Internal("media offer is missing a quoted aspect ratio".into()));
            let prompt = |prompt: Option<String>| prompt
                .ok_or_else(|| ApiError::BadRequest("a prompt is required for this connector offer".into()));
            let input = match &quote.input.workload {
                Workload::Image { count, .. } => ConnectorInput::Image {
                    prompt: prompt(request.image_prompt)?, count: *count, aspect_ratio: ratio()?,
                },
                Workload::Video { duration_seconds, resolution, .. } => ConnectorInput::Video {
                    prompt: prompt(request.video_prompt)?, duration_seconds: *duration_seconds,
                    aspect_ratio: ratio()?, resolution: resolution.clone(), first_frame: None,
                },
                Workload::Audio { duration_seconds, format } => ConnectorInput::Audio {
                    prompt: prompt(request.audio_prompt)?, duration_seconds: *duration_seconds,
                    format: format.clone(),
                },
                Workload::Mesh { format } => ConnectorInput::Mesh {
                    prompt: prompt(request.mesh_prompt)?, format: format.clone(),
                },
                Workload::Text { .. } => return Err(ApiError::Internal("connector offer has an incompatible workload".into())),
            };
            let invalid = || ApiError::Internal("could not encode connector payload".into());
            let body = if staged_frame.is_some() {
                if runtime.content_archive.as_ref().is_some_and(|store| store.retain_media) {
                    let metadata = serde_json::to_vec(&input).map_err(|_| invalid())?;
                    archived_input = Some(runtime.payloads.seal(id.as_str(), "input", metadata)
                        .ok_or_else(invalid)?);
                }
                serde_json::to_vec(&StoredInput::StagedFrame { frame_input: input }).map_err(|_| invalid())?
            } else {
                serde_json::to_vec(&input).map_err(|_| invalid())?
            };
            let encrypted = runtime.payloads.seal(id.as_str(), "input", body)
                .ok_or_else(|| ApiError::Internal("could not seal connector payload".into()))?;
            let model = offer.upstream_model.as_deref().ok_or_else(|| {
                ApiError::Internal("connector offer is missing a model mapping".into())
            })?;
            Some((binding, model, encrypted))
        }
    };
    let limits = super::client_budgets::admission_limits(&mut tx, evidence_scope, workflow_app.as_deref()).await?;
    let trace = TraceContext::admission();
    let new_request = NewGatewayRequest {
        id: &id,
        key,
        intent_digest: &intent,
        client_ref: client,
        app_ref: app,
        webhook_subscription: subscription.as_ref(),
        model: &offer.model,
        offer_id: &offer.id,
        provider: offer.provider.as_str(),
        quote: &snapshot,
        adapter: offer.adapter.as_str(),
        mock_latency_ms: offer.mock_latency_ms,
        estimated_usd_nanos: estimate,
        budget_reserved_usd_nanos: estimate,
        workflow_budget_month_start: workflow_budget.map(|(_, month)| month),
        workflow_app_id: workflow_app.as_deref(),
        client_budget: limits.evidence,
        credential: availability.credential(&offer.id),
        correlation: RequestCorrelation {
            trace_id: &trace.trace_id,
            admission_span_id: &trace.span_id,
            caller: trace.caller.as_ref().map(|link| (link.trace_id.as_str(), link.span_id.as_str())),
        },
    };
    if let Some(subscription) = &subscription {
        let config =
            webhooks.ok_or_else(|| ApiError::BadRequest("webhook delivery is disabled".into()))?;
        let (destination_id, url) =
            gateway_webhooks::authorize_destination(&mut *tx, access.scope(), subscription, client)
                .await?
                .ok_or_else(|| ApiError::NotFound("webhook subscription not found".into()))?;
        if config.matches_destination(&destination_id, &url).is_none() {
            return Err(ApiError::Conflict(
                "webhook destination is unavailable".into(),
            ));
        }
    }
    let inserted = gateway_requests::insert_request(&mut *tx, new_request).await?;
    if inserted {
        super::request_capacity::enforce(&mut tx, key.scope, &policies, &offer.provider).await?;
        if let Some((run, _)) = workflow_budget.filter(|_| estimate > 0) {
            if !db_queries::workflows::consume_budget(&mut *tx, run, estimate).await? {
                return Err(ApiError::BudgetExceeded);
            }
        }
        if !gateway_requests::monthly_budget::within_limits(
            &mut tx,
            evidence_scope,
            policies.organization.policy.monthly_estimated_budget_usd_nanos,
            policies.project.policy.monthly_estimated_budget_usd_nanos,
            &limits.clients,
            workflow_budget.map(|(_, month)| month),
        )
        .await?
        {
            return Err(ApiError::BudgetExceeded);
        }
    }
    if let Some((binding_digest, upstream_model, payload)) =
        provider_payload.as_ref().filter(|_| inserted)
    {
        gateway_requests::create_provider_run(
            &mut *tx,
            NewProviderRun {
                request: &id,
                binding_digest,
                upstream_model,
                payload,
            },
        )
        .await?;
        if let Some(frame) = staged_frame.as_deref() {
            gateway_requests::workflow_frames::insert(&mut *tx, &id, frame).await?;
        }
        if let Some(store) = providers.and_then(|runtime| runtime.content_archive.as_ref()) {
            if offer.adapter == Adapter::OpenAiCompatibleChat
                || (offer.adapter == Adapter::ConnectorQueue && store.retain_media)
            {
                gateway_requests::content_history::enroll(
                    &mut tx, &id, store.target(), archived_input.as_deref().unwrap_or(payload),
                ).await?;
            }
        }
    }
    if inserted {
        let retry_after = gateway_requests::submission_limits::consume(&mut tx, key.scope.account).await?;
        // Quote, capacity, and budget checks can take a lease past its deadline.
        if let Some((run, lease)) = workflow_lease {
            db_queries::workflows::lock_active_run(&mut *tx, run, lease).await?
                .ok_or_else(lease_ended)?;
        }
        // Automatic admissions also use a time-limited price. Recheck after the
        // budget and quota waits before making either reservation durable.
        if admission_deadline.is_some_and(|deadline| Utc::now() >= deadline) {
            return Err(ApiError::QuoteStale);
        }
        if retry_after > 0 {
            return Err(ApiError::SubmissionRateLimit(retry_after));
        }
    }
    tx.commit().await?;
    if inserted {
        tracing::debug!(request_id = %id, trace_id = %trace.trace_id, span_id = %trace.span_id, "request admitted");
        return Ok((accepted(id, key.scope.project, Some(trace.trace_id)), false));
    }
    let existing = replay(pool, key, &intent)
        .await?
        .ok_or_else(|| ApiError::Internal("idempotency admission disappeared".into()))?;
    Ok((existing, true))
}

fn accepted(id: GatewayRequestId, project_id: &GatewayProjectId, trace_id: Option<String>) -> AcceptedRequest {
    AcceptedRequest {
        status_url: format!("/v1/requests/{id}"),
        project_id: project_id.clone(),
        id,
        trace_id,
    }
}

async fn replay<'e>(
    exec: impl sqlx::PgExecutor<'e>,
    key: AdmissionKey<'_>,
    intent: &str,
) -> ApiResult<Option<AcceptedRequest>> {
    let Some(existing) = gateway_requests::find_admission(exec, key).await? else {
        return Ok(None);
    };
    if existing.intent_digest != intent {
        return Err(ApiError::Conflict(
            "idempotency key was already used for a different request".into(),
        ));
    }
    tracing::debug!(request_id = %existing.id, execution_trace_id = ?existing.trace_id, "request receipt replayed");
    Ok(Some(accepted(existing.id, key.scope.project, existing.trace_id)))
}

pub(super) fn validate_app_ref(value: Option<&str>) -> ApiResult<()> {
    validate_reference(value, "app_ref")
}

pub fn validate_client_ref(value: Option<&str>) -> ApiResult<()> {
    validate_reference(value, "client_ref")
}

fn validate_reference(value: Option<&str>, field: &str) -> ApiResult<()> {
    if value.is_some_and(|reference| !valid_identifier(reference)) {
        return Err(ApiError::BadRequest(
            format!("{field} must be an opaque identifier of 1 to 64 characters"),
        ));
    }
    Ok(())
}

pub(super) fn valid_idempotency_key(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

pub async fn get<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    scope: RequestScope<'_>,
    id: &GatewayRequestId,
) -> ApiResult<RequestView> {
    let row = gateway_requests::get_request(exec, scope, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("request not found".into()))?;
    RequestView::try_from(row)
}

pub async fn cancel(
    pool: &db::PgPool,
    scope: RequestScope<'_>,
    id: &GatewayRequestId,
) -> ApiResult<RequestView> {
    let mut tx = pool.begin().await?;
    if gateway_requests::cancel_request(&mut *tx, scope, id).await? {
        gateway_requests::cancel_mock_attempts(&mut *tx, scope, id).await?;
        gateway_requests::clear_provider_input(&mut *tx, id).await?;
    }
    tx.commit().await?;
    let request = get(pool, scope, id).await?;
    if request.status == RequestStatus::Canceled {
        Ok(request)
    } else {
        Err(ApiError::Conflict(
            "Cancellation is available until paid inference starts or the request ends.".into(),
        ))
    }
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum EventKind {
    Accepted,
    MockProviderSubmitted,
    MockProviderCompleted,
    Canceled,
    ProviderSubmitStarted,
    ProviderCompleted,
    ProviderFailed,
    ProviderOutcomeUnconfirmed,
    ProviderReconciled,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TimelineEvent {
    pub(super) sequence: u8,
    pub(super) kind: EventKind,
    pub(super) at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum ProviderRecheck {
    Available { until: DateTime<Utc> },
    Pending { requested_at: DateTime<Utc>, deadline: DateTime<Utc> },
    Finished { requested_at: DateTime<Utc> },
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TraceCorrelation {
    pub trace_id: String,
    pub admission_span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_span_id: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct AppliedClientBudget {
    revision: i32,
    limit_usd_nanos: i64,
}

#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = NativeRequest)]
pub struct RequestView {
    #[schema(value_type = String)]
    pub(super) id: GatewayRequestId,
    #[schema(value_type = String)]
    project_id: GatewayProjectId,
    pub(super) client_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) app_ref: Option<String>,
    #[schema(value_type = super::catalog::Provider)]
    provider: String,
    #[schema(schema_with = request_cost_state_schema)]
    pub(super) cost_state: CostState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) webhook_subscription_id: Option<String>,
    #[schema(schema_with = request_status_schema)]
    pub(super) status: RequestStatus,
    pub(super) can_cancel: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) dispatch_paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) recheck: Option<ProviderRecheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) correlation: Option<TraceCorrelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_budget: Option<AppliedClientBudget>,
    pub(super) quote: Quote,
    pub(super) provider_handle: Option<String>,
    pub(super) mock_reported_usd_nanos: Option<i64>,
    mock_result: Option<MockResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) supplier_settlement: Option<SupplierSettlement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content_removed_at: Option<DateTime<Utc>>,
    pub(super) events: Vec<TimelineEvent>,
}

#[derive(Serialize)]
pub(super) struct ModelRequestView {
    id: GatewayRequestId,
    created_at: DateTime<Utc>,
    client_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) app_ref: Option<String>,
    provider: String,
    pub(super) cost_state: CostState,
    webhook_subscription_id: Option<String>,
    status: RequestStatus,
    can_cancel: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch_paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recheck: Option<ProviderRecheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation: Option<TraceCorrelation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_budget: Option<AppliedClientBudget>,
    model: String,
    estimated_usd_nanos: Option<i64>,
    routing: Option<super::routing_policy::Resolution>,
    chat_options: ChatOptions,
    mock_reported_usd_nanos: Option<i64>,
    mock_result: Option<MockResult>,
    supplier_settlement: Option<SupplierSettlement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_removed_at: Option<DateTime<Utc>>,
    upstream: Option<ModelProcessing>,
}

#[derive(Serialize)]
struct ModelProcessing {
    result_expires_at: Option<DateTime<Utc>>,
    reported_usd_nanos: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_charge: Option<ProviderChargeView>,
    input_tokens: Option<i32>,
    output_tokens: Option<i32>,
    finish_reason: Option<String>,
    queue_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_observed_at: Option<DateTime<Utc>>,
}

impl From<RequestView> for ModelRequestView {
    fn from(request: RequestView) -> Self {
        let created_at = request.accepted_at();
        let quote = request.quote;
        let estimate = quote.selected_candidate()
            .and_then(|candidate| candidate.estimated_usd_nanos);
        Self {
            id: request.id,
            created_at,
            client_ref: request.client_ref,
            app_ref: request.app_ref,
            provider: request.provider,
            cost_state: request.cost_state,
            webhook_subscription_id: request.webhook_subscription_id,
            status: request.status,
            can_cancel: request.can_cancel,
            dispatch_paused: request.dispatch_paused,
            recheck: request.recheck,
            correlation: request.correlation,
            client_budget: request.client_budget,
            model: quote.selected_model().to_owned(),
            estimated_usd_nanos: estimate,
            routing: quote.routing,
            chat_options: quote.chat_options,
            mock_reported_usd_nanos: request.mock_reported_usd_nanos,
            mock_result: request.mock_result,
            supplier_settlement: request.supplier_settlement,
            content_removed_at: request.content_removed_at,
            upstream: request.upstream.map(|upstream| ModelProcessing {
                result_expires_at: upstream.result_expires_at,
                reported_usd_nanos: upstream.reported_usd_nanos,
                provider_charge: upstream.provider_charge,
                input_tokens: upstream.input_tokens,
                output_tokens: upstream.output_tokens,
                finish_reason: upstream.finish_reason,
                queue_state: upstream.queue_state,
                result_observed_at: upstream.result_observed_at,
            }),
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct SupplierSettlement {
    pub(super) revision: i32,
    pub(super) amount_usd_nanos: String,
    recorded_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProviderChargeView {
    pub(super) unit: String,
    pub(super) quantity: String,
    pub(super) observed_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct UpstreamView {
    pub(super) adapter: String,
    pub(super) requested_model: Option<String>,
    pub(super) reported_model: Option<String>,
    pub(super) reported_provider: Option<String>,
    finish_reason: Option<String>,
    pub(super) error_code: Option<String>,
    pub(super) result_expires_at: Option<DateTime<Utc>>,
    pub(super) input_tokens: Option<i32>,
    pub(super) output_tokens: Option<i32>,
    pub(super) reported_usd_nanos: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) provider_charge: Option<ProviderChargeView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) accepted_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) result_observed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) callback_received_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) callback_recovered_from: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) callback_reconciled_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) credential_version: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) credential_binding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) credential_scope: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stream_started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stream_detached_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProviderResultView {
    pub(super) result: ChatResult,
    expires_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(untagged)]
pub enum NativeProviderResult {
    Chat(ProviderResultView),
    Media {
        result: MediaResult,
        expires_at: DateTime<Utc>,
    },
}

pub async fn provider_result(
    pool: &db::PgPool,
    runtime: Option<&ProviderRuntime>,
    scope: RequestScope<'_>,
    id: &GatewayRequestId,
) -> ApiResult<ProviderResultView> {
    match native_provider_result(pool, runtime, scope, id).await? {
        NativeProviderResult::Chat(value) => Ok(value),
        NativeProviderResult::Media { .. } => {
            Err(ApiError::NotFound("chat result is unavailable".into()))
        }
    }
}

pub async fn native_provider_result(
    pool: &db::PgPool,
    runtime: Option<&ProviderRuntime>,
    scope: RequestScope<'_>,
    id: &GatewayRequestId,
) -> ApiResult<NativeProviderResult> {
    let not_found = || ApiError::NotFound("request result is unavailable or expired".into());
    let runtime = runtime.ok_or_else(not_found)?;
    let stored = gateway_requests::provider_result(pool, scope, id)
        .await?
        .ok_or_else(not_found)?;
    let invalid = || ApiError::Internal("could not read stored provider result".into());
    let decoded = runtime
        .payloads
        .open(id.as_str(), "result", &stored.payload)
        .ok_or_else(invalid)?;
    let expires_at = stored.expires_at;
    if matches!(stored.adapter.as_str(), "fal_queue" | "kinovi_queue" | "connector_queue") {
        let result = serde_json::from_slice(&decoded).map_err(|_| invalid())?;
        return Ok(NativeProviderResult::Media { result, expires_at });
    }
    let result = serde_json::from_slice(&decoded).map_err(|_| invalid())?;
    Ok(NativeProviderResult::Chat(ProviderResultView {
        result,
        expires_at,
    }))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RequestEvents {
    #[schema(value_type = String)]
    id: GatewayRequestId,
    events: Vec<TimelineEvent>,
}

impl RequestView {
    pub(super) fn fixture_output(&self) -> bool {
        self.mock_result.is_some()
    }

    pub(super) fn accepted_at(&self) -> DateTime<Utc> {
        self.events[0].at
    }

    pub(super) fn token_counts(&self) -> Option<(i32, i32)> {
        let usage = self.upstream.as_ref()?;
        Some((usage.input_tokens?, usage.output_tokens?))
    }

    pub fn into_events(self) -> RequestEvents {
        RequestEvents {
            id: self.id,
            events: self.events,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schema(as = MockRequestResult)]
enum MockResult {
    Image {
        count: u16,
        aspect_ratio: String,
    },
    Video {
        duration_seconds: u16,
        aspect_ratio: String,
        resolution: String,
    },
    Audio {
        duration_seconds: u16,
        format: String,
    },
    Mesh {
        format: String,
    },
    Text {
        text: &'static str,
    },
}

impl TryFrom<RequestRow> for RequestView {
    type Error = ApiError;

    fn try_from(row: RequestRow) -> ApiResult<Self> {
        let quote: Quote = serde_json::from_value(row.quote)
            .map_err(|_| ApiError::Internal("could not decode stored quote snapshot".into()))?;
        let mut events = vec![TimelineEvent {
            sequence: 1,
            kind: EventKind::Accepted,
            at: row.created_at,
        }];
        let mock = row.adapter == "mock";
        if let Some(at) = row.submitted_at {
            events.push(TimelineEvent {
                sequence: 2,
                kind: if mock {
                    EventKind::MockProviderSubmitted
                } else {
                    EventKind::ProviderSubmitStarted
                },
                at,
            });
        }
        if let (Some(unconfirmed), Some(callback)) = (
            row.provider_callback_recovered_from,
            row.provider_callback_reconciled_at.or(row.provider_callback_at),
        ) {
            events.push(TimelineEvent {
                sequence: 3,
                kind: EventKind::ProviderOutcomeUnconfirmed,
                at: unconfirmed,
            });
            events.push(TimelineEvent {
                sequence: 4,
                kind: EventKind::ProviderReconciled,
                at: callback,
            });
        }
        if let Some(from) = row.provider_recheck_unconfirmed_at {
            events.push(TimelineEvent {
                sequence: (events.len() + 1) as u8,
                kind: EventKind::ProviderOutcomeUnconfirmed,
                at: from,
            });
        }
        if let Some(at) = row.completed_at {
            events.push(TimelineEvent {
                sequence: (events.len() + 1) as u8,
                kind: if mock {
                    EventKind::MockProviderCompleted
                } else {
                    EventKind::ProviderCompleted
                },
                at,
            });
        }
        if let Some(at) = row.canceled_at {
            events.push(TimelineEvent {
                sequence: (events.len() + 1) as u8,
                kind: EventKind::Canceled,
                at,
            });
        }
        if let Some(at) = row.failed_at {
            events.push(TimelineEvent {
                sequence: (events.len() + 1) as u8,
                kind: EventKind::ProviderFailed,
                at,
            });
        }
        if let Some(at) = row.uncertain_at.filter(|at| Some(*at) != row.provider_recheck_unconfirmed_at) {
            events.push(TimelineEvent {
                sequence: (events.len() + 1) as u8,
                kind: EventKind::ProviderOutcomeUnconfirmed,
                at,
            });
        }
        let applied_aspect_ratio = quote
            .candidates
            .iter()
            .find(|candidate| Some(&candidate.offer_id) == quote.selected_offer_id.as_ref())
            .and_then(|candidate| candidate.applied_aspect_ratio.as_ref());
        let mock_result = row
            .completed_at
            .filter(|_| mock)
            .map(|_| match &quote.input.workload {
                Workload::Image {
                    count,
                    aspect_ratio,
                    ..
                } => MockResult::Image {
                    count: *count,
                    aspect_ratio: applied_aspect_ratio.unwrap_or(aspect_ratio).clone(),
                },
                Workload::Video {
                    duration_seconds,
                    aspect_ratio,
                    resolution,
                    ..
                } => MockResult::Video {
                    duration_seconds: *duration_seconds,
                    aspect_ratio: applied_aspect_ratio.unwrap_or(aspect_ratio).clone(),
                    resolution: resolution.clone(),
                },
                Workload::Audio {
                    duration_seconds,
                    format,
                } => MockResult::Audio {
                    duration_seconds: *duration_seconds,
                    format: format.clone(),
                },
                Workload::Mesh { format } => MockResult::Mesh {
                    format: format.clone(),
                },
                Workload::Text { .. } => MockResult::Text {
                    text: "Simulated model output.",
                },
            });
        Ok(Self {
            id: row.id,
            project_id: row.project_id,
            client_ref: row.client_ref,
            app_ref: row.app_ref,
            provider: row.provider,
            cost_state: row.cost_state,
            webhook_subscription_id: row.webhook_subscription_id,
            status: row.status,
            can_cancel: row.can_cancel,
            dispatch_paused: row.dispatch_paused.then_some(true),
            client_budget: row.client_budget_revision.zip(row.client_budget_limit_usd_nanos)
                .map(|(revision, limit_usd_nanos)| AppliedClientBudget { revision, limit_usd_nanos }),
            correlation: row.trace_id.zip(row.admission_span_id).map(|(trace_id, admission_span_id)| TraceCorrelation {
                trace_id,
                admission_span_id,
                caller_trace_id: row.caller_trace_id,
                caller_span_id: row.caller_span_id,
            }),
            recheck: match (row.provider_recheck_requested_at, row.provider_recheck_deadline) {
                (Some(requested_at), Some(deadline)) => Some(ProviderRecheck::Pending { requested_at, deadline }),
                (Some(requested_at), None) => Some(ProviderRecheck::Finished { requested_at }),
                (None, _) => row.provider_recheck_available_until.map(|until| ProviderRecheck::Available { until }),
            },
            quote,
            provider_handle: row.provider_handle,
            mock_reported_usd_nanos: row.reported_usd_nanos,
            mock_result,
            content_removed_at: row.content_removed_at,
            upstream: (!mock).then_some(UpstreamView {
                credential_binding: row.provider_credential_binding_slot.filter(|_| row.adapter == "fal_queue"),
                adapter: row.adapter,
                requested_model: row.upstream_model,
                reported_model: row.reported_model,
                reported_provider: row.reported_provider,
                finish_reason: row.finish_reason,
                error_code: row.error_code,
                result_expires_at: row.result_expires_at,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                reported_usd_nanos: row.upstream_usd_nanos.map(|cost| cost.to_string()),
                provider_charge: match (row.charge_unit, row.charge_quantity, row.charge_observed_at) {
                    (Some(unit), Some(quantity), Some(observed_at)) => Some(ProviderChargeView { unit, quantity, observed_at }),
                    _ => None,
                },
                accepted_at: row.provider_accepted_at,
                result_observed_at: row.provider_result_observed_at,
                queue_state: row.provider_queue_state,
                callback_received_at: row.provider_callback_at,
                callback_recovered_from: row.provider_callback_recovered_from,
                callback_reconciled_at: row.provider_callback_reconciled_at,
                credential_version: row.provider_credential_version,
                credential_scope: row.organization_credential.map(|shared| {
                    if shared { "organization" } else { "project" }
                }),
                stream_started_at: row.stream_started_at,
                stream_detached_at: row.stream_detached_at,
            }),
            supplier_settlement: match (
                row.settlement_revision,
                row.settlement_amount,
                row.settlement_recorded_at,
            ) {
                (Some(revision), Some(amount), Some(recorded_at)) => Some(SupplierSettlement {
                    revision,
                    amount_usd_nanos: amount.to_string(),
                    recorded_at,
                }),
                _ => None,
            },
            events,
        })
    }
}
