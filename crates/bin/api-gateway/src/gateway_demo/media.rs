use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use utoipa::ToSchema;

use super::access::{self, project_key_suffix, GatewayCaller, Permission, ScopeQuery};
use super::adaptive::{self, AdvisorRoute, AdvisorSelection, AdvisorSource};
use super::catalog::Provider;
use super::catalog_refresh::CatalogStore;
use super::data_handling::DataRequirements;
use super::handlers::{idempotency_key, receipt, single_header};
use super::providers::ProviderRuntime;
use super::quote::{QuoteInput, Workload};
use super::requests::{self, AdmissionInput, QuoteSelection};
use super::routing_policy::Policies;
use super::webhooks::WebhookConfig;
use super::{CLIENT_REF, MAX_COST_NANOS, WEBHOOK_SUBSCRIPTION_ID};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Clone)]
pub(super) struct MediaGateway {
    pub(super) app: AppState,
    pub(super) catalog: Arc<CatalogStore>,
    pub(super) quote_seals: Arc<super::quote_seals::QuoteSeals>,
    pub(super) providers: Option<Arc<ProviderRuntime>>,
    pub(super) webhooks: Option<Arc<WebhookConfig>>,
}

#[derive(Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum Task {
    TextToImage,
    TextToVideo,
    TextToMusic,
    TextToSoundEffect,
    #[serde(rename = "text_to_3d")]
    TextToMesh,
}

/// Flat media request used with a scoped project bearer key in the local provider profile.
#[derive(Deserialize, ToSchema)]
#[schema(as = ScopedMediaRequest)]
#[serde(deny_unknown_fields)]
pub(crate) struct MediaRequest {
    /// A model ID, "auto", or "advise". Omission uses the account or project setting.
    model: Option<String>,
    min_quality: Option<u8>,
    min_usage_accounts: Option<u32>,
    task: Task,
    prompt: String,
    /// Optional ID from a dry run on this route. The estimate must still be current.
    quote_id: Option<String>,
    #[serde(default)]
    dry_run: bool,
    /// Output count. Images accept 1–8. Other media accept one output.
    count: Option<u16>,
    aspect_ratio: Option<String>,
    /// Image or video routing tolerance in basis points. Omit to use project policy.
    max_relative_error_bps: Option<u16>,
    resolution: Option<String>,
    duration_seconds: Option<u16>,
    output_format: Option<String>,
    /// Whole USD micros. Alternatively use the x-tokencraft-max-cost-nanos header.
    max_estimated_cost_micros: Option<i64>,
    /// Optional provider priority. An empty list selects by lowest estimated cost.
    preferred_providers: Option<Vec<Provider>>,
    data_requirements: Option<DataRequirements>,
}

impl MediaRequest {
    fn workload(&self, path: &str) -> ApiResult<Workload> {
        let count = self.count.unwrap_or(1);
        let format = self.output_format.as_deref();
        let missing = |field| ApiError::BadRequest(format!("{field} is required for this task"));
        let incompatible =
            || ApiError::BadRequest("unsupported settings for this media task".into());
        Ok(match (path, self.task) {
            ("/v1/image", Task::TextToImage) => {
                if self.duration_seconds.is_some()
                    || self.resolution.is_some()
                    || !matches!(format, None | Some("png"))
                {
                    return Err(incompatible());
                }
                Workload::Image {
                    count,
                    aspect_ratio: self.aspect_ratio.clone().unwrap_or_else(|| "1:1".into()),
                    max_relative_error_bps: self.max_relative_error_bps,
                }
            }
            ("/v1/video", Task::TextToVideo) => {
                if count != 1 || !matches!(format, None | Some("mp4")) {
                    return Err(incompatible());
                }
                Workload::Video {
                    duration_seconds: self
                        .duration_seconds
                        .ok_or_else(|| missing("duration_seconds"))?,
                    aspect_ratio: self
                        .aspect_ratio
                        .clone()
                        .ok_or_else(|| missing("aspect_ratio"))?,
                    resolution: self
                        .resolution
                        .clone()
                        .ok_or_else(|| missing("resolution"))?,
                    max_relative_error_bps: self.max_relative_error_bps,
                }
            }
            ("/v1/audio", Task::TextToMusic | Task::TextToSoundEffect) => {
                if count != 1
                    || self.aspect_ratio.is_some()
                    || self.resolution.is_some()
                    || self.max_relative_error_bps.is_some()
                {
                    return Err(incompatible());
                }
                Workload::Audio {
                    duration_seconds: self
                        .duration_seconds
                        .ok_or_else(|| missing("duration_seconds"))?,
                    format: format.unwrap_or("wav").into(),
                }
            }
            ("/v1/3d", Task::TextToMesh) => {
                if count != 1
                    || self.duration_seconds.is_some()
                    || self.aspect_ratio.is_some()
                    || self.resolution.is_some()
                    || self.max_relative_error_bps.is_some()
                {
                    return Err(incompatible());
                }
                Workload::Mesh {
                    format: format.unwrap_or("glb").into(),
                }
            }
            _ => {
                return Err(ApiError::BadRequest(
                    "task does not match the media endpoint".into(),
                ))
            }
        })
    }

    fn into_admission(self, path: &str, headers: &HeaderMap) -> ApiResult<AdmissionInput> {
        let workload = self.workload(path)?;
        let max_total_usd_nanos = match (
            self.max_estimated_cost_micros,
            single_header(headers, MAX_COST_NANOS)?,
        ) {
            (Some(_), Some(_)) => {
                return Err(ApiError::BadRequest(
                    "supply only one estimated cost cap".into(),
                ))
            }
            (Some(micros), None) => Some(
                micros
                    .checked_mul(1_000)
                    .ok_or_else(|| ApiError::BadRequest("invalid micro-USD cost cap".into()))?,
            ),
            (None, Some(nanos)) => Some(
                nanos
                    .parse::<i64>()
                    .map_err(|_| ApiError::BadRequest("invalid nano-USD cost cap".into()))?,
            ),
            (None, None) => None,
        };
        let (image_prompt, video_prompt, audio_prompt, mesh_prompt) = match &workload {
            Workload::Image { .. } => (Some(self.prompt), None, None, None),
            Workload::Video { .. } => (None, Some(self.prompt), None, None),
            Workload::Audio { .. } => (None, None, Some(self.prompt), None),
            Workload::Mesh { .. } => (None, None, None, Some(self.prompt)),
            Workload::Text { .. } => unreachable!("the media endpoints build media workloads"),
        };
        let advisor_source = match self.model.as_deref() {
            Some("advise") => Some(AdvisorSource::Request),
            None => Some(AdvisorSource::Policy),
            _ => None,
        };
        let selection = match advisor_source {
            Some(source) => {
                if self.quote_id.is_some() {
                    return Err(ApiError::BadRequest(
                        "submit an advised quote using the model returned by the preview".into(),
                    ));
                }
                QuoteSelection::Advised(AdvisorSelection { source })
            }
            None => self.quote_id.map_or(QuoteSelection::Automatic, QuoteSelection::DeclaredMedia),
        };
        Ok(AdmissionInput {
            pinned_offer_id: None,
            selection,
            input: QuoteInput {
                data_requirements: self.data_requirements,
                min_quality: self.min_quality,
                min_usage_accounts: self.min_usage_accounts,
                model: if advisor_source.is_some() { "auto".into() } else { self.model.unwrap_or_default() },
                workload,
                provider: None,
                preferred_providers: self.preferred_providers,
                max_total_usd_nanos,
            },
            client_ref: single_header(headers, CLIENT_REF)?.map(str::to_owned),
            app_ref: single_header(headers, requests::APP_REF)?.map(str::to_owned),
            webhook_subscription_id: single_header(headers, WEBHOOK_SUBSCRIPTION_ID)?
                .map(str::to_owned),
            messages: None,
            image_prompt,
            video_prompt,
            audio_prompt,
            mesh_prompt,
            chat_options: Default::default(),
            stream: false,
        })
    }
}

pub(super) async fn project_media(
    State(gateway): State<Option<MediaGateway>>,
    request: Request,
    next: Next,
) -> Response {
    if request.method() != Method::POST
        || !request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .any(|value| project_key_suffix(value).is_some())
    {
        return next.run(request).await;
    }
    let result = match gateway {
        Some(gateway) => run(&gateway, request).await,
        None => Err(ApiError::ServiceUnavailable(
            "scoped media profile is unavailable".into(),
        )),
    };
    result.into_response()
}

async fn run(gateway: &MediaGateway, request: Request) -> ApiResult<Response> {
    let (mut parts, body) = request.into_parts();
    single_header(&parts.headers, "authorization")?;
    let caller = GatewayCaller::from_request_parts(&mut parts, &gateway.app).await?;
    let Query(scope) = Query::<ScopeQuery>::try_from_uri(&parts.uri)
        .map_err(|_| ApiError::BadRequest("invalid project scope".into()))?;
    let access = access::resolve(&gateway.app, caller, scope, Permission::SubmitRequests).await?;
    let body = to_bytes(body, 8192).await.map_err(|_| {
        ApiError::BadRequest("could not read media request within the 8 KiB body limit".into())
    })?;
    let body: MediaRequest = serde_json::from_slice(&body)
        .map_err(|_| ApiError::BadRequest("invalid scoped media request body".into()))?;
    let dry_run = body.dry_run;
    if dry_run && body.quote_id.is_some() {
        return Err(ApiError::BadRequest(
            "omit quote_id when previewing a media request".into(),
        ));
    }
    let mut admission = body.into_admission(parts.uri.path(), &parts.headers)?;
    let catalog = gateway.catalog.current();
    let pool = &gateway.app.pool;
    if dry_run {
        admission.validate_prompts()?;
        let scope = access.for_client(admission.client_ref.as_deref())?;
        admission.input.normalize()?;
        let decision = match &admission.selection {
            QuoteSelection::Advised(selection) => adaptive::select(AdvisorRoute {
                pool, scope, catalog: &catalog, providers: gateway.providers.as_deref(),
                selection, input: &admission.input,
            }).await?,
            _ => None,
        };
        if let Some(advice) = &decision {
            // A preview returns a concrete model so execution can replay the quote without AI.
            admission.input.model = advice.model.clone();
            admission.input.min_quality = None;
            admission.input.min_usage_accounts = None;
        }
        let policies = Policies::load(pool, scope).await?;
        let availability = super::availability::RouteAvailability::for_pool(
            pool, super::availability::AvailabilityInput {
                scope, catalog: &catalog, providers: gateway.providers.as_deref(),
                quote: &admission.input, policies: &policies,
            },
        ).await?;
        let quote = requests::media_quote(
            &catalog,
            admission.input,
            scope,
            &policies,
            gateway.providers.as_deref(),
            &availability,
        )?;
        let mut quote = super::route_performance::rank(pool, scope, &catalog, gateway.providers.as_deref(), quote).await?;
        if matches!(admission.selection, QuoteSelection::Advised(_))
            && decision.is_some() != quote.selected_offer_id.is_some()
        {
            return Err(ApiError::Conflict("model eligibility changed; preview the request again".into()));
        }
        quote.advisor = decision;
        gateway.quote_seals.seal(&mut quote, scope)?;
        super::route_performance::apply_visibility(&access, &mut quote);
        return Ok(Json(catalog.model_quote(quote)).into_response());
    }
    let (mut accepted, replay) = requests::submit(
        pool,
        &catalog,
        gateway.webhooks.as_deref(),
        gateway.providers.as_deref(),
        Some(&gateway.quote_seals),
        &access,
        idempotency_key(&parts.headers)?,
        admission,
    )
    .await?;
    accepted.status_url = format!("/v1/model-requests/{}", accepted.id);
    Ok(receipt(accepted, replay)?.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn request(model: Option<&str>, quote: Option<&str>) -> ApiResult<AdmissionInput> {
        let mut body = json!({"task":"text_to_image","prompt":"Generate a label."});
        if let Some(model) = model {
            body["model"] = Value::String(model.into());
        }
        if let Some(quote) = quote {
            body["quote_id"] = Value::String(quote.into());
        }
        serde_json::from_value::<MediaRequest>(body).unwrap()
            .into_admission("/v1/image", &HeaderMap::new())
    }

    #[test]
    fn only_missing_or_explicit_advise_can_use_the_advisor() {
        for (model, source) in [(None, AdvisorSource::Policy), (Some("advise"), AdvisorSource::Request)] {
            let request = request(model, None).unwrap();
            assert!(matches!(request.selection, QuoteSelection::Advised(selection) if selection.source == source));
            assert!(request.input.is_auto());
        }
        for model in ["auto", "openai_gpt_image_1"] {
            let request = request(Some(model), None).unwrap();
            assert!(matches!(request.selection, QuoteSelection::Automatic));
            assert_eq!(request.input.model, model);
        }
        assert!(request(Some("advise"), Some("quo_example")).is_err());
        assert!(matches!(
            request(Some("openai_gpt_image_1"), Some("quo_example")).unwrap().selection,
            QuoteSelection::DeclaredMedia(_)
        ));
    }
}
