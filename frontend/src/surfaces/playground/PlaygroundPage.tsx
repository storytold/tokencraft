import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { Link, useLocation, useSearchParams } from "react-router-dom";
import { ArrowDown, ArrowUpRight, AudioLines, Box, Clapperboard, FlaskConical, Image, LoaderCircle, Play, Terminal } from "lucide-react";
import { Badge, Button, Card, Container, Input, Select, Textarea } from "@/components/ui";
import { useAuth } from "@/lib/auth";
import { MODEL_BY_SLUG } from "@/lib/catalog";
import { isGatewayEndpointUnavailable, withProject, type ProjectScope } from "@/lib/gateway";
import { useOrg } from "@/lib/org";
import { parseDollarCap } from "@/lib/routing-policy";
import { comparisonApi, errorMessage, usd, type ModelCatalog, type ModelQuote, type QuoteInput } from "@/surfaces/comparison/api";
import { statusNames } from "@/surfaces/comparison/RequestHistoryFilters";
import { ReceiptRecovery } from "@/surfaces/comparison/ReceiptRecovery";
import { useModelSubmission } from "@/surfaces/comparison/useModelSubmission";
import { ProjectPicker, ProjectStatus, useProject } from "@/surfaces/projects/ProjectScope";
import { defaultImageAspectRatio, modelChoices, modelPresets, modalityNames, presetLabels, type Modality, type ModelChoice } from "@/surfaces/requests/modelOptions";
import { RequestWaterfall } from "@/surfaces/requests/RequestWaterfall";
import { requestIsActive, useRequestStatus } from "@/surfaces/requests/useRequestListener";
import { ModelPicker } from "./ModelPicker";
import { PlaygroundOutput } from "./PlaygroundOutput";

const modalityIcons = { text: Terminal, image: Image, video: Clapperboard, audio: AudioLines, mesh: Box };
const promptLabels = { text: "Message", image: "Image prompt", video: "Video prompt", audio: "Audio prompt", mesh: "3D prompt" };
const promptLimits = { text: 5000, image: 4000, video: 2048, audio: 2048, mesh: 600 };
const placeholders = {
  text: "Explain how a solar eclipse happens.",
  image: "A stone observatory above a sea of clouds at dawn.",
  video: "A slow aerial approach to an observatory above the clouds.",
  audio: "Soft wind across a mountaintop with distant birds.",
  mesh: "A small stone observatory with a copper dome.",
};
const label = "block min-w-0 space-y-1.5 text-sm font-medium";

export default function PlaygroundPage() {
  const { user } = useAuth();
  const { accounts, current, error, loading, reload, select } = useOrg();
  const { pathname, search } = useLocation();
  const accountId = new URLSearchParams(search).get("account_id");

  useEffect(() => {
    const title = document.title;
    document.title = "Playground · TokenCraft";
    return () => { document.title = title; };
  }, []);

  return (
    <Container className={user ? "space-y-5 px-0 sm:px-0" : "space-y-5 pb-14 pt-7 sm:pt-9"}>
      <header className="flex flex-wrap items-end justify-between gap-3 border-b border-border pb-4">
        <div>
          <h1 className="font-display text-4xl font-extrabold tracking-[-0.05em] sm:text-5xl">Playground</h1>
          <p className="mt-2 text-sm text-muted sm:text-base">Run a model and follow the request live.</p>
        </div>
        {current ? <ProjectComparisonLink /> : <ComparisonLink />}
      </header>
      {current ? <ScopedPlayground /> : (
        <Card className="flex flex-wrap items-center justify-between gap-3 p-5">
          <p role={error ? "alert" : "status"} className="text-sm text-muted">
            {!user ? "Sign in to run requests in your project." : loading ? "Loading your workspace…"
              : error ?? (accountId ? "This organization is unavailable on the connected gateway." : "Your user has no organization.")}
          </p>
          {!user && <Link className="font-medium text-brand hover:underline"
            to={`/login?${new URLSearchParams({ redirect_to: pathname + search })}`}>Sign in</Link>}
          {error && <Button variant="outline" size="sm" onClick={() => void reload()}>Reload organizations</Button>}
          {accountId && accounts.length > 0 && <Button variant="outline" size="sm" onClick={() => select(accounts[0].account_id)}>Use default organization</Button>}
        </Card>
      )}
    </Container>
  );
}

function ComparisonLink({ scope }: { scope?: ProjectScope }) {
  const destination = scope ? withProject("/routing", scope.project_id, scope.account_id) : "/compare";
  return (
    <Link to={destination} className="inline-flex items-center gap-1.5 text-sm font-medium text-brand hover:underline">
      {scope ? "Compare routes" : "Compare models"} <ArrowUpRight className="h-4 w-4" aria-hidden />
    </Link>
  );
}

function ProjectComparisonLink() {
  const { project, scope } = useProject();
  const canSubmit = project?.role === "admin" || project?.role === "developer";
  return <ComparisonLink scope={canSubmit ? scope : undefined} />;
}

function ScopedPlayground() {
  const { project, scope } = useProject();
  const [params] = useSearchParams();
  const requestedModel = params.get("model");
  const canSubmit = project?.role === "admin" || project?.role === "developer";
  if (!project || !scope || project.role === "billing_viewer") {
    return (
      <Card>
        <PlaygroundInputHeader />
        <div className="p-4 sm:p-6">
          {!project || !scope ? <ProjectStatus /> : (
            <p className="text-sm text-muted">Your billing viewer role can read project usage.</p>
          )}
        </div>
      </Card>
    );
  }

  return (
    <PlaygroundWorkspace key={JSON.stringify([scope.project_id, requestedModel])} scope={scope}
      requestedModel={requestedModel} boundClient={project.client_ref} canSubmit={canSubmit} />
  );
}

function PlaygroundInputHeader() {
  return (
    <div className="flex min-h-[3.75rem] items-center justify-between gap-2 border-b border-border px-4 py-3 sm:px-6">
      <h2 className="flex shrink-0 items-center gap-2 text-lg font-semibold"><FlaskConical className="h-4 w-4 text-brand" aria-hidden /> Input</h2>
      <ProjectPicker compact />
    </div>
  );
}

function PlaygroundWorkspace({ scope, requestedModel, boundClient, canSubmit }: {
  scope: ProjectScope; requestedModel: string | null; boundClient: string | null; canSubmit: boolean;
}) {
  const [params, setParams] = useSearchParams();
  const { account_id, project_id } = scope;
  const requestId = params.get("request_id");
  const { data: trace, error: traceError, retrying, refresh: refreshTrace, replace: replaceTrace } = useRequestStatus(scope, requestId);
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [catalogError, setCatalogError] = useState("");
  const [catalogVersion, setCatalogVersion] = useState(0);
  const [selectedKind, setKind] = useState<Modality>();
  const [modelId, setModelId] = useState("");
  const [clientRef, setClientRef] = useState("");
  const [runError, setRunError] = useState("");
  const [canceling, setCanceling] = useState(false);
  const mutation = useRef<AbortController | null>(null);
  const submission = useModelSubmission(scope, (id) => {
    setParams((current) => {
      const next = new URLSearchParams(current);
      next.set("account_id", scope.account_id);
      next.set("project_id", scope.project_id);
      next.set("request_id", id);
      return next;
    });
  });

  useEffect(() => () => mutation.current?.abort(), []);
  useEffect(() => {
    if (!canSubmit) return;
    const controller = new AbortController();
    comparisonApi.models(controller.signal, { account_id, project_id }).then((catalog) => {
      if (!controller.signal.aborted) setCatalog(catalog);
    }).catch((cause) => {
      if (!controller.signal.aborted) setCatalogError(isGatewayEndpointUnavailable(cause)
        ? "This console needs a gateway with model routes. Use the console URL printed by the TokenCraft launcher."
        : errorMessage(cause));
    });
    return () => controller.abort();
  }, [account_id, project_id, canSubmit, catalogVersion]);

  const choices = useMemo(() => modelChoices(catalog), [catalog]);
  const modalities = Array.from(new Set(choices.map(([, choice]) => choice.kind)));
  const linkedChoice = choices.find(([, choice]) => choice.model === requestedModel);
  const publicModel = requestedModel && Object.hasOwn(MODEL_BY_SLUG, requestedModel) ? MODEL_BY_SLUG[requestedModel] : undefined;
  const kind = selectedKind ?? linkedChoice?.[1].kind ?? modalities[0];
  const models = choices.filter(([, choice]) => choice.kind === kind);
  const selected = models.find(([id]) => id === modelId)
    ?? (linkedChoice?.[1].kind === kind ? linkedChoice : undefined) ?? models[0];
  const isActive = requestIsActive(trace?.status);
  const pending = submission.sending ? "submit" : canceling ? "cancel" : undefined;
  const busy = Boolean(pending || isActive || (requestId && !trace && (!traceError || retrying)));
  const attribution = boundClient ?? clientRef.trim();

  function run(quote: ModelQuote, prompt: string, appRef: string) {
    if (!canSubmit || busy || mutation.current) return;
    setRunError("");
    submission.submit(quote, {
      clientRef: attribution, appRef, subscription: null, prompt,
      promptMode: quote.prompt_mode === "fixture" ? undefined : quote.prompt_mode ?? undefined,
    });
  }

  async function cancel() {
    if (!trace || !canSubmit || !isActive || mutation.current || submission.sending) return;
    const controller = new AbortController();
    mutation.current = controller;
    setCanceling(true);
    setRunError("");
    try {
      const result = await comparisonApi.cancel(scope, trace.id, controller.signal);
      if (!controller.signal.aborted) replaceTrace(result);
    } catch (cause) {
      if (!controller.signal.aborted) setRunError(errorMessage(cause));
    } finally {
      mutation.current = null;
      if (!controller.signal.aborted) {
        setCanceling(false);
        refreshTrace();
      }
    }
  }

  const history = withProject(`/dashboard/${scope.account_id}/requests`, scope.project_id);
  const details = requestId ? `${history}&request_id=${encodeURIComponent(requestId)}` : history;

  return (
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(17rem,0.78fr)_minmax(0,1.35fr)] lg:gap-5">
      <Card className="min-w-0">
        <PlaygroundInputHeader />
        <div className="space-y-4 p-4 sm:p-6">
          {!canSubmit ? <p className="text-sm text-muted">Your client viewer role can inspect requests for this client.</p>
            : !catalog ? (
              <div className="space-y-3">
                <p className="text-sm text-muted" role={catalogError ? "alert" : "status"}>{catalogError || "Loading models…"}</p>
                {catalogError && <Button type="button" variant="outline" size="sm" onClick={() => { setCatalogError(""); setCatalogVersion((value) => value + 1); }}>Reload models</Button>}
              </div>
            ) : requestedModel && !linkedChoice ? (
              <div className="space-y-3">
                <p role="status" className="text-sm text-muted">{publicModel?.name ?? "This model"} is unavailable in this project.</p>
                <div className="flex flex-wrap items-center gap-3">
                  {publicModel && <Link to={`/models/${publicModel.slug}#playground`} className="text-sm font-medium text-brand hover:underline">Back to model preview</Link>}
                  {choices.length > 0 && <Button type="button" variant="outline" size="sm" onClick={() => {
                    const next = new URLSearchParams(params);
                    next.delete("model");
                    setParams(next, { replace: true });
                  }}>Choose another model</Button>}
                </div>
              </div>
            ) : !selected ? <p className="text-sm text-muted">The gateway catalog has no runnable models.</p> : (
              <>
                <div role="group" aria-label="Output modality" className="flex flex-wrap gap-1.5">
                  {modalities.map((item) => {
                    const Icon = modalityIcons[item];
                    return <Button type="button" key={item} size="sm" variant={item === kind ? "secondary" : "ghost"}
                      className={item === kind ? "rounded-md bg-brand/10 px-2.5 text-brand" : "rounded-md px-2.5"}
                      aria-pressed={item === kind} onClick={() => { setKind(item); setRunError(""); }}>
                      <Icon className="h-3.5 w-3.5" aria-hidden />{modalityNames[item]}
                    </Button>;
                  })}
                </div>
                <ModelPicker key={kind} choices={models} selected={selected}
                  onChange={(id) => { setModelId(id); setRunError(""); }} />
                <PlaygroundForm key={selected[0]} choice={selected[1]} scope={scope} clientRef={attribution}
                  clientDraft={clientRef} setClientDraft={setClientRef} boundClient={boundClient}
                  busy={busy} pending={pending === "submit"} recoveryPending={submission.recoverable} onRun={run} />
              </>
            )}
          {(runError || submission.error) && <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{runError || submission.error}</p>}
          {canSubmit && submission.recoverable && <ReceiptRecovery sending={submission.sending || canceling}
            retry={submission.retry} dismiss={submission.dismiss} />}
        </div>
      </Card>
      <div className={requestId ? "min-w-0 space-y-4" : "flex min-w-0 self-stretch"}>
        <Card className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <div className="grid min-h-[3.75rem] grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b border-border px-4 py-3 sm:px-6">
            <h2 className="col-start-1 row-start-1 text-lg font-semibold">Output</h2>
            {trace && <Badge tone={trace.status === "succeeded" ? "green" : trace.status === "failed" ? "red" : isActive ? "brand" : "muted"}
              className="col-span-3 row-start-2 justify-self-start min-[420px]:col-span-1 min-[420px]:col-start-2 min-[420px]:row-start-1 min-[420px]:justify-self-end">
              {isActive && <LoaderCircle className="h-3.5 w-3.5 animate-spin motion-reduce:animate-none" aria-hidden />}
              {statusNames[trace.status]}
            </Badge>}
            <Link to={history} className="col-start-3 row-start-1 inline-flex min-h-8 items-center whitespace-nowrap text-xs font-medium text-brand hover:underline">
              Request history
            </Link>
          </div>
          <div className="flex flex-1 flex-col p-4 sm:p-6">
            <PlaygroundOutput key={requestId} scope={scope} trace={trace} requestId={requestId}
              submitting={pending === "submit"} selectedKind={kind} statusError={traceError} />
            {traceError && <div className="mt-4 flex flex-wrap items-center gap-2">
              <p role="status" className="text-sm text-muted">{traceError}{retrying && " Reconnecting…"}</p>
              <Button type="button" variant="outline" size="sm" onClick={refreshTrace}>Refresh status</Button>
            </div>}
          </div>
        </Card>
        {requestId && (
          <div id="request-flow" className="scroll-mt-24 space-y-2">
            <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 px-1">
              <span className="min-w-0 break-all font-mono text-xs text-muted">{requestId}</span>
              <div className="flex flex-wrap items-center gap-3 text-xs">
                {trace && canSubmit && isActive && trace.can_cancel === false && <span className="text-muted">Paid inference started</span>}
                {trace && canSubmit && isActive && (trace.can_cancel ?? (!trace.upstream || trace.status === "queued")) && (
                  <Button type="button" variant="outline" size="sm" disabled={Boolean(pending)} onClick={() => void cancel()}>
                    {pending === "cancel" ? "Canceling…" : "Cancel request"}
                  </Button>
                )}
                <Link to={details} className="inline-flex items-center gap-1 font-medium text-brand hover:underline">
                  Open in Requests <ArrowUpRight className="h-3.5 w-3.5" aria-hidden />
                </Link>
              </div>
            </div>
            <RequestWaterfall scope={scope} requestId={requestId} canAnalyze={canSubmit} contentRemovedAt={trace?.content_removed_at} />
          </div>
        )}
      </div>
    </div>
  );
}

function PlaygroundForm({ choice, scope, clientRef, clientDraft, setClientDraft, boundClient, busy, pending, recoveryPending, onRun }: {
  choice: ModelChoice; scope: ProjectScope; clientRef: string; clientDraft: string;
  setClientDraft: (value: string) => void; boundClient: string | null;
  busy: boolean; pending: boolean; recoveryPending: boolean; onRun: (quote: ModelQuote, prompt: string, appRef: string) => void;
}) {
  const { kind, model } = choice;
  const [prompt, setPrompt] = useState("");
  const [imageCount, setImageCount] = useState("1");
  const [ratioChoice, setRatioChoice] = useState("");
  const [presetChoice, setPresetChoice] = useState("");
  const [inputTokens, setInputTokens] = useState("1000");
  const [outputTokens, setOutputTokens] = useState("500");
  const [costCap, setCostCap] = useState("");
  const [appRef, setAppRef] = useState("");
  const [revision, setRevision] = useState(0);
  const [evaluation, setEvaluation] = useState<{ key: string; quote?: ModelQuote; error?: string }>();
  const presets = useMemo(() => modelPresets(choice), [choice]);
  const ratios = choice.aspectRatios.length ? choice.aspectRatios : ["1:1"];
  const ratio = ratioChoice || defaultImageAspectRatio(ratios);
  const preset = (presets.find(([id]) => id === presetChoice) ?? presets[0])?.[1];
  const maxNanos = parseDollarCap(costCap);
  const validClient = !clientRef || /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}$/.test(clientRef);

  const quoteInput = useMemo<QuoteInput | null>(() => {
    if (maxNanos === undefined || !validClient) return null;
    let workload: QuoteInput["workload"] | undefined = preset;
    if (kind === "text" && validInteger(inputTokens, 128000) && validInteger(outputTokens, 64000)) {
      workload = { kind, max_input_tokens: Number(inputTokens), max_output_tokens: Number(outputTokens) };
    }
    if (kind === "image" && validInteger(imageCount, 8)) {
      workload = { kind, count: Number(imageCount), aspect_ratio: ratio, max_relative_error_bps: null };
    }
    return workload ? { model, workload, provider: null, max_total_usd_nanos: maxNanos } : null;
  }, [kind, model, inputTokens, outputTokens, imageCount, ratio, preset, maxNanos, validClient]);
  const key = JSON.stringify([quoteInput, clientRef, revision]);
  const current = evaluation?.key === key ? evaluation : undefined;
  const quote = current?.quote;
  const offer = quote?.candidates.find((candidate) => candidate.selected);
  const fixture = quote?.prompt_mode === "fixture";
  const needsPrompt = Boolean(quote?.prompt_mode && !fixture);

  useEffect(() => {
    if (!quoteInput) return;
    const controller = new AbortController();
    const timer = window.setTimeout(() => {
      comparisonApi.modelQuote(quoteInput, controller.signal, scope, clientRef).then((value) => {
        if (!controller.signal.aborted) setEvaluation({ key, quote: value });
      }).catch((cause) => {
        if (!controller.signal.aborted) setEvaluation({ key, error: errorMessage(cause) });
      });
    }, 250);
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [quoteInput, scope, key, clientRef]);

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!quote || !offer || busy || recoveryPending || (needsPrompt && !prompt.trim())) return;
    onRun(quote, prompt, appRef.trim());
  }

  return (
    <form className="space-y-4" onSubmit={submit}>
      {fixture ? <p className="rounded-lg bg-surface2 px-3 py-2 text-sm text-muted">This route returns a built-in fixture result.</p> : (
        <label className={label}>{promptLabels[kind]}
          <Textarea rows={5} maxLength={promptLimits[kind]} value={prompt} required={needsPrompt}
            className="min-h-28 resize-y" placeholder={placeholders[kind]} onChange={(event) => setPrompt(event.target.value)} />
        </label>
      )}
      {kind === "image" && <div className="grid grid-cols-2 gap-3">
        <label className={label}>Aspect ratio
          <Select value={ratio} onChange={(event) => setRatioChoice(event.target.value)}>
            {ratios.map((item) => <option key={item}>{item}</option>)}
          </Select>
        </label>
        <label className={label}>Image count
          <Input type="number" min={1} max={8} value={imageCount} onChange={(event) => setImageCount(event.target.value)} required />
        </label>
      </div>}
      {kind === "text" && <div className="grid grid-cols-2 gap-3">
        <label className={label}>Max input tokens
          <Input type="number" min={1} max={128000} value={inputTokens} onChange={(event) => setInputTokens(event.target.value)} required />
        </label>
        <label className={label}>Max output tokens
          <Input type="number" min={1} max={64000} value={outputTokens} onChange={(event) => setOutputTokens(event.target.value)} required />
        </label>
      </div>}
      {preset && <label className={label}>{presetLabels[preset.kind]}
        <Select value={presetChoice || presets[0][0]} onChange={(event) => setPresetChoice(event.target.value)}>
          {presets.map(([id]) => <option key={id}>{id}</option>)}
        </Select>
      </label>}
      <details className="rounded-lg border border-border px-3 py-2.5">
        <summary className="cursor-pointer text-sm font-medium">Advanced settings</summary>
        <div className="mt-3 grid gap-3 sm:grid-cols-2">
          <label className={label}>Cost cap (USD)
            <Input value={costCap} onChange={(event) => setCostCap(event.target.value)} inputMode="decimal" maxLength={14} placeholder="Use project policy" />
          </label>
          <label className={label}>Client reference
            <Input value={boundClient ?? clientDraft} onChange={(event) => setClientDraft(event.target.value)} readOnly={Boolean(boundClient)}
              maxLength={64} placeholder="Optional" />
          </label>
          <label className={label}>App reference
            <Input value={appRef} onChange={(event) => setAppRef(event.target.value)} maxLength={64} placeholder="Optional" />
          </label>
        </div>
      </details>
      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-3">
        <div className="min-w-0 text-sm" role="status">
          {offer ? <>
            <p className="font-medium">Estimate <span className="font-mono">{usd(offer.estimated_usd_nanos)}</span></p>
            <p className="text-xs text-muted">{offer.self_hosted ? "Self-hosted route" : offer.live ? "Provider route" : "Fixture route"}</p>
          </> : <p className="text-muted">{!validClient ? "Enter a valid client reference."
            : maxNanos === undefined ? "Enter a valid USD cap." : !quoteInput ? "Check the request settings."
            : !current ? "Checking estimate…" : quote ? "No eligible route for these settings." : current.error}</p>}
          {current && <button className="mt-0.5 text-xs text-brand underline" type="button" onClick={() => setRevision((value) => value + 1)}>Refresh estimate</button>}
        </div>
        <Button type="submit" disabled={busy || recoveryPending || !offer || (needsPrompt && !prompt.trim())}>
          {pending ? <LoaderCircle className="h-4 w-4 animate-spin motion-reduce:animate-none" aria-hidden /> : <Play className="h-4 w-4" aria-hidden />}
          {pending ? "Submitting…" : "Run model"}
        </Button>
      </div>
      {busy && !pending && <a href="#request-flow" className="inline-flex items-center gap-1 text-sm font-medium text-brand hover:underline">
        Follow request <ArrowDown className="h-4 w-4" aria-hidden />
      </a>}
    </form>
  );
}

function validInteger(input: string, maximum: number) {
  return /^\d+$/.test(input) && Number(input) >= 1 && Number(input) <= maximum;
}
