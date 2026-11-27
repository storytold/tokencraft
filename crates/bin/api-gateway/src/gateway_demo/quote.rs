use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use db_queries::gateway_requests::RequestScope;

use super::data_handling::{DataHandling, DataMismatch, DataRequirements};
use super::availability::RouteAvailability;
use super::route_performance::{OfferPerformance, PerformanceWindow};
use serde::{Deserialize, Serialize};

use super::catalog::{digest, public_model_id, valid_identifier, Catalog, Offer, Pricing, Provider};
use super::chat_input::ChatOptions;
use super::routing_policy::{
    validate_model_floors, validate_provider_order, Policies, Resolution, Setting, Source, MAX_COST_NANOS, MAX_TOLERANCE_BPS,
};
use crate::error::{ApiError, ApiResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AspectRatio(u32, u32);

impl FromStr for AspectRatio {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        const ERROR: &str = "aspect ratio must have two integers from 1 to 100";
        let (w, h) = value.split_once(':').ok_or(ERROR)?;
        let parse = |part: &str| {
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ERROR);
            }
            part.parse::<u32>()
                .ok()
                .filter(|n| (1..=100).contains(n))
                .ok_or(ERROR)
        };
        let (w, h) = (parse(w)?, parse(h)?);
        let (mut a, mut b) = (w, h);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Ok(Self(w / a, h / a))
    }
}

impl fmt::Display for AspectRatio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.0, self.1)
    }
}

impl AspectRatio {
    fn orientation(self) -> Ordering {
        self.0.cmp(&self.1)
    }

    fn distance(self, candidate: Self) -> (u64, u64) {
        let requested = u64::from(self.0 * candidate.1);
        let applied = u64::from(candidate.0 * self.1);
        (requested.abs_diff(applied) * 10_000, requested)
    }

    fn nearest(self, ratios: &[String], tolerance: u16) -> Option<AspectRatio> {
        ratios
            .iter()
            .filter_map(|ratio| ratio.parse::<Self>().ok())
            .filter(|ratio| ratio.orientation() == self.orientation())
            .filter(|ratio| {
                let (delta, denominator) = self.distance(*ratio);
                delta <= u64::from(tolerance) * denominator
            })
            .min_by(|a, b| {
                let (an, ad) = self.distance(*a);
                let (bn, bd) = self.distance(*b);
                (an * bd).cmp(&(bn * ad))
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = GatewayQuoteInput)]
pub struct QuoteInput {
    /// A catalog model ID, or "auto" on the scoped gateway.
    pub model: String,
    /// Minimum community average. Applies to automatic model selection only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1, maximum = 10)]
    pub min_quality: Option<u8>,
    /// Minimum distinct organizations with a successful gateway request over the last 30 days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 5, maximum = 10000)]
    pub min_usage_accounts: Option<u32>,
    pub workload: Workload,
    pub provider: Option<Provider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_providers: Option<Vec<Provider>>,
    pub max_total_usd_nanos: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_requirements: Option<DataRequirements>,
}

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = QuoteWorkload)]
pub enum Workload {
    Image {
        count: u16,
        aspect_ratio: String,
        max_relative_error_bps: Option<u16>,
    },
    Text {
        max_input_tokens: u32,
        max_output_tokens: u32,
    },
    Video {
        duration_seconds: u16,
        aspect_ratio: String,
        resolution: String,
        /// Allow the closest ratio with the same orientation, within this tolerance.
        #[schema(maximum = 5000)]
        #[serde(skip_serializing_if = "Option::is_none")]
        max_relative_error_bps: Option<u16>,
    },
    Audio {
        duration_seconds: u16,
        format: String,
    },
    Mesh {
        format: String,
    },
}

impl QuoteInput {
    pub(super) fn is_auto(&self) -> bool {
        self.model == "auto"
    }

    pub(super) fn validate_model_selection(&self) -> ApiResult<()> {
        validate_model_floors(self.min_quality, self.min_usage_accounts)?;
        if !self.is_auto() && (self.min_quality.is_some() || self.min_usage_accounts.is_some()) {
            return Err(ApiError::BadRequest("model thresholds require model: auto".into()));
        }
        Ok(())
    }

    pub(super) fn considers(&self, offer: &Offer) -> bool {
        if self.is_auto() {
            matches!(
                (&self.workload, &offer.pricing),
                (Workload::Image { .. }, Pricing::Image { .. })
                    | (Workload::Text { .. }, Pricing::Text { .. })
                    | (Workload::Video { .. }, Pricing::Video { .. })
                    | (Workload::Audio { .. }, Pricing::Audio { .. })
                    | (Workload::Mesh { .. }, Pricing::Mesh { .. })
            )
        } else {
            public_model_id(&offer.model) == public_model_id(&self.model)
        }
    }

    pub(super) fn normalize(&mut self) -> ApiResult<()> {
        self.validate_model_selection()?;
        if let Some(data) = &mut self.data_requirements {
            data.normalize().map_err(|message| ApiError::BadRequest(message.into()))?;
            if data.is_empty() {
                self.data_requirements = None;
            }
        }
        validate_provider_order(self.preferred_providers.as_deref())?;
        if !valid_identifier(&self.model)
            || self
                .max_total_usd_nanos
                .is_some_and(|cap| !(0..=MAX_COST_NANOS).contains(&cap))
        {
            return Err(ApiError::BadRequest("invalid model or cost cap".into()));
        }
        match &mut self.workload {
            Workload::Image {
                count,
                aspect_ratio,
                max_relative_error_bps,
            } => {
                if !(1..=8).contains(count)
                    || max_relative_error_bps.is_some_and(|limit| limit > MAX_TOLERANCE_BPS)
                {
                    return Err(ApiError::BadRequest(
                        "invalid image count or ratio tolerance".into(),
                    ));
                }
                *aspect_ratio = aspect_ratio
                    .parse::<AspectRatio>()
                    .map_err(|e| ApiError::BadRequest(e.into()))?
                    .to_string();
            }
            Workload::Text {
                max_input_tokens,
                max_output_tokens,
            } => {
                if !(1..=128_000).contains(max_input_tokens)
                    || !(1..=64_000).contains(max_output_tokens)
                {
                    return Err(ApiError::BadRequest("invalid token bounds".into()));
                }
            }
            Workload::Video {
                duration_seconds,
                aspect_ratio,
                resolution,
                max_relative_error_bps,
            } => {
                if !(1..=120).contains(duration_seconds)
                    || !valid_identifier(resolution)
                    || max_relative_error_bps.is_some_and(|limit| limit > MAX_TOLERANCE_BPS)
                {
                    return Err(ApiError::BadRequest(
                        "invalid video duration, resolution, or ratio tolerance".into(),
                    ));
                }
                *aspect_ratio = aspect_ratio
                    .parse::<AspectRatio>()
                    .map_err(|e| ApiError::BadRequest(e.into()))?
                    .to_string();
            }
            Workload::Audio {
                duration_seconds,
                format,
            } => {
                if !(1..=120).contains(duration_seconds) || !valid_identifier(format) {
                    return Err(ApiError::BadRequest(
                        "invalid audio duration or format".into(),
                    ));
                }
            }
            Workload::Mesh { format } => {
                if !valid_identifier(format) {
                    return Err(ApiError::BadRequest("invalid mesh format".into()));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = QuoteCandidateReason)]
pub enum Reason {
    Eligible,
    ProviderExcluded,
    ProviderPolicyExcluded,
    ProviderRegionUnmet,
    ProviderRetentionUnmet,
    WorkloadUnsupported,
    AspectRatioUnsupported,
    AboveCostCap,
    PriceStale,
    QueueCoolingDown,
    ProviderCredentialUnavailable,
    InsufficientModelRatings,
    BelowModelQuality,
    BelowModelUsage,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = QuoteCandidate)]
pub struct Candidate {
    pub offer_id: String,
    /// Canonical model ID when comparing models automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[schema(required = false)]
    pub live: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[schema(required = false)]
    pub self_hosted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_size: Option<ImageSize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_handling: Option<DataHandling>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artcraft_reference: Option<super::catalog::ArtCraftReference>,
    pub reason: Reason,
    pub estimated_usd_nanos: Option<i64>,
    pub applied_aspect_ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_tolerance: Option<Setting<u16>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) rate_evidence: Option<super::catalog::VerifiedRate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preference_rank: Option<u8>,
    /// Earliest retry time for an offer cooling down in this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_after: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) performance: Option<OfferPerformance>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuoteProfile {
    #[default]
    Native,
    QueueMedia,
    Chat,
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[schema(as = GatewayQuote)]
pub struct Quote {
    pub id: String,
    /// Last instant before which this scoped quote can admit a new request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub catalog_revision: String,
    pub currency: String,
    pub input: QuoteInput,
    pub selected_offer_id: Option<String>,
    /// Exact offer pin to preserve when submitting this quote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_offer_id: Option<String>,
    /// Offer eligibility profile recorded when the request was admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<QuoteProfile>,
    pub candidates: Vec<Candidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<Resolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(inline)]
    pub advisor: Option<super::adaptive::AdvisorDecision>,
    #[serde(default, skip_serializing_if = "ChatOptions::is_empty")]
    #[schema(required = false)]
    pub chat_options: ChatOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) performance_window: Option<PerformanceWindow>,
}

impl Quote {
    pub(super) fn selected_candidate(&self) -> Option<&Candidate> {
        let offer = self.selected_offer_id.as_deref()?;
        self.candidates.iter().find(|candidate| candidate.offer_id == offer)
    }

    pub(super) fn selected_model(&self) -> &str {
        self.selected_candidate()
            .and_then(|candidate| candidate.model.as_deref())
            .unwrap_or_else(|| public_model_id(&self.input.model))
    }

    pub(super) fn into_preview(mut self) -> Self {
        self.id.replace_range(..4, "preview_");
        self
    }
}

#[derive(Clone, Copy)]
pub(super) enum QuoteTarget<'a> {
    Any,
    Chat,
    Offer(&'a str),
}

impl Catalog {
    pub fn quote(&self, input: QuoteInput) -> ApiResult<Quote> {
        self.quote_matching(input, None, None, None, |_| true)
    }

    #[cfg(test)]
    fn chat_quote(&self, input: QuoteInput) -> ApiResult<Quote> {
        self.quote_matching(input, None, None, None, |offer| offer.adapter.supports_chat())
    }

    pub(super) fn scoped_quote(
        &self,
        input: QuoteInput,
        scope: RequestScope<'_>,
        policies: &Policies,
        availability: &RouteAvailability,
        target: QuoteTarget<'_>,
    ) -> ApiResult<Quote> {
        if input.is_auto() && matches!(target, QuoteTarget::Chat) {
            return Err(ApiError::BadRequest("use /v1/quotes/scoped for automatic model selection".into()));
        }
        let pinned_offer = match target {
            QuoteTarget::Offer(id) => Some(id),
            _ => None,
        };
        self.quote_matching(input, Some((scope, policies)), Some((scope, availability)), pinned_offer, |offer| {
            !matches!(target, QuoteTarget::Chat) || offer.adapter.supports_chat()
        })
    }

    pub(super) fn scoped_media_quote(
        &self,
        input: QuoteInput,
        scope: RequestScope<'_>,
        policies: &Policies,
        availability: &RouteAvailability,
        eligible: impl Fn(&Offer) -> bool,
    ) -> ApiResult<Quote> {
        self.quote_matching(input, Some((scope, policies)), Some((scope, availability)), None, eligible)
    }

    fn quote_matching(
        &self,
        mut input: QuoteInput,
        policies: Option<(RequestScope<'_>, &Policies)>,
        availability: Option<(RequestScope<'_>, &RouteAvailability)>,
        pinned_offer: Option<&str>,
        eligible: impl Fn(&Offer) -> bool,
    ) -> ApiResult<Quote> {
        input.normalize()?;
        if input.is_auto() && (availability.is_none() || pinned_offer.is_some()) {
            return Err(ApiError::BadRequest("automatic model selection requires a scoped quote without an offer pin".into()));
        }
        if pinned_offer.is_some_and(|id| !valid_identifier(id)) {
            return Err(ApiError::BadRequest("invalid pinned_offer_id".into()));
        }
        let policy = policies.and_then(|(scope, policies)| {
            policies.resolution(&input)
                .filter(|_| input.is_auto() || policies.has_revisions() || input.data_requirements.is_some())
                .map(|resolution| (scope, policies, resolution))
        });
        let public_resolution = policy.is_none().then(|| Policies::default().resolution(&input)).flatten();
        let order = policy.as_ref().map(|(_, _, resolution)| resolution).or(public_resolution.as_ref())
            .and_then(|resolution| resolution.provider_preference.as_ref())
            .map_or(&[][..], |preference| preference.value.as_slice());
        let data_requirements = policy.as_ref().map(|(_, _, resolution)| resolution).or(public_resolution.as_ref())
            .and_then(|resolution| resolution.data_requirements.as_ref());
        let now = chrono::Utc::now();
        let mut candidates = self
            .offers
            .iter()
            .filter(|offer| {
                let visible = match policies {
                    Some((scope, rules)) => rules.shares_connection(self, &offer.connection_id(), scope),
                    None => self.visible_to(offer, None),
                };
                visible && input.considers(offer)
                    && pinned_offer.is_none_or(|id| id == offer.id)
                    && eligible(offer)
            })
            .map(|offer| {
                let (tolerance, cap) = match &policy {
                    Some((_, policies, resolved)) => (
                        policies.for_offer(&offer.id, resolved.aspect_ratio_tolerance),
                        resolved.cost_cap.map(|setting| setting.value),
                    ),
                    None => (None, input.max_total_usd_nanos),
                };
                let allowed = policies.is_none_or(|(scope, rules)| rules.allows_offer(self, offer, scope));
                let allowed = allowed && availability.is_none_or(|(_, limits)| !limits.compute_unavailable(&offer.id));
                let mut candidate = compare(offer, &input, tolerance, cap, allowed);
                if candidate.reason == Reason::Eligible {
                    match data_requirements.and_then(|rules| rules.mismatch(offer.data_handling.as_ref())) {
                        Some(DataMismatch::Region) => candidate.reason = Reason::ProviderRegionUnmet,
                        Some(DataMismatch::Retention) => candidate.reason = Reason::ProviderRetentionUnmet,
                        None => {}
                    }
                }
                if input.is_auto() {
                    candidate.model = Some(public_model_id(&offer.model).to_owned());
                }
                if self.expired_offer(&offer.id, now) {
                    if matches!(candidate.reason, Reason::Eligible | Reason::AboveCostCap) {
                        candidate.reason = Reason::PriceStale;
                    }
                    candidate.estimated_usd_nanos = None;
                }
                candidate.rate_evidence = self.rates.get(&offer.id).cloned();
                if candidate.reason == Reason::Eligible {
                    if availability.is_some_and(|(_, limits)| limits.credential_unavailable(&offer.id)) {
                        candidate.reason = Reason::ProviderCredentialUnavailable;
                    } else if let Some(deadline) = availability.and_then(|(_, limits)| limits.available_after(&offer.id, now)) {
                        candidate.reason = Reason::QueueCoolingDown;
                        candidate.available_after = Some(deadline);
                    } else if let Some(reason) = availability
                        .and_then(|(_, limits)| limits.model_exclusion(public_model_id(&offer.model)))
                    {
                        candidate.reason = reason;
                    } else {
                        candidate.preference_rank = order.iter().position(|provider| provider == &offer.provider)
                            .map(|index| index as u8 + 1);
                    }
                }
                candidate
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            let message = if pinned_offer.is_some() {
                "offer not found for this model"
            } else {
                "model not found in demo catalog"
            };
            return Err(ApiError::NotFound(message.into()));
        }
        candidates.sort_by(|a, b| {
            (
                a.reason != Reason::Eligible,
                a.preference_rank.unwrap_or(u8::MAX),
                a.estimated_usd_nanos.unwrap_or(i64::MAX),
                &a.offer_id,
            )
                .cmp(&(
                    b.reason != Reason::Eligible,
                    b.preference_rank.unwrap_or(u8::MAX),
                    b.estimated_usd_nanos.unwrap_or(i64::MAX),
                    &b.offer_id,
                ))
        });
        let selected_offer_id = candidates
            .first()
            .filter(|c| c.reason == Reason::Eligible)
            .map(|c| c.offer_id.clone());
        let identity = match (pinned_offer, &policy, availability) {
            (Some(id), policy, Some((scope, _))) => serde_json::to_vec(&(
                "pinned_offer_v1",
                id,
                &self.revision,
                &input,
                &candidates,
                scope.account,
                scope.project,
                policy.as_ref().map(|(_, _, resolution)| resolution),
            )),
            (_, Some((scope, _, resolution)), _) => serde_json::to_vec(&(
                &self.revision,
                &input,
                &candidates,
                scope.account,
                scope.project,
                resolution,
            )),
            (_, None, Some((scope, _))) if candidates.iter().any(|candidate| matches!(
                candidate.reason, Reason::ProviderCredentialUnavailable | Reason::QueueCoolingDown
            )) => {
                serde_json::to_vec(&(&self.revision, &input, &candidates, scope.account, scope.project))
            }
            (_, None, _) => serde_json::to_vec(&(&self.revision, &input, &candidates)),
        }
        .map_err(|_| ApiError::Internal("could not encode demo quote".into()))?;
        Ok(Quote {
            id: format!("quo_{}", digest(identity)),
            expires_at: None,
            catalog_revision: self.revision.clone(),
            currency: "USD".into(),
            input,
            selected_offer_id,
            pinned_offer_id: pinned_offer.map(str::to_owned),
            profile: None,
            candidates,
            advisor: None,
            routing: policy.map(|(_, _, resolution)| resolution).or(public_resolution),
            chat_options: ChatOptions::default(),
            performance_window: None,
        })
    }
}

fn compare(
    offer: &Offer,
    input: &QuoteInput,
    tolerance: Option<Setting<u16>>,
    cap: Option<i64>,
    allowed: bool,
) -> Candidate {
    let mut result = Candidate {
        offer_id: offer.id.clone(),
        model: None,
        provider: offer.provider.clone(),
        live: false,
        output_size: None,
        self_hosted: offer.provider.as_str() == "self_hosted",
        data_handling: offer.data_handling.clone(),
        artcraft_reference: offer.artcraft_reference.clone(),
        reason: Reason::Eligible,
        estimated_usd_nanos: None,
        applied_aspect_ratio: None,
        offer_tolerance: None,
        rate_evidence: None,
        preference_rank: None,
        available_after: None,
        performance: None,
    };
    if !allowed {
        result.reason = Reason::ProviderPolicyExcluded;
        return result;
    }
    if input
        .provider.as_ref()
        .is_some_and(|provider| provider != &offer.provider)
    {
        result.reason = Reason::ProviderExcluded;
        return result;
    }
    let amount = match (&input.workload, &offer.pricing) {
        (
            Workload::Image {
                count,
                aspect_ratio,
                max_relative_error_bps,
            },
            Pricing::Image {
                usd_nanos_per_image,
                aspect_ratios,
            },
        ) => {
            if !apply_aspect_ratio(
                &mut result,
                aspect_ratio,
                aspect_ratios,
                tolerance,
                *max_relative_error_bps,
            ) {
                return result;
            }
            i64::from(*count) * usd_nanos_per_image
        }
        (
            Workload::Text {
                max_input_tokens,
                max_output_tokens,
            },
            Pricing::Text {
                usd_nanos_per_1000_input_tokens,
                usd_nanos_per_1000_output_tokens,
            },
        ) => {
            let input = i64::from(*max_input_tokens) * usd_nanos_per_1000_input_tokens;
            let output = i64::from(*max_output_tokens) * usd_nanos_per_1000_output_tokens;
            (input + output + 999) / 1000
        }
        (
            Workload::Video {
                duration_seconds,
                aspect_ratio,
                resolution,
                max_relative_error_bps,
            },
            Pricing::Video {
                usd_nanos_per_second,
                durations_seconds,
                aspect_ratios,
                resolution: offered_resolution,
            },
        ) => {
            if !durations_seconds.contains(duration_seconds) || offered_resolution != resolution {
                result.reason = Reason::WorkloadUnsupported;
                return result;
            }
            if !apply_aspect_ratio(
                &mut result,
                aspect_ratio,
                aspect_ratios,
                tolerance,
                *max_relative_error_bps,
            ) {
                return result;
            }
            i64::from(*duration_seconds) * usd_nanos_per_second
        }
        (
            Workload::Audio {
                duration_seconds,
                format,
            },
            Pricing::Audio {
                usd_nanos_per_second,
                durations_seconds,
                format: offered_format,
            },
        ) if durations_seconds.contains(duration_seconds) && offered_format == format => {
            i64::from(*duration_seconds) * usd_nanos_per_second
        }
        (
            Workload::Mesh { format },
            Pricing::Mesh {
                usd_nanos_per_mesh,
                format: offered_format,
            },
        ) if offered_format == format => *usd_nanos_per_mesh,
        _ => {
            result.reason = Reason::WorkloadUnsupported;
            return result;
        }
    };
    result.estimated_usd_nanos = Some(amount);
    if cap.is_some_and(|cap| amount > cap) {
        result.reason = Reason::AboveCostCap;
    }
    result
}

fn apply_aspect_ratio(
    candidate: &mut Candidate,
    requested_ratio: &str,
    offered_ratios: &[String],
    resolved: Option<Setting<u16>>,
    requested_tolerance: Option<u16>,
) -> bool {
    candidate.offer_tolerance = resolved.filter(|setting| {
        matches!(setting.source, Source::OrganizationOffer | Source::ProjectOffer)
    });
    let limit = resolved
        .map(|setting| setting.value)
        .or(requested_tolerance)
        .unwrap_or(0);
    candidate.applied_aspect_ratio = requested_ratio
        .parse::<AspectRatio>()
        .ok()
        .and_then(|ratio| ratio.nearest(offered_ratios, limit))
        .map(|ratio| ratio.to_string());
    if candidate.applied_aspect_ratio.is_none() {
        candidate.reason = Reason::AspectRatioUnsupported;
        return false;
    }
    true
}
