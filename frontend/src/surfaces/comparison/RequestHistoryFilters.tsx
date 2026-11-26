import { useId, useState } from "react";
import { ChevronDown, Filter } from "lucide-react";
import { Button, Input, Select } from "@/components/ui";
import { isProviderId } from "@/lib/gateway";
import { dateRangeError, diagnosticNames, shiftDate, type InvestigationFilters } from "@/surfaces/requests/historyDiagnostics";
import { providerChoices, providerLabel, type CostState, type Provider, type Trace } from "./api";

export const statusNames: Record<Trace["status"], string> = {
  queued: "Queued",
  running: "Running",
  succeeded: "Succeeded",
  canceled: "Canceled",
  failed: "Failed",
  awaiting_reconciliation: "Outcome unconfirmed",
};

export const costStateNames: Record<CostState, string> = {
  estimated: "Estimate only",
  mock_reported: "Mock reported",
  provider_reported: "Provider reported",
  supplier_settled: "Supplier settled",
};

export function RequestHistoryFilters({ applied, boundClient, providers = [], onApply, embedded = false }: {
  applied: InvestigationFilters;
  boundClient?: string | null;
  providers?: readonly Provider[];
  onApply: (filters: InvestigationFilters) => void;
  embedded?: boolean;
}) {
  const id = useId();
  const [draft, setDraft] = useState<InvestigationFilters>(applied);
  const [error, setError] = useState("");
  const [expanded, setExpanded] = useState(() => Object.values(applied).some(Boolean));
  const active = Object.values(applied).filter(Boolean).length;

  function update<K extends keyof InvestigationFilters>(name: K, value: InvestigationFilters[K]) {
    setDraft((current) => ({ ...current, [name]: value || undefined }));
  }

  const form = (
    <form className={embedded ? "tc-request-filter-form" : "grid grid-cols-1 gap-3 border-t border-border p-3 min-[420px]:grid-cols-2"}
      onSubmit={(event) => {
        event.preventDefault();
        const provider = draft.provider?.trim() || undefined;
        const error = provider && !isProviderId(provider)
          ? "Enter a lowercase provider ID of up to 64 letters, digits, underscores, or hyphens."
          : dateRangeError(draft);
        setError(error);
        if (!error) onApply({ ...draft, provider });
      }}>
      {embedded && (
        <label className="space-y-1 text-xs font-medium">Diagnostic
          <Select value={draft.diagnostic ?? ""} onChange={(event) => update("diagnostic", event.target.value as InvestigationFilters["diagnostic"])}>
            <option value="">Any diagnostic</option>
            {Object.entries(diagnosticNames).map(([key, name]) => <option key={key} value={key}>{name}</option>)}
          </Select>
        </label>
      )}
      {!boundClient && (
        <div className="space-y-2 text-xs font-medium">
          <label className="block space-y-1">Filter by client reference
            <Input value={draft.client_ref ?? ""} maxLength={64} disabled={Boolean(draft.client_unattributed)}
              onChange={(event) => update("client_ref", event.target.value)} />
          </label>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={Boolean(draft.client_unattributed)} className="accent-brand"
              onChange={(event) => setDraft({ ...draft, client_ref: undefined, client_unattributed: event.target.checked ? "true" : undefined })} />
            Unattributed clients only
          </label>
        </div>
      )}
      <div className="space-y-2 text-xs font-medium">
        <label className="block space-y-1">Filter by app reference
          <Input value={draft.app_ref ?? ""} maxLength={64} disabled={Boolean(draft.app_unattributed)}
            onChange={(event) => update("app_ref", event.target.value)} />
        </label>
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={Boolean(draft.app_unattributed)} className="accent-brand"
            onChange={(event) => setDraft({ ...draft, app_ref: undefined, app_unattributed: event.target.checked ? "true" : undefined })} />
          Unattributed apps only
        </label>
      </div>
      <label className="space-y-1 text-xs font-medium">Trace ID
        <Input value={draft.trace_id ?? ""} maxLength={32} pattern="[0-9a-f]{32}" spellCheck={false}
          placeholder="Gateway or caller trace ID" className="font-mono"
          onChange={(event) => update("trace_id", event.target.value.trim())} />
      </label>
      <div className="space-y-2 text-xs font-medium">
        <label className="block space-y-1">Model ID
          <Input value={draft.model ?? ""} maxLength={64} onChange={(event) => {
            const model = event.target.value;
            setDraft({ ...draft, model: model || undefined, exact_model: model ? draft.exact_model : undefined });
          }} />
        </label>
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={Boolean(draft.exact_model)} className="accent-brand"
            disabled={!draft.model} onChange={(event) => update("exact_model", event.target.checked ? "true" : undefined)} />
          Exact model ID
        </label>
      </div>
      <div className="space-y-1 text-xs font-medium">
        <label htmlFor={`${id}-provider`}>Provider ID</label>
        <Input id={`${id}-provider`} list={`${id}-providers`} value={draft.provider ?? ""} maxLength={64}
          placeholder="All providers" autoCapitalize="none" spellCheck={false}
          onChange={(event) => update("provider", event.target.value)} />
        <datalist id={`${id}-providers`}>
          {providerChoices([...providers, applied.provider]).map((provider) =>
            <option key={provider} value={provider}>{providerLabel(provider)}</option>)}
        </datalist>
      </div>
      <div className="space-y-1 text-xs font-medium">
        <label htmlFor={`${id}-status`}>Request status</label>
        <Select id={`${id}-status`} value={draft.status ?? ""} onChange={(event) => update("status", event.target.value as Trace["status"])}>
          <option value="">All statuses</option>
          {Object.entries(statusNames).map(([key, name]) => <option key={key} value={key}>{name}</option>)}
        </Select>
      </div>
      <div className="space-y-1 text-xs font-medium">
        <label htmlFor={`${id}-cost_state`}>Cost evidence</label>
        <Select id={`${id}-cost_state`} value={draft.cost_state ?? ""} onChange={(event) => update("cost_state", event.target.value as CostState)}>
          <option value="">All cost states</option>
          {Object.entries(costStateNames).map(([key, name]) => <option key={key} value={key}>{name}</option>)}
        </Select>
      </div>
      <label className="space-y-1 text-xs font-medium">From (UTC)
        <Input type="date" value={draft.from ?? ""} required={Boolean(draft.to)} onChange={(event) => update("from", event.target.value)} />
      </label>
      <label className="space-y-1 text-xs font-medium">{embedded ? "Through (UTC)" : "Before (UTC)"}
        <Input type="date" value={embedded && draft.to ? shiftDate(draft.to, -1) : draft.to ?? ""} min={draft.from}
          required={Boolean(draft.from)} onChange={(event) => update("to", embedded && event.target.value ? shiftDate(event.target.value, 1) : event.target.value)} />
      </label>
      {error && <p className="col-span-full text-xs text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
      <div className="col-span-full flex flex-wrap gap-2">
        <Button type="submit" size="sm" variant="outline">Apply filters</Button>
        {Object.values(draft).some(Boolean) || active ? (
          <Button type="button" size="sm" variant="ghost" onClick={() => {
            setDraft({});
            setError("");
            onApply({});
          }}>Clear filters</Button>
        ) : null}
      </div>
    </form>
  );

  if (embedded) return form;
  return (
    <>
      {boundClient && <p className="mt-3 break-all text-sm text-muted">Client <code>{boundClient}</code></p>}
      <details open={expanded} onToggle={(event) => setExpanded(event.currentTarget.open)}
        className="group my-4 rounded-lg border border-border">
        <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg px-3 py-2.5 text-sm font-medium hover:bg-surface2 focus-visible:outline-brand [&::-webkit-details-marker]:hidden">
          <Filter className="h-4 w-4 text-muted" aria-hidden />
          Filters {active > 0 && <span className="text-xs text-muted">({active} active)</span>}
          <ChevronDown className="ml-auto h-4 w-4 text-muted group-open:rotate-180" aria-hidden />
        </summary>
        {form}
      </details>
    </>
  );
}
