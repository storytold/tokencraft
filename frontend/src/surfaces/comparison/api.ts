import { gatewayRequest as request, gatewayResponse, isGatewayEndpointUnavailable, isProviderId, type ProjectScope, type Provider } from "@/lib/gateway";
import { ApiError } from "@/lib/api";
import type { DataHandling, DataRequirements, PolicyResolution, PolicySetting } from "@/lib/routing-policy";
import { readModelReceipt, type ModelSubmission } from "./modelSubmission";
export { demoEnabled, errorMessage } from "@/lib/gateway";

export type { Provider } from "@/lib/gateway";
const providerNames: Record<string, string> = {
  kinovi: "Kinovi", fal: "fal", openrouter: "OpenRouter", self_hosted: "Self-hosted",
};

export function providerLabel(provider: Provider) {
  return Object.hasOwn(providerNames, provider) ? providerNames[provider] : provider;
}

export function providerChoices(values: readonly (Provider | null | undefined)[] = []) {
  const ids = new Set([...Object.keys(providerNames), ...values.filter(isProviderId)]);
  return [...ids].sort((a, b) => providerLabel(a).localeCompare(providerLabel(b)));
}

type Pricing =
  | { kind: "image"; usd_nanos_per_image: number; aspect_ratios: string[] }
  | ({ kind: "video"; usd_nanos_per_second: number } & VideoCapability)
  | ({ kind: "audio"; usd_nanos_per_second: number } & AudioCapability)
  | { kind: "mesh"; usd_nanos_per_mesh: number; format: string }
  | { kind: "text"; usd_nanos_per_1000_input_tokens: number; usd_nanos_per_1000_output_tokens: number };

type ArtCraftReference = {
  offer_id: string;
  revision: string;
  sha256: string;
  public_key_sha256?: string;
};

export type Offer = {
  id: string;
  model: string;
  provider: Provider;
  adapter: "mock" | "openrouter_chat" | "fal_queue" | "kinovi_queue" | "openai_compatible_chat" | "connector_queue";
  upstream_model?: string;
  pricing: Pricing;
  data_handling?: DataHandling;
  artcraft_reference?: ArtCraftReference;
};
export type PromptMode = "chat" | "image" | "video" | "audio" | "mesh";
export type Catalog = {
  revision: string;
  base_revision?: string;
  offers: Offer[];
  rate_valid_until?: Record<string, string>;
};

type Workload =
  | { kind: "image"; count: number; aspect_ratio: string; max_relative_error_bps: number | null }
  | { kind: "video"; duration_seconds: number; aspect_ratio: string; resolution: string; max_relative_error_bps?: number | null }
  | { kind: "audio"; duration_seconds: number; format: string }
  | { kind: "mesh"; format: string }
  | { kind: "text"; max_input_tokens: number; max_output_tokens: number };

export type QuoteInput = { model: string; provider: Provider | null; preferred_providers?: Provider[]; min_quality?: number; min_usage_accounts?: number; max_total_usd_nanos: number | null; workload: Workload; data_requirements?: DataRequirements };
export type Reason = "eligible" | "provider_excluded" | "provider_policy_excluded" | "provider_region_unmet" | "provider_retention_unmet" | "workload_unsupported" | "aspect_ratio_unsupported" | "above_cost_cap" | "price_stale" | "queue_cooling_down" | "provider_credential_unavailable" | "insufficient_model_ratings" | "below_model_quality" | "below_model_usage";
type OfferPerformance = {
  samples: number;
  succeeded: number;
  unconfirmed: number;
  median_ms: number | null;
  cost?: {
    settled: number;
    settled_usd_nanos: string | null;
  };
};

type PerformanceWindow = {
  window_days: number;
  minimum_samples: number;
  maximum_samples: number;
  applied: boolean;
};

export type Candidate = {
  offer_id: string;
  model?: string;
  provider: Provider;
  live?: boolean;
  self_hosted?: boolean;
  output_size?: { width: number; height: number };
  data_handling?: DataHandling;
  reason: Reason;
  estimated_usd_nanos: number | null;
  applied_aspect_ratio: string | null;
  offer_tolerance?: PolicySetting;
  preference_rank?: number;
  available_after?: string;
  rate_evidence?: { valid_until: string };
  performance?: OfferPerformance;
};

export type Quote = {
  id: string;
  expires_at?: string;
  currency: "USD";
  input: QuoteInput;
  selected_offer_id: string | null;
  pinned_offer_id?: string;
  profile?: "native" | "queue_media" | "chat";
  catalog_revision: string;
  candidates: Candidate[];
  routing?: PolicyResolution;
  performance_window?: PerformanceWindow;
  chat_options?: { temperature?: number; top_p?: number };
};

export type RoutePreview = {
  observed_at: string;
  recorded: Quote;
  current: Quote | null;
  unavailable_reason?: "pinned_offer_unavailable" | "workload_unavailable";
};

export type ModelCatalog = {
  revision: string;
  providers?: Provider[];
  models: { id: string; image: boolean; text: boolean; aspect_ratios: string[]; video?: VideoCapability[]; audio?: AudioCapability[]; mesh_formats?: string[] }[];
};

export type VideoCapability = {
  durations_seconds: number[];
  aspect_ratios: string[];
  resolution: string;
};

export type AudioCapability = { durations_seconds: number[]; format: string };

export type ModelQuote = Pick<Quote, "id" | "expires_at" | "input" | "currency" | "routing" | "performance_window"> & {
  catalog_revision: string;
  max_total_usd_nanos: number | null;
  prompt_mode: PromptMode | "fixture" | null;
  candidates: (Pick<Candidate, "data_handling" | "reason" | "estimated_usd_nanos" | "applied_aspect_ratio" | "live" | "self_hosted" | "output_size" | "preference_rank" | "available_after" | "performance"> & {
    selected: boolean;
    rate_valid_until?: string;
  })[];
};

export type ProviderRecheck =
  | { state: "available"; until: string }
  | { state: "pending"; requested_at: string; deadline: string }
  | { state: "finished"; requested_at: string };

export type TraceCorrelation = {
  trace_id: string;
  admission_span_id: string;
  caller_trace_id?: string;
  caller_span_id?: string;
};

export type Trace = {
  id: string;
  content_removed_at?: string;
  webhook_subscription_id: string | null;
  client_ref: string | null;
  app_ref?: string;
  provider: Provider;
  created_at: string;
  cost_state: CostState;
  status: "queued" | "running" | "succeeded" | "canceled" | "failed" | "awaiting_reconciliation";
  dispatch_paused?: boolean;
  can_cancel?: boolean;
  recheck?: ProviderRecheck;
  correlation?: TraceCorrelation;
  client_budget?: { revision: number; limit_usd_nanos: number };
  model: string;
  estimated_usd_nanos: number | null;
  routing: PolicyResolution | null;
  chat_options: { temperature?: number; top_p?: number };
  mock_reported_usd_nanos: number | null;
  mock_result: { kind: "image"; count: number; aspect_ratio: string } | { kind: "text"; text: string }
    | { kind: "video"; duration_seconds: number; aspect_ratio: string; resolution: string }
    | { kind: "audio"; duration_seconds: number; format: string }
    | { kind: "mesh"; format: string } | null;
  supplier_settlement: { revision: number; amount_usd_nanos: string; recorded_at: string } | null;
  upstream: {
    finish_reason: string | null;
    result_expires_at: string | null;
    input_tokens: number | null;
    output_tokens: number | null;
    reported_usd_nanos: string | null;
    provider_charge?: ProviderCharge & { observed_at: string };
    queue_state: string | null;
  } | null;
};

export type CostState = "estimated" | "mock_reported" | "provider_reported" | "supplier_settled";
export type HistoryFilters = {
  trace_id?: string;
  client_ref?: string;
  app_ref?: string;
  model?: string;
  provider?: Provider;
  status?: Trace["status"];
  cost_state?: CostState;
  from?: string;
  to?: string;
  client_unattributed?: "true";
  app_unattributed?: "true";
  exact_model?: "true";
};

export type TracePage = { requests: Trace[]; next_after: string | null };
type ProviderImage = {
  url: string;
  width: number;
  height: number;
  content_type: string;
  artifact?: { slot: number; byte_length: number; sha256: string };
};
export type ProviderResult = {
  result: { text: string; finish_reason: string | null } | { images: ProviderImage[] }
    | { video: ProviderVideo } | { audio: ProviderAudio } | { mesh: ProviderMesh };
  expires_at: string;
};
export type ProviderVideo = ProviderImage & { duration_ms: number; artifact: NonNullable<ProviderImage["artifact"]> };
export type ProviderAudio = { url: string; content_type: string; duration_ms: number; sample_rate_hz: number; channels: number; artifact: ProviderVideo["artifact"] };
export type ProviderMesh = { url: string; content_type: string; mesh_count: number; primitive_count: number; artifact: ProviderVideo["artifact"] };

export type EvidenceTone = "ok" | "active" | "warning" | "error" | "neutral";

export type EvidenceStep = {
  id: string;
  parent_id: string | null;
  name: string;
  component: string;
  kind: "point" | "interval";
  started_at: string;
  ended_at: string | null;
  status: string;
  tone: EvidenceTone;
  logs: { at: string; code: string; message: string; severity: EvidenceTone }[];
  attributes: { label: string; value: string }[];
};

export type ProviderAttemptState =
  | "rate_limited" | "queued" | "dispatching" | "acknowledged"
  | "succeeded" | "failed" | "canceled" | "outcome_unconfirmed";

export type ComputeTargetAttempt = {
  step_id: string;
  selection: {
    home_cell: string;
    stack: string;
    target: string;
    cloud: string;
    region: string;
    zone: string;
    priority: number;
  };
};

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

export type TraceSummary = {
  headline: string;
  detail?: string;
  next_check: { label: string; step_id: string };
  signals: { label: string; value: string; tone: EvidenceTone; step_id: string }[];
};

export type TraceSnapshot = {
  correlation?: TraceCorrelation;
  provider_path?: ProviderPath;
  revision: string;
  observed_at: string;
  analysis_available: boolean;
  evidence: {
    schema_version: number;
    request_id: string;
    status: Trace["status"];
    subscription_configured: boolean;
    delivery_status: "pending" | "sending" | "delivered" | "dead" | null;
    delivery_attempts_omitted: number;
    steps: EvidenceStep[];
    coverage: string[];
    summary?: TraceSummary;
    routing?: Pick<ProviderPath, "admitted_offer_id" | "downstream"> & {
      attempts: Omit<ProviderAttempt, "adapter" | "correlation_id" | "compute_attempts">[];
    };
  };
};

export type TraceBaseline = {
  model: string;
  provider: string;
  offer_id: string;
  window_start: string;
  window_end: string;
  request_ids: string[];
  timings: {
    metric: "elapsed" | "queue" | "publication";
    request_ms: number | null;
    median_ms: number | null;
    samples: number;
  }[];
};

export type TraceAnalysis = {
  revision: string;
  step_id: string;
  analysis: {
    summary: string;
    findings: { step_id: string; observation: string }[];
    unknowns: string[];
    next_check: string;
  };
};

export type UsageGroupBy = "provider" | "model" | "client" | "app" | "day";

export type ProviderCharge = {
  unit: string;
  quantity: string;
};

export type ProviderChargeTotal = ProviderCharge & {
  provider: string;
  observations: number;
};

export type UsageFilters = {
  from: string;
  to: string;
  group_by: UsageGroupBy;
  client_ref: string;
  app_ref: string;
  provider: Provider | "";
  model: string;
};

export type UsageMetrics = {
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
};

export type UsageReport = {
  project_id: string;
  window: { from: string; to: string };
  as_of: string;
  source: "mock_provider" | "provider" | "mixed";
  currency: "USD";
  group_by: UsageGroupBy;
  client_ref: string | null;
  app_ref?: string;
  provider: Provider | null;
  model: string | null;
  totals: UsageMetrics;
  groups: (UsageMetrics & { key: string | null })[];
  next_after: string | null;
};

export const comparisonApi = {
  catalog: (signal: AbortSignal) => request<Catalog>("/v1/catalog/offers", { credentials: "omit", signal }),
  async models(signal: AbortSignal, scope?: ProjectScope) {
    if (scope) {
      try {
        return await request<ModelCatalog>(`/v1/catalog/models/scoped?${new URLSearchParams(scope)}`, { cache: "no-store", signal });
      } catch (cause) {
        if (!isGatewayEndpointUnavailable(cause)) throw cause;
      }
    }
    return request<ModelCatalog>("/v1/catalog/models", { credentials: "omit", signal });
  },
  modelQuote: (input: QuoteInput, signal: AbortSignal, scope?: ProjectScope, clientRef?: string) => request<ModelQuote>(
    scope ? `/v1/catalog/quotes/scoped?${new URLSearchParams({ ...scope, ...(clientRef ? { client_ref: clientRef } : {}) })}` : "/v1/catalog/quotes", {
    credentials: scope ? "include" : "omit", method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(input), signal,
  }),
  quote: (input: QuoteInput, signal: AbortSignal, scope?: ProjectScope) => request<Quote>(
    scope ? `/v1/quotes/scoped?${new URLSearchParams(scope)}` : "/v1/quotes", {
    credentials: scope ? "include" : "omit", method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(input), signal,
  }),
  async run(submission: ModelSubmission, signal: AbortSignal) {
    const response = await gatewayResponse("/v1/model-requests", {
      method: "POST", redirect: "error", signal,
      headers: { "Content-Type": "application/json", "Idempotency-Key": submission.key },
      body: submission.body,
    });
    const id = await readModelReceipt(response);
    if (!id) throw new ApiError(0, "receipt_unavailable", "The gateway did not return a complete receipt.");
    return id;
  },
  async trace(scope: ProjectScope, id: string, signal: AbortSignal, validator: string | null) {
    const response = await gatewayResponse(
      `/v1/model-requests/${encodeURIComponent(id)}?${new URLSearchParams(scope)}`,
      { signal, headers: validator ? { "If-None-Match": validator } : undefined },
    );
    const hint = response.headers.get("Retry-After") ?? "";
    return {
      trace: response.status === 304 ? null : await response.json() as Trace,
      validator: response.headers.get("ETag"),
      retryAfterMs: /^\d+$/.test(hint) ? Number(hint) * 1_000 : 0,
    };
  },
  previewRoute: (scope: ProjectScope, id: string, options: { max_total_usd_nanos?: number; preferred_providers?: Provider[] }, signal: AbortSignal) =>
    request<RoutePreview>(`/v1/requests/${encodeURIComponent(id)}/route-preview?${new URLSearchParams(scope)}`, {
      method: "POST", signal, headers: { "Content-Type": "application/json" }, body: JSON.stringify(options),
    }),
  debugTrace: (scope: ProjectScope, id: string, signal: AbortSignal) => request<TraceSnapshot>(
    "/v1/requests/" + encodeURIComponent(id) + "/trace?" + new URLSearchParams(scope), { signal },
  ),
  traceBaseline: (scope: ProjectScope, id: string, signal: AbortSignal) => request<TraceBaseline>(
    "/v1/requests/" + encodeURIComponent(id) + "/trace/baseline?" + new URLSearchParams(scope), { signal },
  ),
  analyzeTrace: (scope: ProjectScope, id: string, revision: string, stepId: string, signal: AbortSignal) => request<TraceAnalysis>(
    "/v1/requests/" + encodeURIComponent(id) + "/trace/analysis?" + new URLSearchParams(scope),
    { method: "POST", signal, headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ revision, step_id: stepId }) },
  ),
  result: (scope: ProjectScope, id: string, signal: AbortSignal) => request<ProviderResult>(
    `/v1/model-requests/${encodeURIComponent(id)}/result?${new URLSearchParams(scope)}`, { signal },
  ),
  cancel: (scope: ProjectScope, id: string, signal: AbortSignal) => request<Trace>(
    `/v1/model-requests/${encodeURIComponent(id)}/cancel?${new URLSearchParams(scope)}`,
    { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}", signal },
  ),
  scheduleRecheck: async ({ account_id, project_id }: ProjectScope, id: string, signal: AbortSignal) => {
    await gatewayResponse(
      `/v1/projects/${encodeURIComponent(project_id)}/requests/${encodeURIComponent(id)}/recheck?${new URLSearchParams({ account_id })}`,
      { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}", signal },
    );
  },
  removeContent: ({ account_id, project_id }: ProjectScope, id: string, signal: AbortSignal) =>
    request<{ content_removed_at: string }>(
      `/v1/projects/${encodeURIComponent(project_id)}/requests/${encodeURIComponent(id)}/content?${new URLSearchParams({ account_id })}`,
      { method: "DELETE", signal },
    ),
  locateRequest: (id: string, signal: AbortSignal) => request<ProjectScope & { request_id: string }>(
    "/v1/requests/" + encodeURIComponent(id) + "/location", { signal },
  ),
  history: (scope: ProjectScope, filters: HistoryFilters, after: string | null, signal: AbortSignal) => {
    const params = new URLSearchParams(scope);
    for (const [name, value] of Object.entries(filters)) if (value) params.set(name, value);
    if (after) params.set("after", after);
    return request<TracePage>(`/v1/model-requests?${params}`, { signal });
  },
  usage: (scope: ProjectScope, filters: UsageFilters, after: string | null, signal: AbortSignal) => {
    const params = new URLSearchParams(scope);
    for (const [name, value] of Object.entries(filters)) if (value) params.set(name, value);
    if (after) params.set("after", after);
    return request<UsageReport>(`/v1/usage?${params}`, { signal });
  },
};

const currency = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", minimumFractionDigits: 3, maximumFractionDigits: 9 });
export const usd = (nanos: number | null) => nanos === null ? "—" : currency.format(nanos / 1_000_000_000);

export function formatProviderCharge({ quantity, unit }: ProviderCharge) {
  const label = unit === "credit" && !/^1(?:\.0+)?$/.test(quantity) ? "credits" : unit;
  return `${quantity} ${label}`;
}

export function formatNanoUsd(value: string | null, dividedBy = 1) {
  if (value === null) return "—";
  const divisor = BigInt(dividedBy);
  const nanos = (BigInt(value) + divisor / 2n) / divisor;
  const whole = (nanos / 1_000_000_000n).toLocaleString("en-US");
  const fraction = (nanos % 1_000_000_000n).toString().padStart(9, "0").replace(/0+$/, "").padEnd(3, "0");
  return `$${whole}.${fraction}`;
}
