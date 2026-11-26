use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::Path;

use anyhow::{bail, ensure, Context};
use chrono::{DateTime, Utc};
use db_queries::gateway_requests::RequestScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::data_handling::DataHandling;
use super::quote::{AspectRatio, Quote, QuoteInput, Reason};
use super::routing_policy::{Policies, Resolution};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, utoipa::ToSchema)]
#[serde(try_from = "String", into = "String")]
#[schema(value_type = String)]
pub struct Provider(String);

impl Provider {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for Provider {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() <= 64
            && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            Ok(Self(value.to_owned()))
        } else {
            Err("use a lowercase provider ID of up to 64 letters, digits, underscores or hyphens")
        }
    }
}

impl TryFrom<String> for Provider {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Provider> for String {
    fn from(provider: Provider) -> Self {
        provider.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Adapter {
    Mock,
    #[serde(rename = "openai_compatible_chat")]
    OpenAiCompatibleChat,
    ConnectorQueue,
}

impl Adapter {
    pub fn supports_chat(self) -> bool {
        self == Self::OpenAiCompatibleChat
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::OpenAiCompatibleChat => "openai_compatible_chat",
            Self::ConnectorQueue => "connector_queue",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pricing {
    Image {
        usd_nanos_per_image: i64,
        aspect_ratios: Vec<String>,
    },
    Text {
        usd_nanos_per_1000_input_tokens: i64,
        usd_nanos_per_1000_output_tokens: i64,
    },
    Video {
        usd_nanos_per_second: i64,
        durations_seconds: Vec<u16>,
        aspect_ratios: Vec<String>,
        resolution: String,
    },
    Audio {
        usd_nanos_per_second: i64,
        durations_seconds: Vec<u16>,
        format: String,
    },
    Mesh {
        usd_nanos_per_mesh: i64,
        format: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub id: String,
    pub provider: Provider,
    pub model: String,
    pub adapter: Adapter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mock_latency_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_binding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_handling: Option<DataHandling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artcraft_reference: Option<ArtCraftReference>,
    pub pricing: Pricing,
}

/// ArtCraft preset used at import. The gateway operator supplies the routing price.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtCraftReference {
    pub offer_id: String,
    pub revision: String,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_sha256: Option<String>,
}

impl ArtCraftReference {
    fn normalize(&mut self) -> anyhow::Result<()> {
        ensure!(valid_identifier(&self.offer_id)
            && !self.revision.trim().is_empty() && self.revision.len() <= 128
            && !self.revision.chars().any(char::is_control),
            "invalid ArtCraft reference offer ID or revision");
        for hash in std::iter::once(&mut self.sha256).chain(&mut self.public_key_sha256) {
            ensure!(hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid ArtCraft reference SHA-256");
            hash.make_ascii_lowercase();
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CatalogFile {
    pub(super) version: u32,
    pub(super) offers: Vec<Offer>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) connection_access: BTreeMap<String, super::provider_access::CatalogAccess>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) rate_feed_offers: Option<Vec<String>>,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct Catalog {
    pub revision: String,
    #[serde(skip)]
    pub(super) file_revision: String,
    pub offers: Vec<Offer>,
    #[serde(skip)]
    pub(super) connection_access: BTreeMap<String, super::provider_access::CatalogAccess>,
    #[serde(skip)]
    pub(super) rates: BTreeMap<String, VerifiedRate>,
    #[serde(skip)]
    pub(super) rate_feed_offers: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, utoipa::ToSchema)]
pub(super) struct VerifiedRate {
    pub source_url: String,
    pub observed_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ModelCatalog {
    revision: String,
    models: Vec<PublicModel>,
    /// Provider IDs of offers shared with the project and allowed by its routing policies.
    #[serde(skip_serializing_if = "Option::is_none")]
    providers: Option<BTreeSet<Provider>>,
}

#[derive(Serialize, Default, utoipa::ToSchema)]
struct PublicModel {
    id: String,
    image: bool,
    text: bool,
    aspect_ratios: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    video: Vec<VideoCapability>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    audio: Vec<AudioCapability>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mesh_formats: Vec<String>,
}

#[derive(Serialize, PartialEq, utoipa::ToSchema)]
struct VideoCapability {
    durations_seconds: Vec<u16>,
    aspect_ratios: Vec<String>,
    resolution: String,
}

#[derive(Serialize, PartialEq, utoipa::ToSchema)]
struct AudioCapability {
    durations_seconds: Vec<u16>,
    format: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ModelQuote {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<DateTime<Utc>>,
    catalog_revision: String,
    currency: String,
    input: QuoteInput,
    max_total_usd_nanos: Option<i64>,
    candidates: Vec<ModelCandidate>,
    prompt_mode: Option<&'static str>,
    routing: Option<Resolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    advisor: Option<super::adaptive::AdvisorDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    performance_window: Option<super::route_performance::PerformanceWindow>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ModelCandidate {
    selected: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    live: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    self_hosted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_size: Option<super::quote::ImageSize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_handling: Option<DataHandling>,
    reason: Reason,
    estimated_usd_nanos: Option<i64>,
    applied_aspect_ratio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preference_rank: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    available_after: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_valid_until: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    performance: Option<super::route_performance::OfferPerformance>,
}

impl Offer {
    pub(super) fn connection_id(&self) -> String {
        super::provider_access::connection_id(self.provider.as_str(), self.adapter.as_str(), self.local_binding.as_deref())
    }
}

impl Catalog {
    pub(super) fn visible_to(&self, offer: &Offer, account: Option<&ids::AccountId>) -> bool {
        self.connection_access.get(&offer.connection_id())
            .is_none_or(|access| account == Some(&access.account))
    }

    pub(super) fn public_view(&self) -> Self {
        let mut catalog = self.clone();
        catalog.offers.retain(|offer| self.visible_to(offer, None));
        catalog.rates.retain(|id, _| catalog.offers.iter().any(|offer| &offer.id == id));
        catalog
    }

    pub(super) fn model_catalog(&self, scope: Option<(&Policies, RequestScope<'_>)>) -> ModelCatalog {
        let mut models = BTreeMap::new();
        let mut providers = scope.map(|_| BTreeSet::new());
        for offer in &self.offers {
            let visible = match scope {
                Some((policies, scope)) => policies.allows_offer(self, offer, scope),
                None => self.visible_to(offer, None),
            };
            if !visible {
                continue;
            }
            if let Some(providers) = &mut providers {
                providers.insert(offer.provider.clone());
            }
            let id = public_model_id(&offer.model);
            let entry = models.entry(id).or_insert_with(|| PublicModel {
                id: id.to_owned(),
                ..Default::default()
            });
            match &offer.pricing {
                Pricing::Image { aspect_ratios, .. } => {
                    entry.image = true;
                    entry.aspect_ratios.extend(aspect_ratios.iter().cloned());
                }
                Pricing::Text { .. } => entry.text = true,
                Pricing::Video {
                    durations_seconds,
                    aspect_ratios,
                    resolution,
                    ..
                } => {
                    let capability = VideoCapability {
                        durations_seconds: durations_seconds.clone(),
                        aspect_ratios: aspect_ratios.clone(),
                        resolution: resolution.clone(),
                    };
                    if !entry.video.contains(&capability) {
                        entry.video.push(capability);
                    }
                }
                Pricing::Audio {
                    durations_seconds,
                    format,
                    ..
                } => {
                    let capability = AudioCapability {
                        durations_seconds: durations_seconds.clone(),
                        format: format.clone(),
                    };
                    if !entry.audio.contains(&capability) {
                        entry.audio.push(capability);
                    }
                }
                Pricing::Mesh { format, .. } => entry.mesh_formats.push(format.clone()),
            }
        }
        let models = models
            .into_values()
            .map(|mut model| {
                model.aspect_ratios.sort_unstable();
                model.aspect_ratios.dedup();
                model.mesh_formats.sort_unstable();
                model.mesh_formats.dedup();
                model
            })
            .collect();
        ModelCatalog {
            revision: self.revision.clone(),
            models,
            providers,
        }
    }

    pub(super) fn model_quote(&self, quote: Quote) -> ModelQuote {
        let prompt_mode = quote.selected_offer_id.as_deref().and_then(|id| {
            self.offers
                .iter()
                .find(|offer| offer.id == id)
                .map(|offer| match offer.adapter {
                    Adapter::Mock => "fixture",
                    Adapter::OpenAiCompatibleChat => "chat",
                    Adapter::ConnectorQueue => match &offer.pricing {
                        Pricing::Image { .. } => "image",
                        Pricing::Video { .. } => "video",
                        Pricing::Audio { .. } => "audio",
                        Pricing::Mesh { .. } => "mesh",
                        Pricing::Text { .. } => unreachable!("validated connector media offer"),
                    },
                })
        });
        ModelQuote {
            id: quote.id,
            expires_at: quote.expires_at,
            catalog_revision: quote.catalog_revision,
            currency: quote.currency,
            max_total_usd_nanos: quote
                .routing
                .as_ref()
                .and_then(|routing| routing.cost_cap.map(|cap| cap.value))
                .or(quote.input.max_total_usd_nanos),
            input: quote.input,
            candidates: quote
                .candidates
                .into_iter()
                .map(|candidate| ModelCandidate {
                    selected: quote.selected_offer_id.as_deref()
                        == Some(candidate.offer_id.as_str()),
                    live: candidate.live,
                    self_hosted: candidate.self_hosted,
                    output_size: candidate.output_size,
                    data_handling: candidate.data_handling,
                    reason: candidate.reason,
                    estimated_usd_nanos: candidate.estimated_usd_nanos,
                    applied_aspect_ratio: candidate.applied_aspect_ratio,
                    preference_rank: candidate.preference_rank,
                    available_after: candidate.available_after,
                    rate_valid_until: candidate.rate_evidence.map(|rate| rate.valid_until),
                    performance: candidate.performance,
                })
                .collect(),
            prompt_mode,
            routing: quote.routing,
            advisor: quote.advisor,
            performance_window: quote.performance_window,
        }
    }

    pub(super) fn expired_offer(&self, id: &str, now: DateTime<Utc>) -> bool {
        self.rates
            .get(id)
            .is_some_and(|rate| rate.valid_until <= now)
    }
}

const MODEL_ALIASES: [(&str, &str); 2] = [
    ("studio_fal_wire_demo", "studio_flux_image"),
    ("studio_chat_wire_demo", "studio_text_chat"),
];

pub(super) fn public_model_id(model: &str) -> &str {
    MODEL_ALIASES.iter()
        .find_map(|&(stored, public)| (model == stored).then_some(public))
        .unwrap_or(model)
}

pub(super) fn alternate_model_id(model: &str) -> Option<&'static str> {
    MODEL_ALIASES.iter().find_map(|&(stored, public)| match model {
        value if value == stored => Some(public),
        value if value == public => Some(stored),
        _ => None,
    })
}

pub(super) const MAX_CONFIG_BYTES: u64 = 65_536;
pub(super) const MAX_OFFERS: usize = 64;
pub(super) const MAX_RATE: i64 = 1_000_000_000;

pub fn digest(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn valid_identifier(value: &str) -> bool {
    let Some(first) = value.as_bytes().first() else {
        return false;
    };
    value.len() <= 64
        && first.is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

impl Catalog {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .context("open TokenCraft demo catalog")?
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            bail!("demo catalog exceeds 64 KiB");
        }
        Self::parse(&bytes)
    }

    pub(crate) fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            bail!("demo catalog exceeds 64 KiB");
        }
        let mut config: CatalogFile = serde_json::from_slice(bytes).map_err(|e| {
            anyhow::anyhow!(
                "invalid demo catalog at line {}, column {}",
                e.line(),
                e.column()
            )
        })?;
        if config.version != 1 || config.offers.is_empty() || config.offers.len() > MAX_OFFERS {
            bail!("demo catalog requires version 1 and one to 64 offers");
        }
        let mut seen = HashSet::new();
        config.offers.sort_by(|a, b| a.id.cmp(&b.id));
        for offer in &mut config.offers {
            if let Some(reference) = &mut offer.artcraft_reference {
                reference.normalize()?;
            }
            if let Some(data) = &mut offer.data_handling {
                data.normalize().map_err(anyhow::Error::msg)?;
            }
            if !valid_identifier(&offer.id)
                || !valid_identifier(&offer.model)
                || matches!(offer.model.as_str(), "auto" | "advise")
                || !seen.insert(offer.id.clone())
            {
                bail!("invalid or repeated offer ID or model in demo catalog");
            }
            let uses_binding = matches!(offer.adapter, Adapter::OpenAiCompatibleChat | Adapter::ConnectorQueue);
            if offer.local_binding.is_some() != uses_binding {
                bail!("connector and chat offers require one local_binding");
            }
            match offer.adapter {
                Adapter::Mock
                    if offer
                        .mock_latency_ms
                        .is_some_and(|ms| (100..=30_000).contains(&ms))
                        && offer.upstream_model.is_none() => {}
                Adapter::OpenAiCompatibleChat
                    if offer.mock_latency_ms.is_none()
                        && matches!(offer.pricing, Pricing::Text { .. })
                        && offer.local_binding.as_deref().is_some_and(valid_identifier)
                        && offer.upstream_model.as_deref().is_some_and(valid_local_model) => {}
                Adapter::ConnectorQueue
                    if offer.mock_latency_ms.is_none()
                        && !matches!(offer.pricing, Pricing::Text { .. })
                        && offer.local_binding.as_deref().is_some_and(valid_identifier)
                        && offer.upstream_model.as_deref().is_some_and(valid_local_model) => {}
                _ => bail!("offer settings do not match the selected adapter"),
            }
            match &mut offer.pricing {
                Pricing::Image {
                    usd_nanos_per_image,
                    aspect_ratios,
                } => {
                    if !(0..=MAX_RATE).contains(usd_nanos_per_image)
                        || !(1..=24).contains(&aspect_ratios.len())
                    {
                        bail!("invalid image price or ratio count");
                    }
                    for ratio in aspect_ratios.iter_mut() {
                        *ratio = ratio
                            .parse::<AspectRatio>()
                            .map_err(anyhow::Error::msg)?
                            .to_string();
                    }
                    aspect_ratios.sort();
                    aspect_ratios.dedup();
                }
                Pricing::Text {
                    usd_nanos_per_1000_input_tokens,
                    usd_nanos_per_1000_output_tokens,
                } => {
                    if !(0..=MAX_RATE).contains(usd_nanos_per_1000_input_tokens)
                        || !(0..=MAX_RATE).contains(usd_nanos_per_1000_output_tokens)
                    {
                        bail!("invalid token price");
                    }
                }
                Pricing::Video {
                    usd_nanos_per_second,
                    durations_seconds,
                    aspect_ratios,
                    resolution,
                } => {
                    if !(0..=MAX_RATE).contains(usd_nanos_per_second)
                        || !(1..=24).contains(&aspect_ratios.len())
                        || !(1..=24).contains(&durations_seconds.len())
                        || durations_seconds
                            .iter()
                            .any(|value| !(1..=120).contains(value))
                        || !valid_identifier(resolution)
                    {
                        bail!("invalid video price or capability");
                    }
                    for ratio in aspect_ratios.iter_mut() {
                        *ratio = ratio
                            .parse::<AspectRatio>()
                            .map_err(anyhow::Error::msg)?
                            .to_string();
                    }
                    aspect_ratios.sort();
                    aspect_ratios.dedup();
                    durations_seconds.sort();
                    durations_seconds.dedup();
                    if offer.adapter == Adapter::ConnectorQueue
                        && aspect_ratios.iter().any(|ratio| !super::providers::connector::supports_video(ratio, resolution))
                    {
                        bail!("connector video requires bounded dimensions and a supported resolution");
                    }
                }
                Pricing::Audio {
                    usd_nanos_per_second,
                    durations_seconds,
                    format,
                } => {
                    if !(0..=MAX_RATE).contains(usd_nanos_per_second)
                        || !(1..=24).contains(&durations_seconds.len())
                        || durations_seconds
                            .iter()
                            .any(|value| !(1..=120).contains(value))
                        || !valid_identifier(format)
                    {
                        bail!("invalid audio price or capability");
                    }
                    durations_seconds.sort();
                    durations_seconds.dedup();
                    if offer.adapter == Adapter::ConnectorQueue && format != "wav" {
                        bail!("connector audio requires WAV output");
                    }
                }
                Pricing::Mesh {
                    usd_nanos_per_mesh,
                    format,
                } => {
                    if !(0..=MAX_RATE).contains(usd_nanos_per_mesh) || !valid_identifier(format) {
                        bail!("invalid mesh price or format");
                    }
                    if offer.adapter == Adapter::ConnectorQueue && format != "glb" {
                        bail!("connector mesh requires GLB output");
                    }
                }
            }
        }
        ensure!(
            config.connection_access.iter().all(|(id, access)| {
                access.projects.as_ref().is_none_or(|projects| !projects.is_empty())
                    && config.offers.iter().any(|offer| offer.connection_id() == *id)
            }),
            "catalog connection access must name a configured connection and a valid scope"
        );
        if let Some(ids) = &mut config.rate_feed_offers {
            ids.sort_unstable();
            ensure!(
                !ids.is_empty() && ids.iter().all(|id| seen.contains(id))
                    && ids.windows(2).all(|pair| pair[0] != pair[1]),
                "rate_feed_offers must name distinct configured offers"
            );
        }
        let revision = format!("demo_{}", digest(serde_json::to_vec(&config)?));
        Ok(Self {
            file_revision: revision.clone(),
            revision,
            offers: config.offers,
            connection_access: config.connection_access,
            rates: BTreeMap::new(),
            rate_feed_offers: config.rate_feed_offers,
        })
    }

    pub(super) fn into_file(self) -> CatalogFile {
        CatalogFile { version: 1, offers: self.offers, connection_access: self.connection_access, rate_feed_offers: self.rate_feed_offers }
    }
}

fn valid_local_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 160
        && model.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
}
