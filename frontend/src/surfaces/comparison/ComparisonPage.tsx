import { useEffect, useId, useMemo, useState, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { SlidersHorizontal } from "lucide-react";
import { Button, Card, Container, Input, Select } from "@/components/ui";
import { useOrg } from "@/lib/org";
import { type ProjectScope, withProject } from "@/lib/gateway";
import { parseDollarCap, parseToleratedPercent } from "@/lib/routing-policy";
import { ProjectPicker, ProjectStatus, useProject } from "@/surfaces/projects/ProjectScope";
import { webhooksApi } from "@/surfaces/projects/api";
import { comparisonApi, errorMessage, type ModelCatalog, type ModelQuote, type PromptMode, type QuoteInput } from "./api";
import { OfferComparison } from "./OfferComparison";
import { ProviderOrder, providerOrderValue } from "./ProviderOrder";
import { RequestActivity, type RequestDelivery } from "./RequestActivity";
import { parseClientReference } from "./modelSubmission";
import { catalogModelName, defaultImageAspectRatio, modelChoices, modelPresets, presetLabels, validAspectRatio } from "@/surfaces/requests/modelOptions";

const labelClass = "block space-y-1.5 text-sm font-medium";

function AspectRatioInput({ label, value, ratios, invalid, onChange }: {
  label: string; value: string; ratios: string[]; invalid: boolean; onChange: (value: string) => void;
}) {
  const id = useId();
  return (
    <div className={labelClass}>
      <label htmlFor={id}>{label}</label>
      <Input id={id} value={value} list={ratios.length ? `${id}-ratios` : undefined} maxLength={7}
        aria-invalid={invalid || undefined} aria-describedby={invalid ? `${id}-error` : undefined}
        onChange={(event) => onChange(event.target.value)} />
      {ratios.length > 0 && <datalist id={`${id}-ratios`}>{ratios.map((ratio) => <option key={ratio} value={ratio} />)}</datalist>}
      {invalid && <span id={`${id}-error`} className="block text-xs text-rose-500">Enter two integers from 1 to 100 separated by a colon.</span>}
    </div>
  );
}

export default function ComparisonPage() {
  useEffect(() => {
    const previous = document.title;
    document.title = "Compare model routes · TokenCraft";
    return () => { document.title = previous; };
  }, []);

  return (
    <Container className="space-y-5 px-0 sm:px-0">
      <section className="border-b border-border pb-6 sm:pb-8">
        <h1 className="font-display text-4xl font-extrabold tracking-[-0.05em] sm:text-6xl">Compare model routes</h1>
        <p className="mt-3 max-w-xl text-base leading-7 text-muted">See which routes meet your requirements and what each would cost. Run a request to inspect the selected route.</p>
      </section>
      <ComparisonWorkspace />
    </Container>
  );
}

function ComparisonForm({ scope, clientRef, quoteEnabled, children }: {
  scope?: ProjectScope; clientRef?: string; quoteEnabled: boolean;
  children: (quote: ModelQuote | null, promptMode?: PromptMode) => ReactNode;
}) {
  const [params] = useSearchParams();
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [catalogError, setCatalogError] = useState("");
  const [catalogVersion, setCatalogVersion] = useState(0);
  const [modelChoice, setModelChoice] = useState("");
  const [presetChoice, setPresetChoice] = useState("");
  const [imageRatios, setImageRatios] = useState<Record<string, string>>({});
  const [ratioMode, setRatioMode] = useState(() => {
    const mode = params.get("ratio");
    return mode === "exact" || mode === "nearby" ? mode : "default";
  });
  const [ratioPercent, setRatioPercent] = useState("8");
  const [videoAspectRatio, setVideoAspectRatio] = useState<string | null>(null);
  const [videoRatioMode, setVideoRatioMode] = useState("default");
  const [videoRatioPercent, setVideoRatioPercent] = useState("8");
  const [count, setCount] = useState(1);
  const [inputTokens, setInputTokens] = useState(1000);
  const [outputTokens, setOutputTokens] = useState(500);
  const [maxUsd, setMaxUsd] = useState("");
  const [providerOrder, setProviderOrder] = useState(() => providerOrderValue());
  const [refresh, setRefresh] = useState(0);
  const [evaluation, setEvaluation] = useState<{
    input: QuoteInput; scope?: ProjectScope; clientRef?: string; refresh: number; quote?: ModelQuote; error?: string;
  } | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    comparisonApi.models(signal, scope).then((data) => {
      if (signal.aborted) return;
      setCatalog(data);
      if (scope && data.providers === undefined) {
        // Older gateways only return model capabilities from their scoped catalog.
        void comparisonApi.catalog(signal).then(({ offers }) => {
          if (!signal.aborted) setCatalog({ ...data, providers: [...new Set(offers.map((offer) => offer.provider))].sort() });
        }).catch(() => {});
      }
    }).catch((error) => {
      if (!signal.aborted) setCatalogError(errorMessage(error));
    });
    return () => controller.abort();
  }, [scope, catalogVersion]);

  const models = useMemo(() => modelChoices(catalog), [catalog]);
  const selected = models.find(([id]) => id === modelChoice) ?? models.find(([, model]) => model.kind === "image") ?? models[0];
  const presets = useMemo(() => modelPresets(selected?.[1]), [selected]);
  const preset = presets.find(([label]) => label === presetChoice) ?? presets[0];
  const videoPreset = preset?.[1].kind === "video" ? preset[1] : undefined;
  const maxNanos = parseDollarCap(maxUsd);
  const isImage = selected?.[1].kind === "image";
  const isVideo = selected?.[1].kind === "video";
  const hasRatio = isVideo || isImage;
  const aspectRatio = selected && isImage
    ? imageRatios[selected[0]] ?? defaultImageAspectRatio(selected[1].aspectRatios)
    : videoAspectRatio ?? videoPreset?.aspect_ratio ?? "";
  const invalidRatio = hasRatio && !validAspectRatio(aspectRatio);
  const videoRatios = !videoPreset ? [] : presets.flatMap(([, workload]) =>
    workload.kind === "video" && workload.resolution === videoPreset.resolution
      && workload.duration_seconds === videoPreset.duration_seconds ? [workload.aspect_ratio] : []);
  const matchingMode = isVideo ? videoRatioMode : ratioMode;
  const setMatchingMode = isVideo ? setVideoRatioMode : setRatioMode;
  const matchingPercent = isVideo ? videoRatioPercent : ratioPercent;
  const setMatchingPercent = isVideo ? setVideoRatioPercent : setRatioPercent;
  const tolerance = matchingMode === "default" ? null : matchingMode === "exact" ? 0 : parseToleratedPercent(scope ? matchingPercent : "8") ?? undefined;
  const input = useMemo<QuoteInput | null>(() => {
    if (!quoteEnabled || !selected || maxNanos === undefined || invalidRatio) return null;
    const { kind, model } = selected[1];
    if (providerOrder.mode === "ordered" && !providerOrder.order.length) return null;
    const preference = providerOrder.mode === "inherit" ? {} : {
      preferred_providers: providerOrder.mode === "ordered" ? providerOrder.order : [],
    };
    if ((kind === "image" || kind === "video") && tolerance === undefined) return null;
    if (videoPreset) {
      return {
        model, provider: null, max_total_usd_nanos: maxNanos, ...preference,
        workload: {
          ...videoPreset, aspect_ratio: aspectRatio,
          ...(tolerance == null ? {} : { max_relative_error_bps: tolerance }),
        },
      };
    }
    if (kind === "video" || kind === "audio" || kind === "mesh") {
      return preset ? { model, provider: null, max_total_usd_nanos: maxNanos, workload: preset[1], ...preference } : null;
    }
    return {
      model, provider: null, max_total_usd_nanos: maxNanos, ...preference,
      workload: kind === "image"
        ? { kind, count, aspect_ratio: aspectRatio, max_relative_error_bps: tolerance ?? null }
        : { kind, max_input_tokens: inputTokens, max_output_tokens: outputTokens },
    };
  }, [quoteEnabled, selected, maxNanos, invalidRatio, count, aspectRatio, tolerance, inputTokens, outputTokens, preset, providerOrder, videoPreset]);

  useEffect(() => {
    if (!input) return;
    const controller = new AbortController();
    const timer = window.setTimeout(() => {
      comparisonApi.modelQuote(input, controller.signal, scope, clientRef).then((quote) => {
        if (!controller.signal.aborted) setEvaluation({ input, scope, clientRef, refresh, quote });
      }).catch((error) => {
        if (!controller.signal.aborted) setEvaluation({ input, scope, clientRef, refresh, error: errorMessage(error) });
      });
    }, 200);
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [input, refresh, scope, clientRef]);

  const current = evaluation && evaluation.input === input && evaluation.scope === scope && evaluation.clientRef === clientRef && evaluation.refresh === refresh ? evaluation : null;
  const quote = current?.quote ?? null;
  const mode = quote?.prompt_mode;
  const promptMode = mode === "fixture" ? undefined : mode ?? undefined;

  return (
    <>
      <div className="grid items-start gap-5 lg:grid-cols-[minmax(17rem,0.77fr)_minmax(0,1.23fr)]">
        <Card className="p-5 sm:p-7">
          <h2 className="mb-5 flex items-center gap-2 text-lg font-semibold"><SlidersHorizontal className="h-4 w-4 text-brand" aria-hidden /> Request settings</h2>
          <div className="space-y-4">
            <label className={labelClass}>Model
              <Select value={selected?.[0] ?? ""} disabled={!models.length} onChange={(event) => { setModelChoice(event.target.value); setVideoAspectRatio(null); }}>
                {!models.length && <option value="">{catalogError ? "Models unavailable" : catalog ? "No models available" : "Loading models…"}</option>}
                {models.map(([id, item]) => <option key={id} value={id}>{catalogModelName(item.model) ?? item.model} · {item.kind === "mesh" ? "3D" : item.kind}</option>)}
              </Select>
            </label>
            {selected?.[1].kind === "image" && (
              <div className="grid grid-cols-2 gap-3">
                <AspectRatioInput label="Aspect ratio" value={aspectRatio} ratios={selected[1].aspectRatios} invalid={invalidRatio}
                  onChange={(value) => setImageRatios((drafts) => ({ ...drafts, [selected[0]]: value }))} />
                <label className={labelClass}>Image count<Input type="number" min={1} max={8} value={count} onChange={(event) => setCount(Number(event.target.value))} /></label>
              </div>
            )}
            {selected?.[1].kind === "text" && (
              <div className="grid grid-cols-2 gap-3">
                <label className={labelClass}>Max input tokens<Input type="number" min={1} max={128000} value={inputTokens} onChange={(event) => setInputTokens(Number(event.target.value))} /></label>
                <label className={labelClass}>Max output tokens<Input type="number" min={1} max={64000} value={outputTokens} onChange={(event) => setOutputTokens(Number(event.target.value))} /></label>
              </div>
            )}
            {preset && <label className={labelClass}>{presetLabels[preset[1].kind]}
              <Select value={preset[0]} onChange={(event) => { setPresetChoice(event.target.value); setVideoAspectRatio(null); }}>
                {presets.map(([label]) => <option key={label}>{label}</option>)}
              </Select>
            </label>}
            {videoPreset && <AspectRatioInput label="Requested aspect ratio" value={aspectRatio} ratios={videoRatios}
              invalid={invalidRatio} onChange={setVideoAspectRatio} />}
            {hasRatio && (scope ? <>
              <label className={labelClass}>Ratio matching
                <Select value={matchingMode} onChange={(event) => setMatchingMode(event.target.value)}>
                  <option value="default">Use project policy</option>
                  <option value="exact">Exact ratio</option>
                  <option value="nearby">Allow a nearby ratio</option>
                </Select>
              </label>
              {matchingMode === "nearby" && <label className={labelClass}>Maximum difference (%)
                <Input value={matchingPercent} inputMode="decimal" maxLength={5} onChange={(event) => setMatchingPercent(event.target.value)} />
              </label>}
            </> : (
              <label className="flex cursor-pointer items-start gap-3 rounded-xl border border-border bg-surface2/40 p-3 text-sm">
                <input type="checkbox" className="mt-1 h-4 w-4 accent-brand" checked={matchingMode === "nearby"} onChange={(event) => setMatchingMode(event.target.checked ? "nearby" : "default")} />
                <span><span className="block font-medium">Allow a nearby ratio</span><span className="mt-0.5 block text-xs text-muted">Up to 8% difference with the same orientation.</span></span>
              </label>
            ))}
            <label className={labelClass}>Cost cap (USD)<Input inputMode="decimal" maxLength={14} value={maxUsd} onChange={(event) => setMaxUsd(event.target.value)} placeholder={scope ? "Use policy" : "Optional"} /></label>
            {scope && <details className="rounded-md border border-border p-3">
              <summary className="cursor-pointer text-sm font-medium">Provider priority</summary>
              <div className="mt-3">
                <ProviderOrder value={providerOrder} providers={catalog?.providers} onChange={setProviderOrder} inheritLabel="Use project policy" />
              </div>
            </details>}
            {providerOrder.mode === "ordered" && !providerOrder.order.length && <p className="text-sm text-muted">Choose a preferred provider to see a quote.</p>}
            {catalogError && <div className="flex flex-wrap items-center gap-3">
              <p className="text-sm text-rose-500" role="alert">{catalogError}</p>
              <Button type="button" variant="outline" size="sm" onClick={() => {
                setCatalogError("");
                setCatalogVersion((value) => value + 1);
              }}>Reload models</Button>
            </div>}
            {maxNanos === undefined && <p className="text-sm text-rose-500" role="alert">Enter a cost from $0 to $1,000 with up to nine decimal places.</p>}
            {tolerance === undefined && hasRatio && <p className="text-sm text-rose-500" role="alert">Enter a ratio tolerance from 0% to 50% with up to two decimal places.</p>}
            {current?.error && <p className="text-sm text-rose-500" role="alert">{current.error}</p>}
            <Button type="button" size="sm" variant="outline" disabled={!input} onClick={() => setRefresh((value) => value + 1)}>Refresh quote</Button>
          </div>
        </Card>
        <OfferComparison quote={quote} loading={Boolean(input && !current)} />
      </div>
      {children(quote, promptMode)}
    </>
  );
}

function ComparisonWorkspace() {
  const { accounts, current, error, loading, reload, select } = useOrg();
  const [params] = useSearchParams();
  const requested = params.get("account_id");
  if (current) return <ScopedComparison />;
  return (
    <Card className="flex flex-wrap items-center justify-between gap-3 p-5">
      <p className="text-sm text-muted" role={error ? "alert" : "status"}>{loading ? "Loading your workspace…"
        : error ?? (requested ? "This organization is unavailable on the connected gateway." : "Your user has no organization.")}</p>
      {error && <Button variant="outline" size="sm" onClick={() => void reload()}>Reload organizations</Button>}
      {requested && accounts.length > 0 && <Button variant="outline" size="sm" onClick={() => select(accounts[0].account_id)}>Use default organization</Button>}
    </Card>
  );
}

function ScopedComparison() {
  const { scope, project } = useProject();
  const [params] = useSearchParams();
  const [attribution, setAttribution] = useState<{ projectId: string; value: string }>();
  const [webhook, setWebhook] = useState<{ key: string; clientRef: string | null } | { key: string; error: string }>();
  const canSubmit = project?.role === "admin" || project?.role === "developer";
  const quoteScope = canSubmit ? scope : undefined;
  const webhookId = canSubmit ? params.get("webhook_subscription_id") : null;
  const webhookKey = quoteScope && webhookId !== null
    ? JSON.stringify([quoteScope.account_id, quoteScope.project_id, webhookId])
    : null;

  useEffect(() => {
    setWebhook(undefined);
    if (!quoteScope || webhookId === null || !webhookKey) return;
    const controller = new AbortController();
    webhooksApi.subscription(quoteScope, webhookId, controller.signal).then((record) => {
      if (controller.signal.aborted) return;
      const clientRef = record.client_ref;
      const validClient = clientRef === null
        || (typeof clientRef === "string" && clientRef !== "" && parseClientReference(clientRef) === clientRef);
      setWebhook(record.id === webhookId && !record.revoked_at && !record.workflow_app_id && validClient
        ? { key: webhookKey, clientRef }
        : { key: webhookKey, error: "This subscription is unavailable for request delivery." });
    }).catch((error) => {
      if (!controller.signal.aborted) setWebhook({ key: webhookKey, error: errorMessage(error) || "Webhook metadata is unavailable." });
    });
    return () => controller.abort();
  }, [quoteScope, webhookId, webhookKey]);

  const resolved = webhook?.key === webhookKey ? webhook : undefined;
  const delivery: RequestDelivery | undefined = webhookId === null ? undefined
    : !resolved ? { id: webhookId, status: "loading" }
      : "error" in resolved ? { id: webhookId, status: "unavailable", error: resolved.error }
        : { id: webhookId, status: "ready" };
  const requiredClient = project?.client_ref ?? (resolved && "clientRef" in resolved ? resolved.clientRef : null);
  const clientDraft = requiredClient ?? (attribution?.projectId === scope?.project_id ? attribution?.value ?? "" : "");
  const clientRef = parseClientReference(clientDraft);
  return (
    <>
      <div className="flex flex-wrap items-end gap-4">
        <ProjectPicker />
        {scope && <Link className="pb-2 font-mono text-sm font-medium text-brand hover:underline" to={withProject(`/dashboard/${scope.account_id}/usage`, scope.project_id)}>View project usage</Link>}
      </div>
      {!scope || !project ? <ProjectStatus /> : (
        <ComparisonForm key={JSON.stringify([scope.account_id, scope.project_id, canSubmit])}
          scope={quoteScope} clientRef={canSubmit ? clientRef : undefined}
          quoteEnabled={clientRef !== undefined && (!delivery || delivery.status === "ready")}>
          {(quote, promptMode) => project.role === "billing_viewer" ? (
            <p className="text-sm text-muted">Your billing viewer role can read usage for this project.</p>
          ) : <RequestActivity scope={scope} quote={quote} promptMode={promptMode}
            readOnly={project.role === "client_viewer"} boundClient={project.client_ref} delivery={delivery}
            submissionClient={{ value: clientDraft, reference: clientRef, locked: requiredClient !== null,
              onChange: (value) => setAttribution({ projectId: scope.project_id, value }) }} />}
        </ComparisonForm>
      )}
    </>
  );
}
