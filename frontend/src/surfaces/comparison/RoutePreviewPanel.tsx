import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { GitCompareArrows, RefreshCw } from "lucide-react";
import { Button, Input } from "@/components/ui";
import type { ProjectScope } from "@/lib/gateway";
import { parseDollarCap } from "@/lib/routing-policy";
import { comparisonApi, errorMessage, providerLabel, usd, type Quote, type RoutePreview } from "./api";
import { OfferComparison } from "./OfferComparison";

const observedTime = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });

export function RoutePreviewPanel({ scope, requestId }: { scope: ProjectScope; requestId: string }) {
  const heading = useId();
  const request = useRef<AbortController | null>(null);
  const [result, setResult] = useState<RoutePreview>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => () => request.current?.abort(), []);

  async function preview(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (request.current) return;
    const form = new FormData(event.currentTarget);
    const cap = parseDollarCap(String(form.get("cost-cap") ?? ""));
    if (cap === undefined) {
      setError("Enter a cost from $0 to $1,000 with up to nine decimal places.");
      return;
    }
    const controller = new AbortController();
    request.current = controller;
    setPending(true);
    setError("");
    setResult(undefined);
    try {
      const options = {
        max_total_usd_nanos: cap ?? undefined,
        preferred_providers: form.has("estimated-price") ? [] : undefined,
      };
      setResult(await comparisonApi.previewRoute(scope, requestId, options, controller.signal));
    } catch (cause) {
      if (!controller.signal.aborted) setError(errorMessage(cause));
    } finally {
      request.current = null;
      if (!controller.signal.aborted) setPending(false);
    }
  }

  return (
    <section aria-labelledby={heading} className="mb-3 min-w-0 border-b border-border pb-3">
      <form onSubmit={preview} className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h4 id={heading} className="flex items-center gap-2 text-sm font-bold">
              <GitCompareArrows className="h-4 w-4 text-brand" aria-hidden /> Routing preview
            </h4>
            <p className="mt-1 text-xs text-muted">Compare the saved route with a current quote.</p>
          </div>
          <Button type="submit" size="sm" variant="outline" disabled={pending}>
            {result && <RefreshCw className="h-3.5 w-3.5" aria-hidden />}
            {result ? "Refresh preview" : "Preview routing"}
          </Button>
        </div>
        <details className="text-xs">
          <summary className="cursor-pointer text-muted">Preview settings</summary>
          <fieldset disabled={pending} className="mt-2 flex flex-wrap items-end gap-3">
            <label className="min-w-0 text-xs font-medium">Cost cap (USD)
              <Input name="cost-cap" className="mt-1" inputMode="decimal" maxLength={15} placeholder="Saved cap" />
            </label>
            <label className="flex items-center gap-2 pb-2 text-xs">
              <input name="estimated-price" type="checkbox" className="h-4 w-4 accent-brand" />
              Use estimated price order
            </label>
          </fieldset>
        </details>
      </form>
      <div aria-live="polite" aria-busy={pending}>
        {pending && <p className="mt-3 text-xs text-muted" role="status">Calculating current quote…</p>}
        {error && <p className="mt-3 text-xs text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
        {result && <PreviewResult result={result} />}
      </div>
    </section>
  );
}

function PreviewResult({ result }: { result: RoutePreview }) {
  const { recorded, current } = result;
  const outcome = current?.selected_offer_id
    ? current.selected_offer_id === recorded.selected_offer_id ? "Same selected offer" : "Selected offer changed"
    : "Current quote has no eligible offer";
  return (
    <div className="mt-3 space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-xs font-semibold">{outcome}</p>
        <p className="text-xs text-muted">Checked <time dateTime={result.observed_at}>{observedTime.format(new Date(result.observed_at))}</time></p>
      </div>
      <div className="grid min-w-0 gap-2 sm:grid-cols-2">
        <Decision title="At admission" quote={recorded} />
        <Decision title="Current quote" quote={current} />
      </div>
      {!recorded.profile && <p className="text-xs text-muted">This older request uses the native offer profile for preview.</p>}
      {result.unavailable_reason && <p className="text-xs text-muted">
        {result.unavailable_reason === "pinned_offer_unavailable" ? "The saved offer pin is unavailable in the current catalog." : "The current catalog has no offer for this workload."}
      </p>}
      {current && <details className="min-w-0">
        <summary className="cursor-pointer text-xs font-medium text-brand">Current eligible offers and exclusions</summary>
        <div className="mt-2 overflow-x-auto"><OfferComparison quote={current} loading={false} /></div>
      </details>}
    </div>
  );
}

function Decision({ title, quote }: { title: string; quote: Quote | null }) {
  const selected = quote?.candidates.find(candidate => candidate.offer_id === quote.selected_offer_id);
  const renderedShape = selected?.output_size
    ? `${selected.output_size.width} × ${selected.output_size.height} px`
    : selected?.applied_aspect_ratio;
  return (
    <div className="min-w-0 space-y-2 rounded-md border border-border bg-surface2/50 p-3">
      <h5 className="text-xs font-semibold">{title}</h5>
      {selected && <p className="break-words text-sm font-semibold">
        {providerLabel(selected.provider)} <code className="break-all text-xs font-normal text-muted">{selected.offer_id}</code>
      </p>}
      {quote && <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
        <dt className="text-muted">Estimate</dt><dd className="font-mono">{usd(selected?.estimated_usd_nanos ?? null)}</dd>
        {renderedShape && <><dt className="text-muted">Output</dt><dd className="font-mono">{renderedShape}</dd></>}
        <dt className="text-muted">Policy</dt>
        <dd className="font-mono">Org {quote.routing?.organization_revision ?? 0} · Project {quote.routing?.project_revision ?? 0}</dd>
        <dt className="text-muted">Catalog</dt><dd className="truncate font-mono" title={quote.catalog_revision}>{quote.catalog_revision.slice(0, 12)}</dd>
      </dl>}
      {!quote && <p className="text-xs text-muted">Unavailable</p>}
    </div>
  );
}
