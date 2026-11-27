import { useLayoutEffect, useId, useRef, useState, type ReactNode } from "react";
import { ArrowLeft, Link2, ChevronDown, ChevronLeft, ChevronRight, Clock, List, Radio, RefreshCw, SlidersHorizontal, Sparkles, X } from "lucide-react";
import { Badge, Button, Card } from "@/components/ui";
import { providerLabel, usd, type HistoryFilters, type Trace } from "@/surfaces/comparison/api";
import { costStateNames, RequestHistoryFilters, statusNames } from "@/surfaces/comparison/RequestHistoryFilters";
import { CopyRequestId } from "./CopyRequestId";
import { diagnosticNames, shiftDate, validDate } from "./historyDiagnostics";
import type { useRequestHistory } from "./useRequestHistory";
import { RequestFilterChat } from "./RequestFilterChat";
import "./requests-workspace.css";

const shortTime = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });

function filterChips(filters: HistoryFilters) {
  const chips: { label: string; keys: (keyof HistoryFilters)[] }[] = [];
  const { status, diagnostic, provider, model, from, to, client_ref, app_ref, trace_id, cost_state } = filters;
  if (status) chips.push({ label: statusNames[status] ?? status, keys: ["status"] });
  if (diagnostic) chips.push({ label: diagnosticNames[diagnostic], keys: ["diagnostic"] });
  if (from || to) {
    const end = to && validDate(to) ? shiftDate(to, -1) : to;
    chips.push({ label: from === end ? `${from} UTC` : `${from ?? "…"} – ${end ?? "…"} UTC`, keys: ["from", "to"] });
  }
  if (provider) chips.push({ label: providerLabel(provider), keys: ["provider"] });
  if (model) chips.push({ label: `${filters.exact_model ? "Exact model" : "Model"} · ${model}`, keys: ["model", "exact_model"] });
  if (client_ref || filters.client_unattributed) chips.push({ label: client_ref ? `Client · ${client_ref}` : "Unattributed clients", keys: ["client_ref", "client_unattributed"] });
  if (app_ref || filters.app_unattributed) chips.push({ label: app_ref ? `App · ${app_ref}` : "Unattributed apps", keys: ["app_ref", "app_unattributed"] });
  if (trace_id) chips.push({ label: `Trace · ${trace_id}`, keys: ["trace_id"] });
  if (cost_state) chips.push({ label: costStateNames[cost_state] ?? cost_state, keys: ["cost_state"] });
  return chips;
}

const quickViews = [
  { label: "All", status: undefined, diagnostic: undefined },
  { label: "Failed", status: "failed", diagnostic: undefined },
  { label: "Warnings", status: undefined, diagnostic: "warnings" },
  { label: "Unconfirmed", status: "awaiting_reconciliation", diagnostic: undefined },
] as const;

const statusTone = {
  queued: "muted",
  running: "brand",
  succeeded: "green",
  canceled: "muted",
  failed: "red",
  awaiting_reconciliation: "amber",
} as const;

export function RequestsWorkspace({
  lookup, trace, requestId, filters, boundClient, requestHistory, onApply, onOpen, onClose,
}: {
  lookup: ReactNode; trace: ReactNode; requestId: string | null;
  filters: HistoryFilters; boundClient?: string | null; requestHistory: ReturnType<typeof useRequestHistory>;
  onApply: (filters: HistoryFilters) => void;
  onOpen: (id: string) => void; onClose: () => void;
}) {
  const { history, rows, loading, error, page, hasOlder, hasNewer } = requestHistory;
  const historyId = useId();
  const filtersId = useId();
  const chatId = useId();
  const [collapsed, setCollapsed] = useState(false);
  const [mobileHistory, setMobileHistory] = useState(false);
  const [copyStatus, setCopyStatus] = useState("");
  const [panel, setPanel] = useState<"filters" | "chat" | null>(null);
  const filterButton = useRef<HTMLButtonElement>(null);
  const historyTitle = useRef<HTMLHeadingElement>(null);
  const traceTitle = useRef<HTMLHeadingElement>(null);
  const historyScroll = useRef<number | null>(null);
  const previousRequest = useRef<string | null>(null);
  const chips = filterChips(filters);

  useLayoutEffect(() => {
    setCopyStatus("");
    if (requestId) {
      setMobileHistory(false);
      traceTitle.current?.focus({ preventScroll: true });
      if (previousRequest.current === null) traceTitle.current?.scrollIntoView({ block: "nearest" });
    } else if (previousRequest.current) {
      const results = historyTitle.current?.closest("section");
      const button = results?.querySelector<HTMLButtonElement>(`button[data-request-id="${CSS.escape(previousRequest.current)}"]`);
      (button ?? historyTitle.current)?.focus({ preventScroll: true });
      if (historyScroll.current !== null) {
        window.scrollTo({ top: historyScroll.current, behavior: "instant" });
        historyScroll.current = null;
      }
      (button ?? historyTitle.current)?.scrollIntoView({ block: "nearest" });
    }
    previousRequest.current = requestId;
  }, [requestId]);

  function togglePanel(next: "filters" | "chat") {
    setPanel((current) => current === next ? null : next);
  }

  function open(id: string) {
    if (!requestId) historyScroll.current = window.scrollY;
    setMobileHistory(false);
    if (id === requestId) traceTitle.current?.focus({ preventScroll: true });
    onOpen(id);
  }

  return (
    <div className="tc-requests-workspace" data-trace={Boolean(requestId)}
      data-collapsed={collapsed} data-mobile-history={mobileHistory}>
      <section id={historyId} className="tc-requests-history" aria-label="Request history">
        <Card className="min-w-0">
          <div className="tc-requests-toolbar">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
              <h2 ref={historyTitle} tabIndex={-1} className="mr-auto flex items-center gap-2 text-base font-semibold">
                <Clock className="h-4 w-4 text-brand" aria-hidden /> Request history
              </h2>
              <Button type="button" size="sm" variant="ghost" onClick={requestHistory.refresh} disabled={loading}
                aria-label="Refresh history" title="Refresh history" className="px-2">
                <RefreshCw className="h-4 w-4" aria-hidden />
              </Button>
              {requestId && <Button type="button" size="sm" variant="ghost" onClick={() => {
                setCollapsed(true);
                requestAnimationFrame(() => traceTitle.current?.focus({ preventScroll: true }));
              }}
                aria-label="Collapse history" aria-expanded={!collapsed} aria-controls={historyId} title="Collapse history"
                className="tc-requests-desktop px-2">
                <ChevronLeft className="h-4 w-4" aria-hidden />
              </Button>}
              {requestId && <Button type="button" size="sm" variant="outline" className="tc-requests-mobile"
                onClick={() => { setMobileHistory(false); requestAnimationFrame(() => traceTitle.current?.focus({ preventScroll: true })); }}>
                View trace <ChevronRight className="h-4 w-4" aria-hidden />
              </Button>}
            </div>
            {boundClient && <p className="break-all text-xs text-muted">Client <code>{boundClient}</code></p>}
            <div className="tc-requests-search">
              {lookup}
              <div className="flex flex-wrap gap-2">
                <Button ref={filterButton} type="button" size="sm" variant={panel === "filters" ? "secondary" : "outline"}
                  className="h-10 rounded-lg" aria-controls={filtersId} aria-expanded={panel === "filters"}
                  onClick={() => togglePanel("filters")}>
                  <SlidersHorizontal className="h-4 w-4" aria-hidden /> Filters
                  {chips.length > 0 && <span className="rounded bg-brand/10 px-1.5 text-xs text-brand">{chips.length}</span>}
                  <ChevronDown className={`h-3.5 w-3.5 ${panel === "filters" ? "rotate-180" : ""}`} aria-hidden />
                </Button>
                <Button type="button" size="sm" variant={panel === "chat" ? "secondary" : "outline"}
                  className="h-10 rounded-lg text-brand" aria-controls={chatId} aria-expanded={panel === "chat"}
                  onClick={() => togglePanel("chat")}>
                  <Sparkles className="h-4 w-4" aria-hidden /> Ask AI
                </Button>
              </div>
            </div>
            <div id={filtersId} hidden={panel !== "filters"}>
              <RequestHistoryFilters embedded key={JSON.stringify(filters)} applied={filters} providers={rows.map((row) => row.provider)}
                boundClient={boundClient} onApply={(next) => {
                  onApply(next);
                  setPanel(null);
                  filterButton.current?.focus();
                }} />
            </div>
            <div id={chatId} hidden={panel !== "chat"}>
              <RequestFilterChat filters={filters} providers={rows.map((row) => row.provider)} onApply={onApply}
                describe={(value) => filterChips(value).map(({ label }) => label).join(" · ") || "All requests"} />
            </div>
            <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Quick views">
              {quickViews.map(({ label, status, diagnostic }) => {
                const selected = filters.status === status && filters.diagnostic === diagnostic;
                return <button type="button" key={label} aria-pressed={selected} className="tc-request-quick-view"
                  onClick={() => onApply({ ...filters, status, diagnostic })}>{label}</button>;
              })}
            </div>
            {chips.length > 0 && <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Applied filters">
              {chips.map(({ label, keys }) => (
                <button type="button" key={keys[0]} title={`Remove ${label}`} aria-label={`Remove ${label}`}
                  className="tc-request-filter-chip" onClick={() => {
                    const next = { ...filters };
                    keys.forEach((key) => delete next[key]);
                    onApply(next);
                  }}>
                  <span className="truncate">{label}</span><X className="h-3 w-3 shrink-0" aria-hidden />
                </button>
              ))}
              <button type="button" className="px-1.5 py-1 text-xs text-muted hover:text-fg hover:underline"
                onClick={() => onApply({})}>Clear all</button>
            </div>}
          </div>
          <div className="tc-requests-results" aria-busy={loading && !rows.length}>
            {error && <p className="px-4 py-4 text-sm text-rose-600 dark:text-rose-300" role="alert">
              {error}
            </p>}
            {rows.length ? (
              <table className="tc-requests-table">
                <thead><tr>
                  <th scope="col">Request</th>
                  <th scope="col">Status</th>
                  <th scope="col" className="tc-requests-wide">Provider</th>
                  <th scope="col" className="tc-requests-wide">Created</th>
                  <th scope="col" className="tc-requests-cost">Est. cost</th>
                  <th scope="col"><span className="sr-only">Copy</span></th>
                </tr></thead>
                <tbody>
                  {rows.map((item) => {
                    const time = item.created_at ? <time dateTime={item.created_at}>{shortTime.format(new Date(item.created_at))}</time> : null;
                    const provider = providerLabel(item.provider);
                    const warning = history?.diagnostic === "warnings"
                      || (history?.diagnostic === "failed_or_warning" && item.status !== "failed");
                    return <tr key={item.id} data-selected={item.id === requestId}>
                      <td>
                        <button type="button" data-request-id={item.id} aria-current={item.id === requestId || undefined}
                          className="tc-requests-row-title" onClick={() => open(item.id)}>
                          <span className="min-w-0">
                            <span className="block truncate font-semibold" title={item.model}>{item.model}</span>
                            <span className="tc-requests-wide block truncate text-xs text-muted">
                              {[item.app_ref && `App ${item.app_ref}`, item.client_ref && `Client ${item.client_ref}`].filter(Boolean).join(" · ")}
                            </span>
                            <span className="tc-requests-narrow block truncate text-xs text-muted">{provider}{time && <> · {time}</>}</span>
                          </span>
                          <ChevronRight className="h-4 w-4 shrink-0 text-muted" aria-hidden />
                        </button>
                      </td>
                      <td>
                        <Badge className="text-xs" tone={statusTone[item.status]}>{statusNames[item.status]}</Badge>
                        {warning && <Badge className="ml-1 text-xs" tone="amber">Warning</Badge>}
                      </td>
                      <td className="tc-requests-wide truncate text-muted">{provider}</td>
                      <td className="tc-requests-wide text-xs text-muted">{time}</td>
                      <td className="tc-requests-cost text-right text-xs text-muted">{usd(item.estimated_usd_nanos)}</td>
                      <td><CopyRequestId id={item.id} /></td>
                    </tr>;
                  })}
                </tbody>
              </table>
            ) : !error && <div className="px-4 py-8 text-center text-sm">
              <p role="status">{loading ? "Loading history…"
                : chips.length ? "No matching requests" : "No requests yet"}</p>
              {!loading && chips.length > 0 && <p className="mt-1 text-xs text-muted">
                Adjust or clear the filters to widen your search.
              </p>}
            </div>}
          </div>
          <footer className="tc-requests-footer">
            <p className="mr-auto text-xs text-muted" aria-live="polite">
              {history ? `Page ${page} · ${rows.length} shown` : null}
            </p>
            <Button size="sm" type="button" variant="ghost" disabled={!hasNewer} onClick={requestHistory.newer}
              aria-label="Newer requests" title="Newer requests" className="px-2"><ChevronLeft className="h-4 w-4" aria-hidden /></Button>
            <Button size="sm" type="button" variant="ghost" disabled={!hasOlder} onClick={requestHistory.older}
              aria-label="Older requests" title="Older requests">Older <ChevronRight className="h-4 w-4" aria-hidden /></Button>
          </footer>
        </Card>
      </section>
      {requestId && (
        <section className="tc-requests-trace" aria-label="Request trace">
          <Card className="min-w-0 p-3 sm:p-5">
            <div className="mb-4 flex flex-wrap items-center gap-2">
              <Button type="button" variant="ghost" size="sm" className="-ml-2 px-2" onClick={onClose}>
                <ArrowLeft className="h-4 w-4" aria-hidden /> Back to history
              </Button>
              {collapsed && <Button type="button" variant="outline" size="sm" className="tc-requests-desktop ml-auto"
                aria-controls={historyId} aria-expanded={false} onClick={() => {
                  setCollapsed(false);
                  requestAnimationFrame(() => historyTitle.current?.focus({ preventScroll: true }));
                }}>
                <ChevronRight className="h-4 w-4" aria-hidden /> Show history
              </Button>}
              <Button type="button" variant="outline" size="sm" className="tc-requests-mobile ml-auto"
                aria-controls={historyId} aria-expanded={mobileHistory} onClick={() => {
                  setMobileHistory(true);
                  requestAnimationFrame(() => historyTitle.current?.focus({ preventScroll: true }));
                }}>
                <List className="h-4 w-4" aria-hidden /> Browse
              </Button>
              <Button type="button" variant="ghost" size="sm" aria-label="Copy trace link" title="Copy trace link"
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(window.location.href);
                    setCopyStatus("Link copied");
                  } catch {
                    setCopyStatus("Copy the link from the address bar");
                  }
                }} className="px-2"><Link2 className="h-4 w-4" aria-hidden /></Button>
              <span className="text-xs text-muted" role="status">{copyStatus}</span>
            </div>
            <h2 ref={traceTitle} tabIndex={-1} className="mb-3 flex items-center gap-2 text-lg font-semibold">
              <Radio className="h-4 w-4 text-brand" aria-hidden /> Request trace
            </h2>
            {trace}
          </Card>
        </section>
      )}
    </div>
  );
}
