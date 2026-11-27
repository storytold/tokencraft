import { useEffect, useId, useLayoutEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { ArrowUpRight, BookOpen } from "lucide-react";
import { ChoicePicker } from "@/components/ChoicePicker";
import { Button, Card, Input } from "@/components/ui";
import { errorMessage, gatewayRequest, isGatewayEndpointUnavailable, type ProjectScope } from "@/lib/gateway";
import {
  dollarText, maxMonthlyBudget, parseActiveRequestLimit, parseDataRequirements, parseDollarCap, parseToleratedPercent,
  policyRequestTimeoutMs, routeRankingNames,
  type ActiveRequestLimits, type ActiveRequests, type Bounds, type RoutingPolicy,
} from "@/lib/routing-policy";
import { comparisonApi, formatNanoUsd, providerLabel, usd, type Offer, type Provider, type Quote, type QuoteInput } from "@/surfaces/comparison/api";
import { OfferComparison } from "@/surfaces/comparison/OfferComparison";
import { ProviderOrder, providerOrderValue } from "@/surfaces/comparison/ProviderOrder";

import { PolicyInfo, ProjectDisclosure, routingDocsPath } from "./ProjectDisclosure";
import { useProject } from "./ProjectScope";
import { providerOnboarding } from "./providers/provider-onboarding";
import { usePolicyAutosave } from "./usePolicyAutosave";

type Layer = "organization" | "project";
const label = "block min-w-0 space-y-1 text-sm font-medium";
const errorClass = "text-sm text-rose-600 dark:text-rose-300";
const json = { "Content-Type": "application/json" };
const resetDate = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeZone: "UTC" });
type ProviderOptions = readonly (readonly [Provider, string])[];
type RoutingOffer = Pick<Offer, "id" | "model" | "provider" | "pricing">;

export function ProjectRouting({ scope }: { scope: ProjectScope }) {
  const [layer, setLayer] = useState<Layer>("project");
  return <RoutingPolicyCard accountId={scope.account_id} projectId={scope.project_id} layer={layer} onLayerChange={setLayer} />;
}

export function OrganizationRouting({ accountId }: { accountId: string }) {
  const { scope, projects } = useProject();
  const projectId = scope?.account_id === accountId ? scope.project_id : projects.find((project) => project.is_default)?.id;
  return <RoutingPolicyCard accountId={accountId} projectId={projectId} layer="organization" />;
}

function RoutingPolicyCard({ accountId, projectId, layer, onLayerChange }: {
  accountId: string;
  projectId?: string;
  layer: Layer;
  onLayerChange?: (layer: Layer) => void;
}) {
  const scopedProject = layer === "project" ? projectId : undefined;
  const scopeKey = JSON.stringify([accountId, layer, scopedProject]);
  const [inventory, setInventory] = useState<{ key: string; offers?: RoutingOffer[]; error?: string }>();
  const activeInventory = inventory?.key === scopeKey ? inventory : undefined;

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    providerOnboarding.inventory({ account_id: accountId, project_id: scopedProject }, signal)
      .then(({ offers }) => offers.filter((offer) => offer.available_in_project && !offer.excluded_in_project))
      .catch((issue) => {
        if (!isGatewayEndpointUnavailable(issue)) throw issue;
        return comparisonApi.catalog(signal).then(({ offers }) => offers);
      }).then((offers) => {
        if (!signal.aborted) setInventory({ key: scopeKey, offers });
      }).catch((issue) => {
        if (!signal.aborted) setInventory({ key: scopeKey, error: errorMessage(issue) });
      });
    return () => controller.abort();
  }, [accountId, scopedProject, scopeKey]);

  return (
    <Card className={onLayerChange ? "space-y-4 border-0 p-0" : "space-y-4 p-4 sm:p-6"}>
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          {!onLayerChange && <h2 className="font-display text-xl font-semibold">Routing policy</h2>}
          <p className="mt-1 text-sm text-muted">Set which offers qualify and how the gateway chooses among them. Changes save automatically.</p>
        </div>
        <a href={routingDocsPath} target="_blank" rel="noopener noreferrer"
          className="inline-flex min-h-8 items-center gap-1.5 rounded-md text-sm font-semibold text-brand underline-offset-4 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-brand">
          <BookOpen className="h-4 w-4" aria-hidden /> Policy documentation
          <ArrowUpRight className="h-3.5 w-3.5" aria-hidden /><span className="sr-only"> (opens in a new tab)</span>
        </a>
      </div>
      <div className={onLayerChange
        ? "grid items-end gap-3 rounded-xl border border-border bg-surface2/50 p-3.5 sm:grid-cols-[minmax(12rem,15rem)_1fr] sm:gap-5 sm:p-4"
        : "rounded-xl border border-border bg-surface2/50 p-3.5 sm:p-4"}>
        {onLayerChange && <ChoicePicker label="Routing policy scope" value={layer}
          options={[{ value: "project", label: "This project" }, { value: "organization", label: "Organization" }]}
          onChange={(next) => onLayerChange(next === "organization" ? "organization" : "project")} />}
        <div className="space-y-1 text-xs leading-relaxed text-muted">
          <p className="font-semibold text-fg">{layer === "project" ? "Changes apply to this project." : "Changes apply across the organization."}</p>
          <p>Preferences follow request → project → organization → gateway defaults. All applicable hard limits still apply.</p>
        </div>
      </div>
      {activeInventory?.error && <p role="alert" className={errorClass}>{activeInventory.error}</p>}
      <PolicyEditor key={scopeKey}
        accountId={accountId} projectId={projectId} layer={layer} offers={activeInventory?.offers} />
    </Card>
  );
}

function PolicyEditor({ accountId, projectId, layer, offers = [] }: { accountId: string; projectId?: string; layer: Layer; offers?: RoutingOffer[] }) {
  const query = new URLSearchParams({ account_id: accountId });
  if (layer === "project" && projectId) query.set("project_id", projectId);
  const autosave = usePolicyAutosave(`/v1/routing-policy?${query}`);
  const form = useRef<HTMLFormElement>(null);
  const [editNumber, setEditNumber] = useState(0);
  const capturedEdit = useRef(0);
  const previewRequest = useRef<AbortController>(null);
  const [preview, setPreview] = useState<Quote>();
  const [previewError, setPreviewError] = useState("");
  const [previewBusy, setPreviewBusy] = useState(false);
  const revision = autosave.current;
  const saveBusy = autosave.status.kind === "pending" || autosave.status.kind === "saving";

  useEffect(() => () => previewRequest.current?.abort(), []);
  useEffect(() => {
    if (!saveBusy) return;
    function warnOnExit(event: BeforeUnloadEvent) {
      event.preventDefault();
      event.returnValue = true;
    }
    window.addEventListener("beforeunload", warnOnExit);
    return () => window.removeEventListener("beforeunload", warnOnExit);
  }, [saveBusy]);

  const imageOffers = offers.filter((offer) => offer.pricing.kind === "image");
  const chatOffer = offers.find((offer) => offer.pricing.kind === "text");
  const sample: QuoteInput | undefined = imageOffers[0]
    ? {
      model: imageOffers[0].model, provider: null, max_total_usd_nanos: null,
      workload: { kind: "image", count: 1, aspect_ratio: "7:5", max_relative_error_bps: null },
    }
    : chatOffer && {
      model: chatOffer.model, provider: null, max_total_usd_nanos: null,
      workload: { kind: "text", max_input_tokens: 1000, max_output_tokens: 500 },
    };
  const videoOffers = offers.filter((offer) => offer.pricing.kind === "video");
  const videoOffer = videoOffers[0];
  const video = videoOffer?.pricing.kind === "video" ? videoOffer.pricing : undefined;
  const videoSample: QuoteInput | undefined = video && video.durations_seconds.length && video.aspect_ratios.length ? {
    model: videoOffer.model, provider: null, max_total_usd_nanos: null,
    workload: {
      kind: "video", duration_seconds: video.durations_seconds[0], resolution: video.resolution,
      aspect_ratio: video.aspect_ratios.includes("16:9") ? "17:10" : video.aspect_ratios[0],
    },
  } : undefined;
  const autoSample = sample ?? videoSample;
  const automatic: QuoteInput | undefined = autoSample && {
    ...autoSample, model: "auto",
    workload: autoSample.workload.kind === "image" && imageOffers[0]?.pricing.kind === "image"
      ? { ...autoSample.workload, aspect_ratio: imageOffers[0].pricing.aspect_ratios[0] ?? "1:1" }
      : autoSample.workload.kind === "video" && video
        ? { ...autoSample.workload, aspect_ratio: video.aspect_ratios[0] }
        : autoSample.workload,
  };
  const offerLabels = new Map([...imageOffers, ...videoOffers].map((offer) => [
    offer.id, `${providerLabel(offer.provider)} · ${offer.id}`,
  ]));
  const policy = autosave.loaded?.policy;
  const providerIds = new Set<Provider>([
    ...offers.map((offer) => offer.provider),
    ...(policy?.allowed_providers ?? []),
    ...(policy?.preferred_providers ?? []),
    ...Object.keys(policy?.active_request_limits?.by_provider ?? {}),
  ]);
  const providers: ProviderOptions = [...providerIds].sort().map((id) => [id, providerLabel(id)]);
  for (const id of Object.keys(policy?.offer_overrides ?? {})) {
    if (!offerLabels.has(id)) offerLabels.set(id, id);
  }

  function readDraft() {
    const element = form.current;
    if (!element) return "Policy is still loading.";
    const values = new FormData(element);
    if (values.get("provider-order-mode") === "ordered" && !values.getAll("preferred-provider").length) {
      return "Choose at least one preferred provider to save changes.";
    }
    const draft = readPolicy(values, offerLabels.keys(), providers);
    if (!draft || element.querySelector(":invalid")) {
      return "Check the policy values before saving. Ratio tolerances must be 0–50%, cost caps $0–$1,000, and monthly budgets $0–$1,000,000.";
    }
    const dataRequirements = parseDataRequirements(String(values.get("allowed-regions") ?? ""), String(values.get("provider-retention") ?? ""));
    if (dataRequirements === undefined) return "Enter up to 16 unique lowercase region identifiers and a whole number of seconds.";
    if (dataRequirements) draft.data_requirements = dataRequirements;
    return draft;
  }

  // Dropdowns and provider ordering commit their hidden fields before the form is read.
  useLayoutEffect(() => {
    if (editNumber === capturedEdit.current) return;
    capturedEdit.current = editNumber;
    autosave.change(readDraft());
  });

  function clearPreview() {
    previewRequest.current?.abort();
    setPreview(undefined);
    setPreviewError("");
    setPreviewBusy(false);
  }

  function onEdit() {
    clearPreview();
    setEditNumber((number) => number + 1);
  }

  async function previewPolicy(input: QuoteInput) {
    if (!projectId) return;
    clearPreview();
    const draft = readDraft();
    if (typeof draft === "string") {
      setPreviewError(draft);
      return;
    }
    const controller = new AbortController();
    previewRequest.current = controller;
    const deadline = AbortSignal.timeout(policyRequestTimeoutMs);
    const signal = AbortSignal.any([controller.signal, deadline]);
    const scope = new URLSearchParams({ account_id: accountId, project_id: projectId });
    setPreviewBusy(true);
    try {
      const next = await gatewayRequest<Quote>(`/v1/routing-policy/preview?${scope}`, {
        method: "POST", headers: json, signal, body: JSON.stringify({ layer, policy: draft, input }),
      });
      if (!controller.signal.aborted) setPreview(next);
    } catch (error) {
      if (!controller.signal.aborted) {
        setPreviewError(deadline.aborted ? "Timed out previewing the routing policy." : errorMessage(error));
      }
    } finally {
      if (!controller.signal.aborted) setPreviewBusy(false);
    }
  }

  const statusText = autosave.status.message ?? (autosave.status.kind === "saved" ? `Saved revision ${revision?.revision}.`
    : saveBusy ? "Saving changes…" : `Revision ${revision?.revision}`);

  return (
    <div className="space-y-3">
      {!autosave.loadError && !policy && <p role="status" className="text-sm text-muted">Loading policy…</p>}
      {autosave.loadError && <p role="alert" className={errorClass}>{autosave.loadError}</p>}
      {revision && policy && <form ref={form} className="space-y-4" onSubmit={(event) => { event.preventDefault(); autosave.flush(); }}
        onChange={onEdit} onInvalidCapture={revealInvalidField}>
        <div className="flex flex-wrap items-center gap-2">
          <p role="status" className={autosave.status.message ? errorClass : "text-xs text-muted"}>{statusText}</p>
          {autosave.status.kind === "failed" && <Button type="button" size="sm" variant="outline" onClick={autosave.retry}>Retry save</Button>}
        </div>
        <div className="space-y-2">
          <ProjectDisclosure title="Provider routing" description="Provider access, priority, and offer ranking"
            help={<PolicyInfo title="provider routing" topic="policy-provider-routing">
              All provider allowlists apply first. Priority and ranking help choose among eligible offers.
            </PolicyInfo>}>
            <ProviderAllowlist providers={providers} allowed={policy.allowed_providers} />
            <PolicyProviderOrder providers={providers} current={policy.preferred_providers} layer={layer} onEdit={onEdit} />
            <PolicyChoice title="Route ranking" name="route-ranking" topic="policy-provider-routing"
              help="Ranks eligible offers for an explicit model. Provider priority applies first."
              initialValue={policy.route_ranking ?? ""}
              options={[
                { value: "", label: layer === "project" ? "Use organization default" : "Use gateway default" },
                ...Object.entries(routeRankingNames).map(([value, label]) => ({ value, label })),
              ]} onEdit={onEdit} />
          </ProjectDisclosure>
          <ProjectDisclosure title="Request defaults" description="Starting ratio tolerance and estimated cost cap"
            help={<PolicyInfo title="request defaults" topic="policy-precedence">
              A value supplied by a request takes precedence. Blank project defaults inherit from the organization.
            </PolicyInfo>}>
            <BoundFields group="defaults" bounds={policy.defaults} />
          </ProjectDisclosure>
          <ProjectDisclosure title="Hard limits" description="Maximum ratio tolerance and estimated cost per request"
            help={<PolicyInfo title="hard limits" topic="policy-precedence">
              The lower configured ceiling applies across the organization and project, even when a request supplies its own value.
            </PolicyInfo>}>
            <BoundFields group="limits" bounds={policy.limits} />
            {offerLabels.size > 0 && <ProjectDisclosure title="Offer-specific ratio limits"
              help={<PolicyInfo title="offer-specific ratio limits" topic="policy-precedence">
                An offer limit can further tighten the resolved image or video ratio tolerance for that catalog offer.
              </PolicyInfo>}>
              <div className="grid gap-3 sm:grid-cols-2">
                {[...offerLabels].map(([id, title]) => (
                  <label key={id} className={label}>{title}
                    <Input name={`offer:${id}`} inputMode="decimal" maxLength={5} placeholder="No limit"
                      defaultValue={percentText(policy.offer_overrides[id]?.max_relative_error_bps ?? null)} />
                  </label>
                ))}
              </div>
            </ProjectDisclosure>}
          </ProjectDisclosure>
          <ProjectDisclosure title="Automatic model selection" description="Model Advisor, minimum rating, and usage"
            help={<PolicyInfo title="automatic model selection" topic="policy-automatic-models">
              Rating and usage minimums narrow automatic and advised selection. Each threshold resolves from the request, project, then organization.
            </PolicyInfo>}>
            <PolicyChoice title="Model Advisor" name="advisor-enabled" topic="policy-automatic-models"
              help="Selects a concrete model for scoped image, video, audio, and 3D requests using the gateway’s catalog, eligibility rules, and ranking."
              initialValue={policy.advisor_enabled == null ? (layer === "organization" ? "off" : "") : policy.advisor_enabled ? "on" : "off"}
              options={[
                ...(layer === "project" ? [{ value: "", label: "Use organization default" }] : []),
                { value: "off", label: layer === "organization" ? "Off (default)" : "Off" },
                { value: "on", label: "Use Advisor when model is omitted" },
              ]} onEdit={onEdit} />
            <div className="grid gap-3 sm:grid-cols-2">
              <PolicyField title="Minimum community rating out of 10" topic="policy-automatic-models"
                help="A rating minimum requires at least 10 ratings. Enter 6 for the equivalent of 3 out of 5 stars.">
                {(id) => <Input id={id} name="min-quality" type="number" inputMode="numeric" min={1} max={10} step={1}
                  defaultValue={policy.min_quality ?? ""} placeholder={layer === "project" ? "Inherit" : "No minimum"} />}
              </PolicyField>
              <PolicyField title="Minimum organizations using the model" topic="policy-automatic-models"
                help="Counts organizations with a successful request for this model in the past 30 days.">
                {(id) => <Input id={id} name="min-usage-accounts" type="number" inputMode="numeric" min={5} max={10000} step={1}
                  defaultValue={policy.min_usage_accounts ?? ""} placeholder={layer === "project" ? "Inherit" : "No minimum"} />}
              </PolicyField>
            </div>
          </ProjectDisclosure>
          <ProjectDisclosure title="Provider data handling" description="Allowed processing regions and maximum retention"
            help={<PolicyInfo title="provider data handling" topic="policy-data-handling">
              Eligible offers must declare data handling that meets every configured requirement.
            </PolicyInfo>}>
            <div className="grid gap-3 sm:grid-cols-2">
              <PolicyField title="Allowed processing regions" topic="policy-data-handling"
                help="Enter comma-separated catalog region IDs. The request, project, and organization lists are intersected.">
                {(id) => <Input id={id} name="allowed-regions" maxLength={543} placeholder="e.g. us-east, eu-west"
                  defaultValue={policy.data_requirements?.allowed_regions?.join(", ") ?? ""} />}
              </PolicyField>
              <PolicyField title="Maximum upstream retention (seconds)" topic="policy-data-handling"
                help="The shortest configured maximum applies. Enter zero to require declared zero retention.">
                {(id) => <Input id={id} name="provider-retention" inputMode="numeric" maxLength={10} placeholder="No limit"
                  defaultValue={policy.data_requirements?.max_retention_seconds ?? ""} />}
              </PolicyField>
            </div>
          </ProjectDisclosure>
          <ProjectDisclosure title="Active request limits" description="Overall and per-provider admission caps"
            help={<PolicyInfo title="active request limits" topic="policy-spending-capacity">
              Both scopes can cap queued, running, and unconfirmed requests. Enter zero to pause new admissions under that limit.
            </PolicyInfo>}>
            <CapacityFields providers={providers} limits={policy.active_request_limits} usage={revision.active_requests} />
          </ProjectDisclosure>
          <ProjectDisclosure title="Monthly budget" description="Estimated spend for accepted requests each UTC month"
            help={<PolicyInfo title="monthly budget" topic="policy-spending-capacity">
              New requests must fit every configured budget. The organization budget is shared across its projects.
            </PolicyInfo>}>
            <label className={label}>Estimated budget (USD)
              <Input name="monthly-budget" inputMode="decimal" maxLength={17} placeholder="Unlimited"
                defaultValue={dollarText(policy.monthly_estimated_budget_usd_nanos ?? null)} />
            </label>
            {revision.monthly_budget && <p className="text-sm text-muted">
              <span className="font-medium text-foreground">{formatNanoUsd(revision.monthly_budget.reserved_usd_nanos)}</span>
              {" reserved of "}{usd(revision.monthly_budget.limit_usd_nanos)}
              {" · Resets "}{resetDate.format(new Date(revision.monthly_budget.resets_at))} UTC
            </p>}
          </ProjectDisclosure>
        </div>
        <div className="flex flex-wrap items-center gap-2 border-t border-border pt-4">
          {projectId && sample && <Button type="button" size="sm" variant="outline" disabled={previewBusy} onClick={() => void previewPolicy(sample)}>
            {sample.workload.kind === "image" ? "Preview 7:5 image" : "Preview chat routing"}
          </Button>}
          {projectId && videoSample && <Button type="button" size="sm" variant="outline" disabled={previewBusy} onClick={() => void previewPolicy(videoSample)}>
            Preview video routing
          </Button>}
          {projectId && automatic && <Button type="button" size="sm" variant="outline" disabled={previewBusy} onClick={() => void previewPolicy(automatic)}>
            Preview automatic selection
          </Button>}
          <Button type="button" size="sm" variant="ghost" disabled={saveBusy} onClick={() => { clearPreview(); autosave.reload(); }}>Reload policy</Button>
        </div>
        {previewError && <p role="alert" className={errorClass}>{previewError}</p>}
      </form>}
      {!revision && autosave.loadError && <Button type="button" size="sm" variant="outline" onClick={autosave.reload}>Reload policy</Button>}
      {preview && <OfferComparison quote={preview} loading={false} />}
    </div>
  );
}

function revealInvalidField(event: FormEvent<HTMLFormElement>) {
  if (!(event.target instanceof HTMLElement)) return;
  let section = event.target.closest("details");
  while (section) {
    section.open = true;
    section = section.parentElement?.closest("details") ?? null;
  }
}

function PolicyChoice({ title, name, topic, help, initialValue, options, onEdit }: {
  title: string;
  name: string;
  topic: string;
  help: string;
  initialValue: string;
  options: { value: string; label: string }[];
  onEdit: () => void;
}) {
  const [value, setValue] = useState(initialValue);
  return (
    <div className="flex items-end justify-between gap-2">
      <div className="min-w-0 flex-1">
        <ChoicePicker label={title} value={value} options={options}
          onChange={(next) => { setValue(next); onEdit(); }} />
        <input type="hidden" name={name} value={value} />
      </div>
      <PolicyInfo title={title.toLowerCase()} topic={topic}>{help}</PolicyInfo>
    </div>
  );
}

function PolicyField({ title, topic, help, children }: {
  title: string;
  topic: string;
  help: string;
  children: (id: string) => ReactNode;
}) {
  const id = useId();
  return (
    <div className="min-w-0 space-y-1">
      <div className="flex min-h-8 items-center justify-between gap-1">
        <label htmlFor={id} className="text-sm font-medium">{title}</label>
        <PolicyInfo title={title.toLowerCase()} topic={topic}>{help}</PolicyInfo>
      </div>
      {children(id)}
    </div>
  );
}

function ProviderAllowlist({ allowed, providers }: { allowed?: Provider[] | null; providers: ProviderOptions }) {
  const [restricted, setRestricted] = useState(allowed != null);
  return (
    <fieldset className="min-w-0 space-y-2 rounded-lg border border-border p-3">
      <legend className="px-1 text-sm font-medium">Allowed providers</legend>
      <div className="flex items-center justify-between gap-2">
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" name="restrict-providers" checked={restricted}
            onChange={(event) => setRestricted(event.target.checked)} className="h-4 w-4 accent-brand" />
          Restrict providers
        </label>
        <PolicyInfo title="allowed providers" topic="policy-provider-routing">
          An offer must pass both provider allowlists. Turn on the restriction and clear every provider to pause routing in this scope.
        </PolicyInfo>
      </div>
      <div hidden={!restricted} className={restricted ? "flex flex-wrap gap-x-5 gap-y-2" : undefined}>
        {providers.map(([id, name]) => <label key={id} className="flex items-center gap-2 text-sm">
          <input type="checkbox" name="allowed-provider" value={id} disabled={!restricted}
            defaultChecked={allowed?.includes(id) ?? true} className="h-4 w-4 accent-brand" />
          {name}
        </label>)}
      </div>
    </fieldset>
  );
}

function PolicyProviderOrder({ current, layer, onEdit, providers }: {
  providers: ProviderOptions;
  current?: Provider[] | null;
  layer: Layer;
  onEdit: () => void;
}) {
  const [value, setValue] = useState(() => providerOrderValue(current));
  return (
    <div className="relative pr-10">
      <div className="absolute right-0 top-5">
        <PolicyInfo title="provider priority" topic="policy-provider-routing">
          Preferred providers are considered first among eligible offers. Choose lowest estimated cost to clear inherited provider ordering.
        </PolicyInfo>
      </div>
      <ProviderOrder value={value} providers={providers.map(([id]) => id)} label="Provider priority" inheritLabel={layer === "project" ? "Use organization default" : "Use gateway default"}
        onChange={(next) => { setValue(next); onEdit(); }} />
      <input type="hidden" name="provider-order-mode" value={value.mode} />
      {value.mode === "ordered" && value.order.map((provider) =>
        <input key={provider} type="hidden" name="preferred-provider" value={provider} />
      )}
    </div>
  );
}

function CapacityFields({ limits, usage, providers }: { limits?: ActiveRequestLimits; usage?: ActiveRequests; providers: ProviderOptions }) {
  return (
    <div className="space-y-3">
      <label className={label}>Max active requests
        <Input name="active-request-total" type="number" inputMode="numeric" min={0} step={1}
          max={4_294_967_295} defaultValue={limits?.total ?? ""} placeholder="Unlimited" />
      </label>
      {usage && <p className="text-sm text-muted">{usage.total} active in this scope</p>}
      <ProjectDisclosure title="Per-provider limits"
        help={<PolicyInfo title="per-provider limits" topic="policy-spending-capacity">
          Each provider cap applies alongside the overall cap. A blank field sets no cap for that provider in this scope.
        </PolicyInfo>}>
        <div className="grid gap-3 sm:grid-cols-2">
          {providers.map(([id, name]) => (
            <label className={label} key={id}>{name} active limit
              <Input name={`active-request:${id}`} type="number" inputMode="numeric" min={0} step={1}
                max={4_294_967_295} defaultValue={limits?.by_provider && Object.hasOwn(limits.by_provider, id) ? limits.by_provider[id] : ""} placeholder="Unlimited" />
              {usage && <span className="block text-xs font-normal text-muted">{Object.hasOwn(usage.by_provider, id) ? usage.by_provider[id] : 0} active</span>}
            </label>
          ))}
        </div>
      </ProjectDisclosure>
    </div>
  );
}

function BoundFields({ group, bounds }: { group: "defaults" | "limits"; bounds: Bounds }) {
  const placeholder = group === "defaults" ? "Inherit" : "No limit";
  return (
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      <PolicyField title="Image ratio tolerance (%)" topic="policy-precedence"
        help="Maximum relative difference from the requested image aspect ratio. Zero requires an exact match.">
        {(id) => <Input id={id} name={`${group}:ratio`} inputMode="decimal" maxLength={5} placeholder={placeholder}
          defaultValue={percentText(bounds.max_relative_error_bps)} />}
      </PolicyField>
      <PolicyField title="Video ratio tolerance (%)" topic="policy-precedence"
        help="Maximum relative difference from the requested video aspect ratio. Zero requires an exact match.">
        {(id) => <Input id={id} name={`${group}:video-ratio`} inputMode="decimal" maxLength={5} placeholder={placeholder}
          defaultValue={percentText(bounds.video_max_relative_error_bps ?? null)} />}
      </PolicyField>
      <PolicyField title="Cost cap (USD)" topic="policy-precedence"
        help="Maximum estimated total for one request after the gateway applies any configured hard ceilings.">
        {(id) => <Input id={id} name={`${group}:cost`} inputMode="decimal" maxLength={14} placeholder={placeholder}
          defaultValue={dollarText(bounds.max_total_usd_nanos)} />}
      </PolicyField>
    </div>
  );
}

function percentText(bps: number | null) {
  return bps === null ? "" : String(bps / 100);
}

function readPolicy(form: FormData, offers: Iterable<string>, providers: ProviderOptions): RoutingPolicy | null {
  function readBounds(group: string): Bounds | null {
    const ratio = parseToleratedPercent(String(form.get(`${group}:ratio`) ?? ""));
    const videoRatio = parseToleratedPercent(String(form.get(`${group}:video-ratio`) ?? ""));
    const cost = parseDollarCap(String(form.get(`${group}:cost`) ?? ""));
    if (ratio === undefined || videoRatio === undefined || cost === undefined) return null;
    return {
      max_relative_error_bps: ratio, max_total_usd_nanos: cost,
      ...(videoRatio === null ? {} : { video_max_relative_error_bps: videoRatio }),
    };
  }

  const defaults = readBounds("defaults");
  const limits = readBounds("limits");
  const monthlyBudget = parseDollarCap(String(form.get("monthly-budget") ?? ""), maxMonthlyBudget);
  const activeTotal = parseActiveRequestLimit(String(form.get("active-request-total") ?? ""));
  const minQuality = parseActiveRequestLimit(String(form.get("min-quality") ?? ""));
  const minUsage = parseActiveRequestLimit(String(form.get("min-usage-accounts") ?? ""));
  if (activeTotal === undefined || minQuality === undefined || minUsage === undefined
    || (minQuality !== null && (minQuality < 1 || minQuality > 10))
    || (minUsage !== null && (minUsage < 5 || minUsage > 10000))) return null;
  const activeByProvider: ActiveRequestLimits["by_provider"] = {};
  for (const [id] of providers) {
    const limit = parseActiveRequestLimit(String(form.get(`active-request:${id}`) ?? ""));
    if (limit === undefined) return null;
    if (limit !== null) activeByProvider[id] = limit;
  }
  const capacity: ActiveRequestLimits = {
    ...(activeTotal === null ? {} : { total: activeTotal }),
    ...(Object.keys(activeByProvider).length ? { by_provider: activeByProvider } : {}),
  };
  const selectedProviders = form.getAll("allowed-provider");
  const mode = form.get("provider-order-mode");
  const ranking = form.get("route-ranking");
  const advisor = form.get("advisor-enabled");
  const order = form.getAll("preferred-provider") as Provider[];
  if (!defaults || !limits || monthlyBudget === undefined) return null;
  const entries: [string, { max_relative_error_bps: number }][] = [];
  for (const id of offers) {
    const value = parseToleratedPercent(String(form.get(`offer:${id}`) ?? ""));
    if (value === undefined) return null;
    if (value !== null) entries.push([id, { max_relative_error_bps: value }]);
  }
  return {
    defaults,
    limits,
    offer_overrides: Object.fromEntries(entries),
    ...(minQuality === null ? {} : { min_quality: minQuality }),
    ...(minUsage === null ? {} : { min_usage_accounts: minUsage }),
    ...(advisor === "on" || advisor === "off" ? { advisor_enabled: advisor === "on" } : {}),
    ...(form.has("restrict-providers") ? {
      allowed_providers: providers.flatMap(([id]) => selectedProviders.includes(id) ? [id] : []),
    } : {}),
    ...(mode === "inherit" ? {} : { preferred_providers: order }),
    ...(ranking === "price" || ranking === "recent_reliability" || ranking === "cost_per_completion"
      ? { route_ranking: ranking } : {}),
    ...(monthlyBudget === null ? {} : { monthly_estimated_budget_usd_nanos: monthlyBudget }),
    ...(Object.keys(capacity).length ? { active_request_limits: capacity } : {}),
  };
}
