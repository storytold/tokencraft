use std::sync::Arc;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use db_queries::gateway_mcp::Grant;
use ids::GatewayRequestId;
use ring::hmac;
use serde::{Deserialize, Serialize};

use super::super::{
    access::{self, Access, Permission},
    catalog::{public_model_id, Adapter, Catalog, Offer, Pricing, Provider},
    catalog_refresh::CatalogStore,
    debug_trace,
    handlers,
    providers::ProviderRuntime,
    quote::{QuoteInput, Workload},
    quote_seals::{self, QuoteSeals},
    requests::{self, AdmissionInput, NativeProviderResult, QuoteSelection, RequestStatus, RequestView},
    routing_policy::Policies,
    usage,
};
use super::settings::Settings;
use crate::{error::{ApiError, ApiResult}, state::AppState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModelInput {
    model: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestInput {
    #[serde(deserialize_with = "request_id")]
    request_id: GatewayRequestId,
}

fn request_id<'de, D: serde::Deserializer<'de>>(value: D) -> Result<GatewayRequestId, D::Error> {
    String::deserialize(value)?.parse().map_err(serde::de::Error::custom)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Generation {
    model: String,
    prompt: String,
    max_total_usd_nanos: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_providers: Option<Vec<Provider>>,
    media: Workload,
}

impl Generation {
    fn normalize(&mut self) -> ApiResult<QuoteInput> {
        use super::super::providers::video;

        let fits = match &self.media {
            Workload::Image { .. } => self.prompt.len() <= 4000,
            Workload::Video { .. } => self.prompt.len() <= video::MAX_PROMPT_BYTES,
            Workload::Audio { .. } => self.prompt.len() <= 2048,
            Workload::Mesh { .. } => self.prompt.chars().count() <= 600,
            Workload::Text { .. } => return Err(ApiError::BadRequest("use a media workload".into())),
        };
        if self.prompt.trim().is_empty() || !fits {
            return Err(ApiError::BadRequest("prompt is empty or exceeds the workload limit".into()));
        }
        self.model = public_model_id(&self.model).to_owned();
        let mut input = QuoteInput {
            data_requirements: None,
            min_quality: None,
            min_usage_accounts: None,
            model: self.model.clone(),
            workload: self.media.clone(),
            provider: None,
            preferred_providers: self.preferred_providers.clone(),
            max_total_usd_nanos: Some(self.max_total_usd_nanos),
        };
        input.normalize()?;
        Ok(input)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Submission {
    intent: Generation,
    quote_handle: String,
    idempotency_key: String,
    app_ref: Option<String>,
}

#[derive(Serialize)]
pub(super) struct Model<'a> {
    id: &'a str,
    media: Vec<Capability<'a>>,
}

#[derive(Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Capability<'a> {
    Image { aspect_ratios: &'a [String] },
    Video { durations_seconds: &'a [u16], aspect_ratios: &'a [String], resolution: &'a str },
    Audio { durations_seconds: &'a [u16], format: &'a str },
    Mesh { format: &'a str },
}

#[derive(Serialize)]
pub(super) struct Models<'a> {
    catalog_revision: &'a str,
    models: Vec<Model<'a>>,
}

#[derive(Serialize)]
pub(super) struct Estimate {
    model: String,
    catalog_revision: String,
    quote_handle: String,
    expires_at: Option<DateTime<Utc>>,
    currency: String,
    estimated_usd_nanos: String,
    max_total_usd_nanos: String,
    requested_aspect_ratio: Option<String>,
    applied_aspect_ratio: Option<String>,
}

#[derive(Serialize)]
pub(super) struct Receipt {
    request_id: GatewayRequestId,
    duplicate: bool,
    dashboard_url: String,
}

#[derive(Serialize)]
pub(super) struct Status {
    request_id: GatewayRequestId,
    model: String,
    status: RequestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_ref: Option<String>,
    poll_after_seconds: Option<u16>,
    dashboard_url: String,
    estimated_usd_nanos: Option<String>,
    provider_reported_usd_nanos: Option<String>,
    supplier_settled_usd_nanos: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ResultOutput {
    request_id: GatewayRequestId,
    dashboard_url: String,
    #[serde(flatten)]
    result: NativeProviderResult,
}

#[derive(Serialize)]
pub(super) struct Trace {
    dashboard_url: String,
    revision: String,
    observed_at: DateTime<Utc>,
    evidence: debug_trace::Evidence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct History {
    #[serde(default)]
    after: Option<String>,
    app_ref: Option<String>,
}

#[derive(Serialize)]
pub(super) struct RequestPage {
    requests: Vec<Status>,
    next_after: Option<GatewayRequestId>,
}

pub(super) struct Gateway {
    pub state: AppState,
    pub catalog: Arc<CatalogStore>,
    pub quote_seals: Arc<QuoteSeals>,
    pub providers: Option<Arc<ProviderRuntime>>,
}

impl Gateway {
    fn runnable_offer(&self, offer: &Offer) -> bool {
        offer.adapter == Adapter::ConnectorQueue
            && self.providers.as_ref().is_some_and(|runtime| runtime.queue_offer_binding(offer).is_some())
    }

    pub fn models<'a>(&self, catalog: &'a Catalog) -> Models<'a> {
        let mut models = std::collections::BTreeMap::<&str, Vec<Capability<'a>>>::new();
        for offer in catalog.offers.iter().filter(|offer| self.runnable_offer(offer)) {
            let capability = match &offer.pricing {
                Pricing::Image { aspect_ratios, .. } => Capability::Image { aspect_ratios },
                Pricing::Video { durations_seconds, aspect_ratios, resolution, .. } => {
                    Capability::Video { durations_seconds, aspect_ratios, resolution }
                }
                Pricing::Audio { durations_seconds, format, .. } => Capability::Audio { durations_seconds, format },
                Pricing::Mesh { format, .. } => Capability::Mesh { format },
                Pricing::Text { .. } => continue,
            };
            let choices = models.entry(public_model_id(&offer.model)).or_default();
            if !choices.contains(&capability) {
                choices.push(capability);
            }
        }
        Models {
            catalog_revision: &catalog.revision,
            models: models.into_iter().map(|(id, media)| Model { id, media }).collect(),
        }
    }

    pub fn model<'a>(&self, catalog: &'a Catalog, input: ModelInput) -> ApiResult<Model<'a>> {
        self.models(catalog).models.into_iter().find(|model| model.id == input.model)
            .ok_or_else(|| ApiError::NotFound("model has no connected media offer".into()))
    }

    pub async fn quote(&self, settings: &Settings, grant: &Grant, mut input: Generation) -> ApiResult<Estimate> {
        let access = self.access(grant, Permission::ReadRequests).await?;
        let request = input.normalize()?;
        let policies = Policies::load(&self.state.pool, access.scope()).await?;
        let catalog = self.catalog.current();
        let availability = super::super::availability::RouteAvailability::for_pool(
            &self.state.pool, super::super::availability::AvailabilityInput {
                scope: access.scope(), catalog: &catalog, providers: self.providers.as_deref(),
                quote: &request, policies: &policies,
            },
        ).await?;
        let quote = catalog.scoped_media_quote(request, access.scope(), &policies, &availability, |offer| self.runnable_offer(offer))?;
        let mut quote = super::super::route_performance::rank(
            &self.state.pool, access.scope(), &catalog, self.providers.as_deref(), quote,
        ).await?;
        self.quote_seals.seal(&mut quote, access.scope())?;
        let selected = quote.selected_candidate().ok_or_else(|| ApiError::BadRequest(
            "no enabled offer matches this intent and estimate ceiling".into(),
        ))?;
        let estimated = selected.estimated_usd_nanos
            .ok_or_else(|| ApiError::Internal("selected offer has no estimate".into()))?;
        let aspect_ratio = match &quote.input.workload {
            Workload::Image { aspect_ratio, .. } | Workload::Video { aspect_ratio, .. } => Some(aspect_ratio),
            _ => None,
        };
        Ok(Estimate {
            model: quote.selected_model().to_owned(),
            catalog_revision: quote.catalog_revision.clone(),
            quote_handle: sign_quote(settings, &access, &input, &quote.id)?,
            expires_at: quote.expires_at,
            currency: quote.currency.clone(),
            estimated_usd_nanos: estimated.to_string(),
            max_total_usd_nanos: input.max_total_usd_nanos.to_string(),
            requested_aspect_ratio: aspect_ratio.cloned(),
            applied_aspect_ratio: selected.applied_aspect_ratio.as_ref().or(aspect_ratio).cloned(),
        })
    }

    pub async fn submit(&self, settings: &Settings, grant: &Grant, mut input: Submission) -> ApiResult<Receipt> {
        let access = self.access(grant, Permission::SubmitRequests).await?;
        let request = input.intent.normalize()?;
        let id = verify_quote(settings, &access, &input.intent, &input.quote_handle)?;
        let mut admitted = AdmissionInput {
            pinned_offer_id: None,
            selection: QuoteSelection::DeclaredMedia(id),
            input: request,
            client_ref: None,
            app_ref: input.app_ref,
            webhook_subscription_id: None,
            messages: None,
            image_prompt: None,
            video_prompt: None,
            audio_prompt: None,
            mesh_prompt: None,
            chat_options: Default::default(),
            stream: false,
        };
        let prompt = Some(input.intent.prompt);
        match &admitted.input.workload {
            Workload::Image { .. } => admitted.image_prompt = prompt,
            Workload::Video { .. } => admitted.video_prompt = prompt,
            Workload::Audio { .. } => admitted.audio_prompt = prompt,
            Workload::Mesh { .. } => admitted.mesh_prompt = prompt,
            Workload::Text { .. } => return Err(ApiError::BadRequest("use a media workload".into())),
        }
        let (receipt, duplicate) = requests::submit(
            &self.state.pool, &self.catalog.current(), None, self.providers.as_deref(), Some(&self.quote_seals), &access,
            &input.idempotency_key, admitted,
        ).await?;
        Ok(Receipt {
            dashboard_url: self.dashboard(&access, &receipt.id)?,
            request_id: receipt.id,
            duplicate,
        })
    }

    pub async fn status(&self, grant: &Grant, input: RequestInput) -> ApiResult<Status> {
        let access = self.access(grant, Permission::ReadRequests).await?;
        let request = requests::get(&self.state.pool, access.scope(), &input.request_id).await?;
        self.status_view(&access, request)
    }

    pub async fn list(&self, grant: &Grant, input: History) -> ApiResult<RequestPage> {
        let access = self.access(grant, Permission::ReadRequests).await?;
        let mut query = handlers::HistoryQuery::default();
        query.after = input.after;
        query.app_ref = input.app_ref;
        let page = handlers::history(&self.state.pool, access.scope(), &query).await?;
        let requests = page.requests.into_iter().map(|request| self.status_view(&access, request)).collect::<ApiResult<_>>()?;
        Ok(RequestPage { requests, next_after: page.next_after })
    }

    pub async fn cancel(&self, grant: &Grant, input: RequestInput) -> ApiResult<Status> {
        let access = self.access(grant, Permission::CancelRequests).await?;
        let request = requests::cancel(&self.state.pool, access.scope(), &input.request_id).await?;
        self.status_view(&access, request)
    }

    pub async fn result(&self, grant: &Grant, input: RequestInput) -> ApiResult<ResultOutput> {
        let access = self.access(grant, Permission::ReadRequests).await?;
        let result = requests::native_provider_result(
            &self.state.pool, self.providers.as_deref(), access.scope(), &input.request_id,
        ).await?;
        Ok(ResultOutput {
            dashboard_url: self.dashboard(&access, &input.request_id)?,
            request_id: input.request_id,
            result,
        })
    }

    pub async fn trace(&self, grant: &Grant, input: RequestInput) -> ApiResult<Trace> {
        let access = self.access(grant, Permission::ReadRequests).await?;
        let dashboard_url = self.dashboard(&access, &input.request_id)?;
        let snapshot = debug_trace::load(&self.state.pool, access.scope(), input.request_id, false).await?;
        Ok(Trace {
            dashboard_url,
            revision: snapshot.revision,
            observed_at: snapshot.observed_at,
            evidence: snapshot.evidence,
        })
    }

    pub async fn usage(&self, grant: &Grant, input: usage::UsageQuery) -> ApiResult<usage::UsageReport> {
        let access = self.access(grant, Permission::ReadUsage).await?;
        usage::report_for(&self.state.pool, &access, input).await
    }

    pub async fn access(&self, grant: &Grant, permission: Permission) -> ApiResult<Access> {
        access::delegated(&self.state.pool, grant, permission).await
    }

    fn status_view(&self, access: &Access, request: RequestView) -> ApiResult<Status> {
        let poll_after = match request.status {
            RequestStatus::Queued | RequestStatus::Running => Some(2),
            RequestStatus::AwaitingReconciliation => Some(10),
            _ => None,
        };
        Ok(Status {
            dashboard_url: self.dashboard(access, &request.id)?,
            request_id: request.id,
            model: request.quote.selected_model().to_owned(),
            status: request.status,
            app_ref: request.app_ref,
            poll_after_seconds: poll_after,
            estimated_usd_nanos: request.quote.selected_candidate()
                .and_then(|candidate| candidate.estimated_usd_nanos)
                .map(|amount| amount.to_string()),
            provider_reported_usd_nanos: request.upstream.and_then(|provider| provider.reported_usd_nanos),
            supplier_settled_usd_nanos: request.supplier_settlement.map(|cost| cost.amount_usd_nanos),
        })
    }

    fn dashboard(&self, access: &Access, id: &GatewayRequestId) -> ApiResult<String> {
        let scope = access.scope();
        let path = format!("/dashboard/{}/requests", scope.account);
        let mut url = reqwest::Url::parse(&self.state.app_base_url).and_then(|base| base.join(&path))
            .map_err(|_| ApiError::Internal("invalid TokenCraft dashboard URL".into()))?;
        url.query_pairs_mut().append_pair("project_id", scope.project.as_str()).append_pair("request_id", id.as_str());
        Ok(url.to_string())
    }
}

fn sign_quote(settings: &Settings, access: &Access, input: &Generation, id: &str) -> ApiResult<String> {
    let body = quote_binding(access, input, id)?;
    let signature = hmac::sign(&settings.quote_key, &body);
    Ok(format!("{id}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref())))
}

fn verify_quote(settings: &Settings, access: &Access, input: &Generation, handle: &str) -> ApiResult<String> {
    let invalid = || ApiError::BadRequest("quote handle does not match this intent; request a fresh quote".into());
    if handle.len() > quote_seals::MAX_QUOTE_ID_BYTES + 44 {
        return Err(invalid());
    }
    let (id, sig) = handle.split_once('.').ok_or_else(invalid)?;
    if !quote_seals::valid_id(id) || sig.len() != 43 {
        return Err(invalid());
    }
    let signature = URL_SAFE_NO_PAD.decode(sig).map_err(|_| invalid())?;
    let body = quote_binding(access, input, id)?;
    hmac::verify(&settings.quote_key, &body, &signature).map_err(|_| invalid())?;
    Ok(id.to_owned())
}

fn quote_binding(access: &Access, input: &Generation, id: &str) -> ApiResult<Vec<u8>> {
    let scope = access.scope();
    serde_json::to_vec(&(scope.account, scope.project, scope.client_ref, &access.actor_id, id, input))
        .map_err(|_| ApiError::Internal("could not bind quote intent".into()))
}
