use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The catalog's provider identifier, including operator-defined connectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provider {
    Kinovi,
    Fal,
    OpenRouter,
    SelfHosted,
    Custom(String),
}

impl Provider {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Kinovi => "kinovi",
            Self::Fal => "fal",
            Self::OpenRouter => "openrouter",
            Self::SelfHosted => "self_hosted",
            Self::Custom(name) => name,
        }
    }
}

impl Serialize for Provider {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Provider {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Ok(match name.as_str() {
            "kinovi" => Self::Kinovi,
            "fal" => Self::Fal,
            "openrouter" => Self::OpenRouter,
            "self_hosted" => Self::SelfHosted,
            _ => Self::Custom(name),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pricing {
    Image {
        usd_nanos_per_image: u64,
        aspect_ratios: Vec<String>,
    },
    Text {
        usd_nanos_per_1000_input_tokens: u64,
        usd_nanos_per_1000_output_tokens: u64,
    },
    Video {
        usd_nanos_per_second: u64,
        durations_seconds: Vec<u16>,
        aspect_ratios: Vec<String>,
        resolution: String,
    },
    Audio {
        usd_nanos_per_second: u64,
        durations_seconds: Vec<u16>,
        format: String,
    },
    Mesh {
        usd_nanos_per_mesh: u64,
        format: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DataHandling {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_regions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_seconds: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DataRequirements {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_regions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retention_seconds: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Offer {
    pub id: String,
    pub model: String,
    pub provider: Provider,
    pub adapter: String,
    pub pricing: Pricing,
    pub upstream_model: Option<String>,
    pub data_handling: Option<DataHandling>,
    pub artcraft_reference: Option<ArtCraftReference>,
}

/// The public preset used to create a local offer.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ArtCraftReference {
    pub offer_id: String,
    pub revision: String,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key_sha256: Option<String>,
}

/// Source and validity window of the reviewed rate applied to a quote.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RateEvidence {
    pub source_url: String,
    pub observed_at: String,
    pub valid_until: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Catalog {
    pub revision: String,
    pub offers: Vec<Offer>,
}

/// Model IDs and their configured workload capabilities.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelCatalog {
    pub revision: String,
    pub models: Vec<CatalogModel>,
    /// Provider IDs available to the project. Public catalogs and older gateways omit this list.
    pub providers: Option<Vec<Provider>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    pub image: bool,
    pub text: bool,
    pub aspect_ratios: Vec<String>,
    #[serde(default)]
    pub video: Vec<VideoCapability>,
    #[serde(default)]
    pub audio: Vec<AudioCapability>,
    #[serde(default)]
    pub mesh_formats: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct VideoCapability {
    pub durations_seconds: Vec<u16>,
    pub aspect_ratios: Vec<String>,
    pub resolution: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AudioCapability {
    pub durations_seconds: Vec<u16>,
    pub format: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QuoteInput {
    pub model: String,
    pub workload: Workload,
    pub provider: Option<Provider>,
    pub max_total_usd_nanos: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct QuoteOptions<'a> {
    #[serde(skip)]
    pub client_ref: Option<&'a str>,
    #[serde(skip)]
    pub pinned_offer_id: Option<&'a str>,
    /// Pass an empty slice to select by the lowest estimate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_providers: Option<&'a [Provider]>,
    /// Minimum community rating for automatic model selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_quality: Option<u8>,
    /// Minimum number of organizations using an automatically selected model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_usage_accounts: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateReason {
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    Request,
    ModelDefault,
    OrganizationDefault,
    ProjectDefault,
    OrganizationLimit,
    ProjectLimit,
    OrganizationOffer,
    ProjectOffer,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PolicySetting<T> {
    pub value: T,
    pub source: PolicySource,
}

impl<T> PolicySetting<T> {
    pub(crate) fn request_value(&self) -> Option<&T> {
        (self.source == PolicySource::Request).then_some(&self.value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteRanking {
    Price,
    RecentReliability,
    CostPerCompletion,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoutingPolicy {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_requirements: Option<DataRequirements>,
    pub organization_revision: u32,
    pub project_revision: u32,
    pub aspect_ratio_tolerance: Option<PolicySetting<u16>>,
    pub cost_cap: Option<PolicySetting<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_limits: Option<ProviderLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_preference: Option<PolicySetting<Vec<Provider>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_ranking: Option<PolicySetting<RouteRanking>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_quality: Option<PolicySetting<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_usage_accounts: Option<PolicySetting<u32>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderLimits {
    pub organization: bool,
    pub project: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CostEvidence {
    pub settled: u64,
    pub settled_usd_nanos: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OfferPerformance {
    pub samples: u64,
    pub succeeded: u64,
    pub unconfirmed: u64,
    pub median_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostEvidence>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct PerformanceWindow {
    pub window_days: u32,
    pub minimum_samples: u64,
    pub maximum_samples: u64,
    pub applied: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Candidate {
    pub offer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub self_hosted: bool,
    pub reason: CandidateReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_handling: Option<DataHandling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artcraft_reference: Option<ArtCraftReference>,
    pub estimated_usd_nanos: Option<u64>,
    pub applied_aspect_ratio: Option<String>,
    pub offer_tolerance: Option<PolicySetting<u16>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_evidence: Option<RateEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preference_rank: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<OfferPerformance>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteProfile {
    Native,
    QueueMedia,
    Chat,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Quote {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub catalog_revision: String,
    pub input: QuoteInput,
    pub currency: String,
    pub selected_offer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_offer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<QuoteProfile>,
    pub candidates: Vec<Candidate>,
    pub chat_options: Option<ChatOptions>,
    pub routing: Option<RoutingPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance_window: Option<PerformanceWindow>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptState {
    RateLimited,
    Queued,
    Dispatching,
    Acknowledged,
    Succeeded,
    Failed,
    Canceled,
    OutcomeUnconfirmed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderAttempt {
    pub offer_id: String,
    pub provider: Option<Provider>,
    pub adapter: Option<String>,
    pub state: ProviderAttemptState,
    pub step_id: String,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    pub correlation_id: Option<String>,
    pub acknowledged_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DownstreamProvider {
    Reported { provider: String },
    Pending,
    NotDisclosed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderPath {
    pub admitted_offer_id: Option<String>,
    pub attempts: Vec<ProviderAttempt>,
    pub downstream: Option<DownstreamProvider>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RoutePreviewOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_usd_nanos: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_providers: Option<Vec<Provider>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteUnavailable {
    PinnedOfferUnavailable,
    WorkloadUnavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoutePreview {
    pub observed_at: String,
    pub recorded: Quote,
    pub current: Option<Quote>,
    pub unavailable_reason: Option<RouteUnavailable>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Serialize)]
pub struct Message<'a> {
    pub role: Role,
    pub content: &'a str,
}

pub struct SubmitOptions<'a> {
    pub idempotency_key: &'a str,
    pub client_ref: Option<&'a str>,
    pub webhook_subscription_id: Option<&'a str>,
    pub messages: Option<&'a [Message<'a>]>,
    pub image_prompt: Option<&'a str>,
    pub chat_options: Option<&'a ChatOptions>,
}

impl<'a> SubmitOptions<'a> {
    pub fn new(idempotency_key: &'a str) -> Self {
        Self {
            idempotency_key,
            client_ref: None,
            webhook_subscription_id: None,
            messages: None,
            image_prompt: None,
            chat_options: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub id: String,
    pub project_id: String,
    pub status_url: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Queued,
    Running,
    Succeeded,
    Canceled,
    Failed,
    AwaitingReconciliation,
}

impl RequestStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Canceled | Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub sequence: u32,
    pub kind: EventKind,
    pub at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RequestEvents {
    pub id: String,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FixtureResult {
    Image { count: u16, aspect_ratio: String },
    Text { text: String },
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderCharge {
    pub unit: String,
    pub quantity: String,
    pub observed_at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Upstream {
    pub adapter: String,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    pub reported_provider: Option<String>,
    pub finish_reason: Option<String>,
    pub error_code: Option<String>,
    pub result_expires_at: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub reported_usd_nanos: Option<String>,
    pub provider_charge: Option<ProviderCharge>,
    pub accepted_at: Option<String>,
    pub queue_state: Option<String>,
    pub callback_received_at: Option<String>,
    pub callback_recovered_from: Option<String>,
    pub callback_reconciled_at: Option<String>,
    pub credential_version: Option<i32>,
    pub credential_scope: Option<String>,
    pub stream_started_at: Option<String>,
    pub stream_detached_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CostState {
    Estimated,
    MockReported,
    ProviderReported,
    SupplierSettled,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TraceCorrelation {
    pub trace_id: String,
    pub admission_span_id: String,
    pub caller_trace_id: Option<String>,
    pub caller_span_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RequestTrace {
    pub correlation: Option<TraceCorrelation>,
    pub id: String,
    pub project_id: String,
    pub client_ref: Option<String>,
    pub app_ref: Option<String>,
    pub provider: Option<Provider>,
    pub cost_state: Option<CostState>,
    pub webhook_subscription_id: Option<String>,
    pub provider_handle: Option<String>,
    pub status: RequestStatus,
    pub quote: Quote,
    pub mock_reported_usd_nanos: Option<u64>,
    pub mock_result: Option<FixtureResult>,
    pub upstream: Option<Upstream>,
    pub supplier_settlement: Option<SupplierSettlement>,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SupplierSettlement {
    pub revision: i32,
    pub amount_usd_nanos: String,
    pub recorded_at: String,
}

#[derive(Clone, Deserialize)]
pub struct ProviderResult {
    pub result: ChatResult,
    pub expires_at: String,
}

#[derive(Clone, Deserialize)]
pub struct ImageProviderResult {
    pub result: ImageOutputs,
    pub expires_at: String,
}

#[derive(Clone, Deserialize)]
pub struct ImageOutputs {
    pub images: Vec<ImageOutput>,
}

#[derive(Clone, Deserialize)]
pub struct ImageOutput {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub content_type: String,
    pub artifact: Option<ImageArtifactRef>,
}

#[derive(Clone, Deserialize)]
pub struct ArtifactRef {
    pub slot: i16,
    pub byte_length: u32,
    pub sha256: String,
}

pub type ImageArtifactRef = ArtifactRef;

#[derive(Clone, Deserialize)]
pub struct VideoProviderResult {
    pub result: VideoOutput,
    pub expires_at: String,
}

#[derive(Clone, Deserialize)]
pub struct VideoOutput {
    pub video: VideoFile,
}

#[derive(Clone, Deserialize)]
pub struct AudioProviderResult {
    pub result: AudioOutput,
    pub expires_at: String,
}

#[derive(Clone, Deserialize)]
pub struct AudioOutput {
    pub audio: AudioFile,
}

#[derive(Clone, Deserialize)]
pub struct AudioFile {
    pub url: String,
    pub content_type: String,
    pub duration_ms: u32,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub artifact: ArtifactRef,
}

#[derive(Clone, Deserialize)]
pub struct MeshProviderResult {
    pub result: MeshOutput,
    pub expires_at: String,
}

#[derive(Clone, Deserialize)]
pub struct MeshOutput {
    pub mesh: MeshFile,
}

#[derive(Clone, Deserialize)]
pub struct MeshFile {
    pub url: String,
    pub content_type: String,
    pub mesh_count: u32,
    pub primitive_count: u32,
    pub artifact: ArtifactRef,
}

#[derive(Clone, Deserialize)]
pub struct VideoFile {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    pub content_type: String,
    pub artifact: ArtifactRef,
}

#[derive(Clone, Deserialize)]
pub struct ChatResult {
    pub text: String,
    pub finish_reason: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct WaitOptions {
    pub timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(750),
        }
    }
}

#[derive(Default, Serialize)]
pub struct ListOptions<'a> {
    pub client_ref: Option<&'a str>,
    pub after: Option<&'a str>,
}

#[derive(Default, Serialize)]
pub struct RequestFilters<'a> {
    pub client_ref: Option<&'a str>,
    /// Only requests without a client reference.
    pub client_unattributed: Option<bool>,
    /// Exact gateway or caller trace ID.
    pub trace_id: Option<&'a str>,
    pub app_ref: Option<&'a str>,
    /// Only requests without an application reference.
    pub app_unattributed: Option<bool>,
    pub model: Option<&'a str>,
    /// Match the stored model ID exactly, including when it has a catalog alias.
    pub exact_model: Option<bool>,
    pub provider: Option<Provider>,
    pub status: Option<RequestStatus>,
    pub cost_state: Option<CostState>,
    /// Inclusive UTC date in YYYY-MM-DD format.
    pub from: Option<&'a str>,
    /// Exclusive UTC date in YYYY-MM-DD format.
    pub to: Option<&'a str>,
    pub after: Option<&'a str>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RequestPage {
    pub requests: Vec<RequestTrace>,
    pub next_after: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageGroup {
    Provider,
    Model,
    Client,
    App,
    Day,
}

#[derive(Default, Serialize)]
pub struct UsageOptions<'a> {
    pub client_ref: Option<&'a str>,
    pub after: Option<&'a str>,
    pub from: Option<&'a str>,
    pub to: Option<&'a str>,
    pub group_by: Option<UsageGroup>,
    pub model: Option<&'a str>,
    pub provider: Option<Provider>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageMetrics {
    pub requests: u64,
    pub succeeded: u64,
    pub canceled: u64,
    pub failed: u64,
    pub awaiting_reconciliation: u64,
    pub in_progress: u64,
    pub provider_requests: u64,
    pub completed_outputs: u64,
    pub estimated_usd_nanos: String,
    pub in_progress_estimated_usd_nanos: String,
    pub mock_reported_usd_nanos: String,
    pub provider_reported_usd_nanos: Option<String>,
    pub provider_reports_missing_cost: u64,
    #[serde(default)]
    pub provider_charges: Vec<ProviderChargeTotal>,
    pub supplier_settled_usd_nanos: Option<String>,
    #[serde(default)]
    pub supplier_settled_requests: u64,
    pub provider_input_tokens: Option<u64>,
    pub provider_output_tokens: Option<u64>,
    pub avg_success_latency_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderChargeTotal {
    pub provider: Provider,
    pub unit: String,
    pub quantity: String,
    pub observations: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageWindow {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageBreakdown {
    pub key: Option<String>,
    #[serde(flatten)]
    pub metrics: UsageMetrics,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UsageReport {
    pub project_id: String,
    pub window: UsageWindow,
    pub as_of: String,
    pub source: String,
    pub currency: String,
    pub group_by: UsageGroup,
    pub client_ref: Option<String>,
    pub app_ref: Option<String>,
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub totals: UsageMetrics,
    pub groups: Vec<UsageBreakdown>,
    pub next_after: Option<String>,
}
