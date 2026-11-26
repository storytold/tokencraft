import type { ComputeTargetAttempt } from "./trace.js";

/** Provider ID declared in the gateway catalog. */
export type Provider = string;

export type Pricing =
  | { kind: "image"; usd_nanos_per_image: number; aspect_ratios: string[] }
  | { kind: "video"; usd_nanos_per_second: number; durations_seconds: number[]; aspect_ratios: string[]; resolution: string }
  | { kind: "audio"; usd_nanos_per_second: number; durations_seconds: number[]; format: string }
  | { kind: "mesh"; usd_nanos_per_mesh: number; format: string }
  | { kind: "text"; usd_nanos_per_1000_input_tokens: number; usd_nanos_per_1000_output_tokens: number };

export interface DataHandling {
  processing_regions?: string[];
  retention_seconds?: number;
}

export interface DataRequirements {
  allowed_regions?: string[];
  max_retention_seconds?: number;
}

export interface Offer {
  id: string;
  model: string;
  provider: Provider;
  adapter: "mock" | "openrouter_chat" | "fal_queue" | "kinovi_queue" | "openai_compatible_chat" | "connector_queue";
  upstream_model?: string;
  mock_latency_ms?: number;
  pricing: Pricing;
  data_handling?: DataHandling;
  artcraft_reference?: ArtCraftReference;
}

/** The public preset used to create a local offer. */
export interface ArtCraftReference {
  offer_id: string;
  revision: string;
  sha256: string;
  public_key_sha256?: string;
}

/** Source and validity window of the reviewed rate applied to a quote. */
export interface RateEvidence {
  source_url: string;
  observed_at: string;
  valid_until: string;
}

export interface Catalog {
  revision: string;
  offers: Offer[];
}

export interface ModelCatalog {
  revision: string;
  models: CatalogModel[];
  /** Provider IDs available to the project. Public catalogs and older gateways omit this list. */
  providers?: Provider[];
}

export interface CatalogModel {
  id: string;
  image: boolean;
  text: boolean;
  aspect_ratios: string[];
  video?: { durations_seconds: number[]; aspect_ratios: string[]; resolution: string }[];
  audio?: { durations_seconds: number[]; format: string }[];
  mesh_formats?: string[];
}

export interface ModelListOptions extends CallOptions {
  scope?: "public" | "project";
}

export type Workload =
  | { kind: "image"; count: number; aspect_ratio: string; max_relative_error_bps?: number | null }
  | { kind: "video"; duration_seconds: number; aspect_ratio: string; resolution: string; max_relative_error_bps?: number | null }
  | { kind: "audio"; duration_seconds: number; format: string }
  | { kind: "mesh"; format: string }
  | { kind: "text"; max_input_tokens: number; max_output_tokens: number };

export interface QuoteInput {
  model: string;
  workload: Workload;
  provider?: Provider | null;
  preferred_providers?: Provider[] | null;
  max_total_usd_nanos?: number | null;
  data_requirements?: DataRequirements;
}

export type PolicySetting<T = number> = {
  value: T;
  source: "request" | "model_default" | "organization_default" | "project_default"
    | "organization_limit" | "project_limit" | "organization_offer" | "project_offer";
};

export type RouteRanking = "price" | "recent_reliability" | "cost_per_completion";

export interface CostEvidence {
  settled: number;
  settled_usd_nanos: string | null;
}

export interface OfferPerformance {
  samples: number;
  succeeded: number;
  unconfirmed: number;
  median_ms: number | null;
  cost?: CostEvidence;
}

export interface PerformanceWindow {
  window_days: number;
  minimum_samples: number;
  maximum_samples: number;
  applied: boolean;
}

export interface Candidate {
  offer_id: string;
  provider: Provider;
  self_hosted?: boolean;
  output_size?: { width: number; height: number };
  data_handling?: DataHandling;
  artcraft_reference?: ArtCraftReference;
  reason: "eligible" | "provider_excluded" | "provider_policy_excluded"
    | "provider_region_unmet" | "provider_retention_unmet" | "workload_unsupported"
    | "aspect_ratio_unsupported" | "above_cost_cap" | "price_stale" | "queue_cooling_down" | "provider_credential_unavailable";
  estimated_usd_nanos: number | null;
  applied_aspect_ratio: string | null;
  offer_tolerance?: PolicySetting;
  rate_evidence?: RateEvidence;
  preference_rank?: number;
  available_after?: string;
  performance?: OfferPerformance;
}

export interface ChatOptions {
  temperature?: number;
  top_p?: number;
}

export interface Quote {
  id: string;
  expires_at?: string;
  catalog_revision: string;
  input: QuoteInput;
  currency: "USD";
  selected_offer_id: string | null;
  pinned_offer_id?: string;
  profile?: "native" | "queue_media" | "chat";
  candidates: Candidate[];
  chat_options?: ChatOptions;
  performance_window?: PerformanceWindow;
  routing?: {
    organization_revision: number;
    data_requirements?: DataRequirements;
    project_revision: number;
    cost_cap: PolicySetting | null;
    aspect_ratio_tolerance: PolicySetting | null;
    provider_limits?: { organization: boolean; project: boolean };
    provider_preference?: PolicySetting<Provider[]>;
    route_ranking?: PolicySetting<RouteRanking>;
  };
}

export interface CallOptions {
  signal?: AbortSignal;
}

export interface ClientOptions {
  baseUrl: string | URL;
  apiKey?: string;
  timeoutMs?: number;
  /** Total attempts for a native submission, including the first. Defaults to 1. */
  submitMaxAttempts?: number;
  fetch?: typeof fetch;
}

export interface QuoteOptions extends CallOptions {
  scope?: "public" | "project";
  clientRef?: string;
  offerId?: string;
}

export type ProviderAttemptState =
  | "rate_limited" | "queued" | "dispatching" | "acknowledged"
  | "succeeded" | "failed" | "canceled" | "outcome_unconfirmed";

export type ProviderAttempt = {
  offer_id: string;
  provider: Provider | null;
  adapter?: string;
  state: ProviderAttemptState;
  step_id: string;
  requested_model?: string;
  reported_model?: string;
  correlation_id?: string;
  acknowledged_at?: string;
  compute_attempts?: ComputeTargetAttempt[];
};

export type ProviderPath = {
  admitted_offer_id: string | null;
  attempts: ProviderAttempt[];
  downstream?: { status: "reported"; provider: string } | { status: "pending" | "not_disclosed" };
};

export interface RoutePreviewOptions extends CallOptions {
  maxTotalUsdNanos?: number;
  preferredProviders?: Provider[];
}

export interface RoutePreview {
  observed_at: string;
  recorded: Quote;
  current: Quote | null;
  unavailable_reason?: "pinned_offer_unavailable" | "workload_unavailable";
}

export interface SubmitOptions extends CallOptions {
  idempotencyKey: string;
  /** W3C traceparent from the caller's active span. */
  traceparent?: string;
  clientRef?: string;
  appRef?: string;
  webhookSubscriptionId?: string;
  messages?: { role: "system" | "user" | "assistant"; content: string }[];
  imagePrompt?: string;
  videoPrompt?: string;
  audioPrompt?: string;
  meshPrompt?: string;
  chatOptions?: ChatOptions;
}

export interface Receipt {
  id: string;
  project_id: string;
  status_url: string;
}

export type RequestStatus = "queued" | "running" | "succeeded" | "canceled" | "failed" | "awaiting_reconciliation";
export type RequestEventKind = "accepted" | "mock_provider_submitted" | "mock_provider_completed" | "canceled"
  | "provider_submit_started" | "provider_completed" | "provider_failed" | "provider_outcome_unconfirmed"
  | "provider_reconciled";

export interface RequestEvent {
  sequence: number;
  kind: RequestEventKind;
  at: string;
}

export interface RequestEvents {
  id: string;
  events: RequestEvent[];
}

export interface TraceCorrelation {
  trace_id: string;
  admission_span_id: string;
  caller_trace_id?: string;
  caller_span_id?: string;
}

export interface RequestTrace extends RequestEvents {
  correlation?: TraceCorrelation;
  project_id: string;
  client_ref: string | null;
  app_ref?: string;
  provider?: Provider;
  cost_state?: CostState;
  webhook_subscription_id?: string;
  provider_handle: string | null;
  status: RequestStatus;
  quote: Quote;
  mock_reported_usd_nanos: number | null;
  mock_result:
    | { kind: "image"; count: number; aspect_ratio: string }
    | { kind: "video"; duration_seconds: number; aspect_ratio: string; resolution: string }
    | { kind: "audio"; duration_seconds: number; format: string }
    | { kind: "mesh"; format: string }
    | { kind: "text"; text: string }
    | null;
  upstream?: {
    adapter: string;
    requested_model: string | null;
    reported_model: string | null;
    reported_provider: string | null;
    finish_reason: string | null;
    error_code: string | null;
    result_expires_at: string | null;
    input_tokens: number | null;
    output_tokens: number | null;
    reported_usd_nanos: string | null;
    provider_charge?: { unit: string; quantity: string; observed_at: string };
    accepted_at?: string;
    queue_state?: "in_queue" | "in_progress" | "completed";
    callback_received_at?: string;
    callback_recovered_from?: string;
    callback_reconciled_at?: string;
    credential_version?: number;
    credential_scope?: "organization" | "project";
    stream_started_at?: string;
    stream_detached_at?: string;
  };
  supplier_settlement?: { revision: number; amount_usd_nanos: string; recorded_at: string };
}

export interface ProviderResult {
  result: { text: string; finish_reason: string | null };
  expires_at: string;
}

export interface ImageProviderResult {
  result: { images: ImageOutput[] };
  expires_at: string;
}

export interface VideoProviderResult {
  result: { video: VideoOutput };
  expires_at: string;
}

export interface VideoOutput extends ImageOutput {
  duration_ms: number;
  artifact: NonNullable<ImageOutput["artifact"]>;
}

export interface AudioProviderResult {
  result: { audio: AudioOutput };
  expires_at: string;
}

export interface AudioOutput {
  url: string;
  content_type: string;
  duration_ms: number;
  sample_rate_hz: number;
  channels: number;
  artifact: NonNullable<ImageOutput["artifact"]>;
}

export interface MeshProviderResult {
  result: { mesh: MeshOutput };
  expires_at: string;
}

export interface MeshOutput {
  url: string;
  content_type: string;
  mesh_count: number;
  primitive_count: number;
  artifact: NonNullable<ImageOutput["artifact"]>;
}

export interface ArtifactRef {
  slot: number;
  byte_length: number;
  sha256: string;
}

export interface ImageOutput {
  url: string;
  width: number;
  height: number;
  content_type: string;
  artifact?: ArtifactRef;
}

export interface WaitOptions extends CallOptions {
  timeoutMs?: number;
  pollIntervalMs?: number;
}

interface ScopedListOptions extends CallOptions {
  clientRef?: string;
  after?: string;
}

export type CostState = "estimated" | "mock_reported" | "provider_reported" | "supplier_settled";

export interface ListOptions extends ScopedListOptions {
  /** Exact gateway or caller trace ID. */
  traceId?: string;
  appRef?: string;
  /** Only requests without an application reference. */
  appUnattributed?: boolean;
  /** Only requests without a client reference. */
  clientUnattributed?: boolean;
  model?: string;
  /** Match the stored model ID exactly, including when it has a catalog alias. */
  exactModel?: boolean;
  provider?: Provider;
  status?: RequestStatus;
  costState?: CostState;
  /** Inclusive UTC date in YYYY-MM-DD format. */
  from?: string;
  /** Exclusive UTC date in YYYY-MM-DD format. */
  to?: string;
}

export interface RequestPage {
  requests: RequestTrace[];
  next_after: string | null;
}

export type UsageGroup = "provider" | "model" | "client" | "app" | "day";

export interface UsageOptions extends ScopedListOptions {
  appRef?: string;
  from?: string;
  to?: string;
  groupBy?: UsageGroup;
  model?: string;
  provider?: Provider;
}

export interface UsageMetrics {
  requests: number;
  succeeded: number;
  canceled: number;
  failed: number;
  awaiting_reconciliation: number;
  in_progress: number;
  provider_requests: number;
  completed_outputs: number;
  estimated_usd_nanos: string;
  in_progress_estimated_usd_nanos: string;
  mock_reported_usd_nanos: string;
  provider_reported_usd_nanos: string | null;
  provider_reports_missing_cost: number;
  provider_charges?: ProviderChargeTotal[];
  supplier_settled_usd_nanos?: string | null;
  supplier_settled_requests?: number;
  provider_input_tokens: number | null;
  provider_output_tokens: number | null;
  avg_success_latency_ms: number | null;
}

export interface ProviderChargeTotal {
  provider: Provider;
  unit: string;
  quantity: string;
  observations: number;
}

export interface UsageReport {
  project_id: string;
  window: { from: string; to: string };
  as_of: string;
  source: "mock_provider" | "provider" | "mixed";
  currency: "USD";
  group_by: UsageGroup;
  client_ref: string | null;
  app_ref?: string;
  provider: Provider | null;
  model: string | null;
  totals: UsageMetrics;
  groups: (UsageMetrics & { key: string | null })[];
  next_after: string | null;
}

export interface WebhookEvent {
  id: string;
  schema_version: 1;
  type: "request.succeeded" | "request.canceled" | "request.failed";
  occurred_at: string;
  sequence: number;
  data: Record<string, unknown> & { request_id: string };
}

export type WorkflowWebhookStatus = "succeeded" | "failed" | "canceled" | "expired";

export interface WorkflowWebhookEvent {
  id: string;
  schema_version: 1;
  type: `workflow.run.${WorkflowWebhookStatus}`;
  occurred_at: string;
  sequence: number;
  data: {
    run_id: string;
    workflow_id: string;
    project_id: string;
    workflow_version: number;
    status: WorkflowWebhookStatus;
    fetch_url: string;
  };
}

export interface VerifyWebhookOptions {
  now?: number;
  toleranceSeconds?: number;
}
