import { policySources, routeRankingNames, type PolicyResolution, type PolicySetting } from "@/lib/routing-policy";
import { providerLabel, usd } from "./api";

export function toleranceLabel(setting: PolicySetting) {
  return setting.value === 0 ? "Exact" : `Up to ${setting.value / 100}%`;
}

export function PolicySummary({ routing }: { routing: PolicyResolution }) {
  const { aspect_ratio_tolerance: tolerance, cost_cap: cap, provider_limits: providers, provider_preference: preference, route_ranking: ranking } = routing;
  return (
    <dl className="flex flex-wrap gap-x-6 gap-y-2 text-xs">
      <div><dt className="text-muted">Policy revisions</dt><dd className="mt-1 font-mono">Org {routing.organization_revision} · Project {routing.project_revision}</dd></div>
      {tolerance && <div><dt className="text-muted">Ratio tolerance</dt><dd className="mt-1">{toleranceLabel(tolerance)} · {policySources[tolerance.source]}</dd></div>}
      <div><dt className="text-muted">Estimated cost cap</dt><dd className="mt-1">{cap ? `${usd(cap.value)} · ${policySources[cap.source]}` : "No cap"}</dd></div>
      {routing.data_requirements?.allowed_regions && <div><dt className="text-muted">Allowed processing regions</dt>
        <dd className="mt-1 break-all">{routing.data_requirements.allowed_regions.join(", ") || "No common regions"}</dd>
      </div>}
      {routing.data_requirements?.max_retention_seconds !== undefined && <div><dt className="text-muted">Maximum upstream retention</dt>
        <dd className="mt-1">{routing.data_requirements.max_retention_seconds.toLocaleString()} s</dd>
      </div>}
      {routing.min_quality && <div><dt className="text-muted">Minimum rating</dt>
        <dd className="mt-1">{routing.min_quality.value} / 10 · {policySources[routing.min_quality.source]}</dd>
      </div>}
      {routing.min_usage_accounts && <div><dt className="text-muted">Minimum recent usage</dt>
        <dd className="mt-1">{routing.min_usage_accounts.value} organizations · {policySources[routing.min_usage_accounts.source]}</dd>
      </div>}
      {preference && <div><dt className="text-muted">Route preference</dt>
        <dd className="mt-1">{preference.value.length ? preference.value.map(providerLabel).join(" → ") : ranking ? "No provider priority" : "Lowest estimated cost"} · {policySources[preference.source]}</dd>
      </div>}
      {ranking && <div><dt className="text-muted">Ranking strategy</dt>
        <dd className="mt-1">{routeRankingNames[ranking.value]} · {policySources[ranking.source]}</dd>
      </div>}
      {providers && <div><dt className="text-muted">Provider limits</dt>
        <dd className="mt-1">{providers.organization && providers.project ? "Organization and project" : providers.organization ? "Organization" : "Project"}</dd>
      </div>}
    </dl>
  );
}
