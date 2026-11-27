import { useEffect, useId, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { Clock, Radio, RefreshCw } from "lucide-react";
import { Badge, Button, Card, Input, Textarea } from "@/components/ui";
import { demoEnabled, type ProjectScope } from "@/lib/gateway";
import { comparisonApi, errorMessage, formatNanoUsd, providerLabel, usd, type HistoryFilters, type ModelQuote, type PromptMode, type Trace } from "./api";
import { costStateNames, RequestHistoryFilters, statusNames } from "./RequestHistoryFilters";
import { ProviderTrace } from "./ProviderTrace";
import { PolicySummary } from "./PolicySummary";
import { ProviderRecheckControls } from "./ProviderRecheckControls";
import { RequestWaterfall } from "@/surfaces/requests/RequestWaterfall";
import { CopyRequestId } from "@/surfaces/requests/CopyRequestId";
import { requestIsActive, useRequestStatus } from "@/surfaces/requests/useRequestListener";
import { historySearch, setHistoryFilters } from "@/surfaces/requests/historyQuery";
import { diagnosticFromUrl } from "@/surfaces/requests/historyDiagnostics";
import { useRequestHistory } from "@/surfaces/requests/useRequestHistory";
import { RequestsWorkspace } from "@/surfaces/requests/RequestsWorkspace";
import { WorkflowRunLink } from "@/surfaces/requests/WorkflowRunLink";
import { QuoteExpiry } from "./QuoteExpiry";
import { ReceiptRecovery } from "./ReceiptRecovery";
import { useModelSubmission } from "./useModelSubmission";

const timestamp = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const promptLabels = { chat: "Chat message", image: "Image prompt", video: "Video prompt", audio: "Audio prompt", mesh: "3D prompt" };
const promptLimits = { chat: 5000, image: 4000, video: 2048, audio: 2048, mesh: 600 };

export type RequestDelivery = { id: string; status: "loading" | "ready" }
  | { id: string; status: "unavailable"; error: string };

export function RequestActivity({ scope, quote, promptMode, readOnly = false, canManageRequests = false, boundClient, submissionClient, delivery, historyLookup }: {
  scope: ProjectScope; quote?: ModelQuote | null; promptMode?: PromptMode; readOnly?: boolean;
  canManageRequests?: boolean; boundClient?: string | null;
  submissionClient?: { value: string; reference?: string; locked: boolean; onChange: (value: string) => void };
  delivery?: RequestDelivery;
  historyLookup?: ReactNode;
}) {
  const selected = quote?.candidates.find((candidate) => candidate.selected);
  const live = selected?.live === true;
  const local = selected?.self_hosted === true;
  const [params, setParams] = useSearchParams();
  const requestId = params.get("request_id");
  const clientRef = submissionClient ? submissionClient.reference : "";
  const clientId = useId();
  const deliveryBlocked = delivery !== undefined && delivery.status !== "ready";
  const [appRef, setAppRef] = useState("");
  const [prompt, setPrompt] = useState("");
  const isDashboard = historyLookup !== undefined;
  const search = isDashboard ? historySearch(params) : "";
  const diagnostic = isDashboard ? diagnosticFromUrl(params.get("diagnostic")) : undefined;
  const linkedFilters = useMemo<HistoryFilters>(() => ({
    ...Object.fromEntries(new URLSearchParams(search)), diagnostic,
  }), [search, diagnostic]);
  const [localFilters, setLocalFilters] = useState<HistoryFilters>({});
  const filters = isDashboard ? linkedFilters : localFilters;
  const requestHistory = useRequestHistory(scope, filters);
  const { history, error: historyError, refresh: refreshHistory, newest: newestHistory } = requestHistory;
  const { data: trace, error: traceError, retrying, refresh: refreshTrace, replace: setTrace } = useRequestStatus(scope, requestId);
  const [runError, setRunError] = useState("");
  const [canceling, setCanceling] = useState(false);
  const inFlight = useRef<AbortController | null>(null);
  const lastStatus = useRef<{ id: string; status: Trace["status"] } | null>(null);
  const submission = useModelSubmission(scope, (id, recovered) => {
    if (promptMode && !recovered) setPrompt("");
    openRequest(id);
    newestHistory();
    refreshHistory();
  });
  const submitting = submission.sending;

  useEffect(() => () => inFlight.current?.abort(), []);

  const status = trace?.id === requestId ? trace.status : undefined;
  useEffect(() => {
    if (requestId && status) {
      if (lastStatus.current?.id === requestId && requestIsActive(lastStatus.current.status) && !requestIsActive(status)) {
        refreshHistory();
      }
      lastStatus.current = { id: requestId, status };
    }
  }, [requestId, status, refreshHistory]);

  function openRequest(id: string) {
    if (id === requestId) {
      refreshTrace();
      return;
    }
    const next = new URLSearchParams(params);
    next.set("request_id", id);
    next.set("project_id", scope.project_id);
    if (quote !== undefined) next.set("account_id", scope.account_id);
    setParams(next, { replace: isDashboard && Boolean(requestId) });
  }

  function run(event: FormEvent) {
    event.preventDefault();
    if (readOnly || !selected || !quote || deliveryBlocked || clientRef === undefined || inFlight.current || (promptMode && !prompt.trim())) return;
    setRunError("");
    submission.submit(quote, { clientRef, appRef, subscription: delivery?.id ?? null, promptMode, prompt });
  }

  async function cancel() {
    if (readOnly || !trace || trace.id !== requestId || isFinished(trace) || inFlight.current || submitting) return;
    const id = trace.id;
    const controller = new AbortController();
    inFlight.current = controller;
    setCanceling(true);
    setRunError("");
    try {
      const result = await comparisonApi.cancel(scope, id, controller.signal);
      if (!controller.signal.aborted) setTrace(result);
    } catch (error) {
      if (!controller.signal.aborted) setRunError(errorMessage(error));
    } finally {
      inFlight.current = null;
      if (!controller.signal.aborted) {
        setCanceling(false);
        refreshTrace();
      }
    }
  }

  function filterHistory(next: HistoryFilters) {
    if (isDashboard) {
      const query = setHistoryFilters(new URLSearchParams(params), next);
      if (next.diagnostic) query.set("diagnostic", next.diagnostic);
      else query.delete("diagnostic");
      if (query.toString() !== params.toString()) {
        setParams(query);
        return;
      }
    } else {
      setLocalFilters(next);
      return;
    }
    newestHistory();
    refreshHistory();
  }

  const traceContent = (
    <div>
      {trace?.id === requestId && trace && <TraceDetail
        key={trace.id}
        scope={scope}
        trace={trace}
        showWorkflow={demoEnabled && isDashboard && !readOnly && Boolean(trace.app_ref)}
        canceling={canceling || submitting}
        onCancel={readOnly ? undefined : cancel}
        canManageRequests={canManageRequests}
        onContentRemoved={canManageRequests ? (at) => {
          setTrace({
            ...trace,
            content_removed_at: at,
            upstream: trace.upstream && { ...trace.upstream, result_expires_at: null },
          });
          refreshTrace();
        } : undefined}
        onRefresh={() => refreshTrace()}
      />}
      {trace?.id !== requestId && !traceError && <p role="status" className="text-sm text-muted">{requestId ? "Reading the request…" : "Select a request from history."}</p>}
      {traceError && <div className="mt-4 flex flex-wrap items-center gap-3"><p role="alert" className="text-sm text-rose-500">{traceError}{retrying && " Reconnecting…"}</p><Button type="button" variant="outline" size="sm" onClick={() => refreshTrace()}>Refresh trace</Button></div>}
    </div>
  );

  if (isDashboard) {
    return <RequestsWorkspace
      lookup={historyLookup} trace={<>{runError && <p role="alert" className="mb-4 text-sm text-rose-500">{runError}</p>}{traceContent}</>}
      requestId={requestId} filters={filters} boundClient={boundClient} requestHistory={requestHistory}
      onApply={filterHistory} onOpen={openRequest}
      onClose={() => {
        const next = new URLSearchParams(params);
        next.delete("request_id");
        setParams(next, { replace: true });
      }}
    />;
  }

  return (
    <div className={requestId
      ? "grid items-start gap-5 xl:grid-cols-[minmax(0,3fr)_minmax(15rem,1fr)]"
      : "grid items-start gap-5 lg:grid-cols-2"}>
      <Card className="min-w-0 p-5 sm:p-7">
        <h2 className="flex items-center gap-2 text-lg font-semibold"><Radio className="h-4 w-4 text-brand" aria-hidden /> Request trace</h2>
        {delivery && !readOnly && <div className="mt-4 flex flex-wrap items-center justify-between gap-2 border border-border p-3 text-xs">
          <div className="min-w-0">
            <p className="break-all">Webhook <code>{delivery.id}</code></p>
            {delivery.status === "loading" && <p role="status" className="mt-1 text-muted">Checking webhook…</p>}
            {delivery.status === "unavailable" && <p role="alert" className="mt-1 text-rose-500">{delivery.error}</p>}
          </div>
          <Button type="button" size="sm" variant="ghost" disabled={submitting || submission.recoverable} onClick={() => {
            const next = new URLSearchParams(params);
            next.delete("webhook_subscription_id");
            setParams(next, { replace: true });
          }}>Use polling only</Button>
        </div>}
        {quote !== undefined && !readOnly && <form onSubmit={run} className="my-5 flex flex-wrap items-end gap-3">
          {promptMode && <label className="w-full space-y-1.5 text-sm font-medium">{promptLabels[promptMode]}
            <Textarea value={prompt} onChange={(event) => setPrompt(event.target.value)} maxLength={promptLimits[promptMode]}
              rows={3} required placeholder={live || local ? undefined : promptMode === "chat" ? "Enter a message for the local OpenRouter fixture." : `Describe the ${promptMode} for the local fal fixture.`} />
          </label>}
          <div className="min-w-40 flex-1 text-sm font-medium">
            <label htmlFor={clientId}>Client reference</label>
            <Input id={clientId} className="mt-1.5 font-mono" value={submissionClient?.value ?? ""}
              onChange={(event) => submissionClient?.onChange(event.target.value)} readOnly={submissionClient?.locked}
              aria-invalid={clientRef === undefined || undefined} aria-describedby={clientRef === undefined ? clientId + "-error" : undefined}
              maxLength={64} placeholder="Optional opaque ID" />
            {clientRef === undefined && <p id={clientId + "-error"} className="mt-1 text-xs text-rose-500">Enter a valid client reference.</p>}
          </div>
          <label className="min-w-40 flex-1 text-sm font-medium">App reference
            <Input className="mt-1.5 font-mono" value={appRef} onChange={(event) => setAppRef(event.target.value)} maxLength={64} placeholder="Optional opaque ID" />
          </label>
          <Button type="submit" disabled={submitting || canceling || submission.recoverable || deliveryBlocked
            || clientRef === undefined || !selected || Boolean(promptMode && !prompt.trim())}>
            {submitting ? "Submitting…" : local ? "Send self-hosted request" : live ? "Send live request" : promptMode ? `Run ${promptMode === "mesh" ? "3D" : promptMode} fixture` : "Run mock request"}
          </Button>
          {quote?.expires_at && <p className="w-full text-xs text-muted" role="status"><QuoteExpiry expiresAt={quote.expires_at} /></p>}
        </form>}
        {(runError || submission.error) && <p role="alert" className="mb-4 text-sm text-rose-500">{runError || submission.error}</p>}
        {!readOnly && submission.recoverable && <div className="mb-4">
          <ReceiptRecovery sending={submitting || canceling} retry={submission.retry} dismiss={submission.dismiss} />
        </div>}
        {traceContent}
      </Card>
      <Card className="min-w-0 p-5 sm:p-7">
        <div className="flex items-center justify-between gap-3">
          <h2 className="flex items-center gap-2 text-lg font-semibold"><Clock className="h-4 w-4 text-brand" aria-hidden /> Request history</h2>
          <Button type="button" variant="ghost" size="sm" aria-label="Refresh history" onClick={refreshHistory}><RefreshCw className="h-4 w-4" aria-hidden /></Button>
        </div>
        <RequestHistoryFilters key={JSON.stringify(filters)} applied={filters} boundClient={boundClient}
          providers={history?.requests.map((row) => row.provider)} onApply={filterHistory} />
        {historyError && <p role="alert" className="text-sm text-rose-500">{historyError}</p>}
        {history && history.requests.length > 0 && (
          <ul className="divide-y divide-border">
            {history.requests.map((item) => (
              <li key={item.id} className="flex flex-wrap items-center gap-x-1 px-2 py-1">
                <button type="button" aria-pressed={item.id === requestId} className="flex min-w-0 flex-[1_1_12rem] items-center justify-between gap-2 rounded-md px-1 py-2 text-left hover:bg-surface2 focus-visible:outline-brand" onClick={() => openRequest(item.id)}>
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-medium" title={item.model}>{item.model}</span>
                    <span className="mt-1 block break-words text-xs text-muted">
                      {[providerLabel(item.provider), item.app_ref, item.client_ref].filter(Boolean).join(" · ")}
                    </span>
                    <span className="mt-1 block text-xs text-muted">
                      {costStateNames[item.cost_state]}
                      {item.created_at && <> · <time dateTime={item.created_at}>{timestamp.format(new Date(item.created_at))}</time></>}
                    </span>
                  </span>
                  <Badge tone={item.status === "succeeded" ? "green" : "muted"}>{statusNames[item.status]}</Badge>
                </button>
                <CopyRequestId id={item.id} />
              </li>
            ))}
          </ul>
        )}
        {history?.requests.length === 0 && <p className="text-sm text-muted">No requests match this view.</p>}
        {!history && !historyError && <p className="text-sm text-muted">Loading history…</p>}
        <div className="mt-4 flex gap-2">
          {requestHistory.hasNewer && <Button type="button" variant="outline" size="sm" onClick={newestHistory}>Newest</Button>}
          {requestHistory.hasOlder && <Button type="button" variant="outline" size="sm" onClick={requestHistory.older}>Older requests</Button>}
        </div>
      </Card>
    </div>
  );
}

function isFinished(trace: Trace) {
  return !requestIsActive(trace.status);
}

function TraceDetail({ trace, scope, showWorkflow, canceling, onCancel, canManageRequests, onContentRemoved, onRefresh }: {
  trace: Trace;
  scope: ProjectScope;
  showWorkflow: boolean;
  canceling: boolean;
  onCancel?: () => void;
  canManageRequests: boolean;
  onContentRemoved?: (at: string) => void;
  onRefresh: () => void;
}) {
  return (
    <div>
      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border py-4">
        <CopyRequestId id={trace.id} />
        <div className="flex flex-wrap items-center gap-2">
          <Badge tone={trace.status === "succeeded" ? "green" : "muted"}>{statusNames[trace.status]}</Badge>
          {trace.dispatch_paused && <Badge tone="amber">Dispatch paused by organization</Badge>}
          {trace.status === "awaiting_reconciliation" &&
            <Button type="button" size="sm" variant="outline" onClick={onRefresh}>Refresh status</Button>}
          {onCancel && !isFinished(trace) && trace.can_cancel === false && <span className="text-xs text-muted">Paid inference started</span>}
          {onCancel && !isFinished(trace) && (trace.can_cancel ?? (!trace.upstream || trace.status === "queued")) &&
            <Button type="button" size="sm" variant="outline" disabled={canceling} onClick={onCancel}>Cancel request</Button>}
        </div>
      </div>
      {showWorkflow && <WorkflowRunLink scope={scope} request={trace.id} />}
      {trace.recheck && trace.status === "awaiting_reconciliation" && (
        <ProviderRecheckControls scope={scope} requestId={trace.id} recheck={trace.recheck}
          canSchedule={canManageRequests} onRefresh={onRefresh} />
      )}
      <RequestWaterfall scope={scope} requestId={trace.id}
        contentRemovedAt={trace.content_removed_at} canAnalyze={onCancel !== undefined} />
      <dl className="mt-4 grid grid-cols-2 gap-3 rounded-xl bg-surface2/70 p-4 text-xs">
        <div><dt className="text-muted">Model</dt><dd className="mt-1 font-medium">{trace.model}</dd></div>
        <div><dt className="text-muted">Client reference</dt><dd className="mt-1 break-all font-mono">{trace.client_ref ?? "—"}</dd></div>
        <div><dt className="text-muted">App reference</dt><dd className="mt-1 break-all font-mono">{trace.app_ref ?? "—"}</dd></div>
        <div><dt className="text-muted">Provider</dt><dd className="mt-1">{providerLabel(trace.provider)}</dd></div>
        <div><dt className="text-muted">Quoted estimate</dt><dd className="mt-1 font-mono">{usd(trace.estimated_usd_nanos)}</dd></div>
        <div><dt className="text-muted">{trace.upstream ? "Provider-reported cost" : "Mock provider reported"}</dt>
          <dd className="mt-1 font-mono">{trace.upstream ? formatNanoUsd(trace.upstream.reported_usd_nanos) : usd(trace.mock_reported_usd_nanos)}</dd>
        </div>
        {trace.client_budget && <div><dt className="text-muted">Client budget at admission</dt>
          <dd className="mt-1 font-mono">{usd(trace.client_budget.limit_usd_nanos)} · revision {trace.client_budget.revision}</dd>
        </div>}
        {trace.supplier_settlement && <div><dt className="text-muted">Supplier settled</dt>
          <dd className="mt-1 font-mono">{formatNanoUsd(trace.supplier_settlement.amount_usd_nanos)} · revision {trace.supplier_settlement.revision}</dd>
        </div>}
      </dl>
      {trace.correlation && (
        <dl aria-label="Trace correlation" className="mt-3 grid gap-2 rounded-xl border border-border p-3 text-xs sm:grid-cols-2">
          <div><dt className="text-muted">Gateway trace ID</dt>
            <dd className="mt-1 break-all font-mono">{trace.correlation.trace_id}</dd></div>
          {trace.correlation.caller_trace_id && (
            <div><dt className="text-muted">Caller trace ID</dt>
              <dd className="mt-1 break-all font-mono">{trace.correlation.caller_trace_id}</dd></div>
          )}
        </dl>
      )}
      {trace.routing && <div className="mt-3"><PolicySummary routing={trace.routing} /></div>}
      <ProviderTrace scope={scope} trace={trace} />
      {trace.content_removed_at ? <p role="status" className="mt-3 text-xs text-muted">
        Stored content removed <time dateTime={trace.content_removed_at}>{new Date(trace.content_removed_at).toLocaleString()}</time>.
      </p> : onContentRemoved && trace.upstream && isFinished(trace) && trace.status !== "awaiting_reconciliation" && (
        <RemoveRequestContent scope={scope} requestId={trace.id} onRemoved={onContentRemoved} />
      )}
      {trace.webhook_subscription_id && <p className="mt-3 break-all text-xs text-muted">Webhook <code>{trace.webhook_subscription_id}</code></p>}
      {trace.mock_result && <p className="mt-3 text-xs"><span className="text-muted">Fixture result</span>{" "}
        {trace.mock_result.kind === "image"
          ? `${trace.mock_result.count} × ${trace.mock_result.aspect_ratio} image`
          : trace.mock_result.kind === "video"
          ? `${trace.mock_result.duration_seconds}s · ${trace.mock_result.resolution} · ${trace.mock_result.aspect_ratio}`
          : trace.mock_result.kind === "audio"
          ? `${trace.mock_result.duration_seconds}s · ${trace.mock_result.format.toUpperCase()}`
          : trace.mock_result.kind === "mesh"
          ? trace.mock_result.format.toUpperCase()
          : trace.mock_result.text}
      </p>}
    </div>
  );
}

function RemoveRequestContent({ scope, requestId, onRemoved }: {
  scope: ProjectScope; requestId: string; onRemoved: (at: string) => void;
}) {
  const [removing, setRemoving] = useState(false);
  const [error, setError] = useState("");
  const pending = useRef<AbortController | null>(null);
  useEffect(() => () => pending.current?.abort(), []);

  async function remove() {
    if (pending.current || !window.confirm(
      "Permanently remove this request’s stored prompts, results, and media from TokenCraft? The request trace and usage history will remain.",
    )) return;
    const controller = new AbortController();
    pending.current = controller;
    setRemoving(true);
    setError("");
    try {
      const result = await comparisonApi.removeContent(scope, requestId, controller.signal);
      if (!controller.signal.aborted) onRemoved(result.content_removed_at);
    } catch (cause) {
      if (!controller.signal.aborted) setError(errorMessage(cause));
    } finally {
      pending.current = null;
      if (!controller.signal.aborted) setRemoving(false);
    }
  }

  return <div className="mt-3 space-y-2">
    <Button type="button" size="sm" variant="outline" onClick={() => void remove()} disabled={removing}>
      {removing ? "Removing content…" : "Remove stored content"}
    </Button>
    {error && <p role="alert" className="text-xs text-rose-600 dark:text-rose-300">{error}</p>}
  </div>;
}
