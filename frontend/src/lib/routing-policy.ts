import type { Provider } from "./gateway";

export const policyRequestTimeoutMs = 10_000;

export type PolicySource =
  | "request" | "model_default" | "organization_default" | "project_default"
  | "organization_limit" | "project_limit" | "organization_offer" | "project_offer";

export type PolicySetting<T = number> = { value: T; source: PolicySource };

export const policySources: Record<PolicySource, string> = {
  request: "Request",
  model_default: "Model default",
  organization_default: "Organization default",
  project_default: "Project default",
  organization_limit: "Organization limit",
  project_limit: "Project limit",
  organization_offer: "Organization offer limit",
  project_offer: "Project offer limit",
};

export type RouteRanking = "price" | "recent_reliability" | "cost_per_completion";

export const routeRankingNames: Record<RouteRanking, string> = {
  price: "Lowest estimated cost",
  recent_reliability: "Recent queue reliability",
  cost_per_completion: "Settled cost per completion",
};

export type DataRequirements = {
  allowed_regions?: string[];
  max_retention_seconds?: number;
};

export type DataHandling = {
  processing_regions?: string[];
  retention_seconds?: number;
};

export type PolicyResolution = {
  data_requirements?: DataRequirements;
  organization_revision: number;
  project_revision: number;
  aspect_ratio_tolerance: PolicySetting | null;
  cost_cap: PolicySetting | null;
  provider_limits?: { organization: boolean; project: boolean };
  provider_preference?: PolicySetting<Provider[]>;
  route_ranking?: PolicySetting<RouteRanking>;
  min_quality?: PolicySetting;
  min_usage_accounts?: PolicySetting;
};

export type Bounds = {
  max_relative_error_bps: number | null;
  video_max_relative_error_bps?: number | null;
  max_total_usd_nanos: number | null;
};

export type ActiveRequestLimits = {
  total?: number;
  by_provider?: Partial<Record<Provider, number>>;
};

export type ActiveRequests = {
  total: number;
  by_provider: Partial<Record<Provider, number>>;
};

export type RoutingPolicy = {
  advisor_enabled?: boolean;
  data_requirements?: DataRequirements;
  defaults: Bounds;
  limits: Bounds;
  offer_overrides: Record<string, { max_relative_error_bps: number }>;
  monthly_estimated_budget_usd_nanos?: number | null;
  active_request_limits?: ActiveRequestLimits;
  allowed_providers?: Provider[] | null;
  preferred_providers?: Provider[] | null;
  route_ranking?: RouteRanking | null;
  min_quality?: number;
  min_usage_accounts?: number;
};

export type PolicyRevision = {
  revision: number;
  policy: RoutingPolicy;
  active_requests?: ActiveRequests;
  monthly_budget?: { reserved_usd_nanos: string; limit_usd_nanos: number; resets_at: string };
};

export function parseToleratedPercent(value: string): number | null | undefined {
  const text = value.trim();
  if (!text) return null;
  if (!/^\d{1,2}(?:\.\d{0,2})?$/.test(text)) return undefined;
  const [whole, fraction = ""] = text.split(".");
  const bps = Number(whole) * 100 + Number(fraction.padEnd(2, "0"));
  return bps <= 5000 ? bps : undefined;
}

export const maxMonthlyBudget = 1_000_000_000_000_000;

export function dollarText(nanos: number | null) {
  return nanos === null ? "" : (nanos / 1_000_000_000).toFixed(9).replace(/\.?0+$/, "");
}

export function parseDollarCap(value: string, maximum = 1_000_000_000_000): number | null | undefined {
  const text = value.trim();
  if (!text) return null;
  if (!/^\d{1,7}(?:\.\d{0,9})?$/.test(text)) return undefined;
  const [whole, fraction = ""] = text.split(".");
  const nanos = Number(whole) * 1_000_000_000 + Number(fraction.padEnd(9, "0"));
  return nanos <= maximum ? nanos : undefined;
}

export function parseActiveRequestLimit(value: string): number | null | undefined {
  const text = value.trim();
  if (!text) return null;
  if (!/^\d{1,10}$/.test(text)) return undefined;
  const limit = Number(text);
  return limit <= 4_294_967_295 ? limit : undefined;
}


export function parseDataRequirements(regions: string, retention: string): DataRequirements | null | undefined {
  const allowed = regions.trim() ? regions.split(",").map((region) => region.trim()) : [];
  const seconds = parseActiveRequestLimit(retention);
  if (allowed.length > 16 || new Set(allowed).size !== allowed.length ||
      allowed.some((region) => !/^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(region) || region.length < 2 || region.length > 32) ||
      seconds === undefined) return undefined;
  if (!allowed.length && seconds === null) return null;
  return {
    ...(allowed.length ? { allowed_regions: allowed.sort() } : {}),
    ...(seconds === null ? {} : { max_retention_seconds: seconds }),
  };
}
