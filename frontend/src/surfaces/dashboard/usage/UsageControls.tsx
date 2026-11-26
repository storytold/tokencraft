import { useState, type FormEvent } from "react";
import { CalendarDays, ChevronDown, SlidersHorizontal, X } from "lucide-react";
import { ChoicePicker } from "@/components/ChoicePicker";
import { Button, Card, Input, Select } from "@/components/ui";
import { isProviderId } from "@/lib/gateway";
import { providerLabel } from "@/surfaces/comparison/api";
import { shiftDate } from "@/surfaces/requests/historyDiagnostics";
import { customWindow, periods, recentWindow, usageModelName, windowLabel, type UsageSelection } from "./usageData";

const fieldClass = "block min-w-0 space-y-1.5 text-sm font-medium";

export function UsageControls({ query, boundClient, models, providers, onChange }: {
  query: UsageSelection; boundClient: string | null; models: string[]; providers: string[];
  onChange: (query: UsageSelection) => void;
}) {
  const [showDates, setShowDates] = useState(false);
  const [filterError, setFilterError] = useState("");
  const activePeriod = periods.find((days) => {
    const { from, to } = recentWindow(days);
    return query.from === from && query.to === to;
  });
  const applied = [
    query.model && { label: usageModelName(query.model), field: "model" as const },
    query.provider && { label: providerLabel(query.provider), field: "provider" as const },
    query.client_ref && !boundClient && { label: `Client · ${query.client_ref}`, field: "client_ref" as const },
    query.app_ref && { label: `App · ${query.app_ref}`, field: "app_ref" as const },
  ].filter((item) => item !== "" && item !== false);

  return (
    <Card className="p-4 sm:p-5">
      <div className="grid items-end gap-4 sm:grid-cols-2 xl:grid-cols-[minmax(17rem,1.25fr)_minmax(0,1fr)_minmax(0,0.75fr)] [&_.choice-picker-trigger]:min-h-10 [&_.choice-picker-trigger]:rounded-lg [&_.choice-picker-trigger]:py-1">
        <div className="min-w-0 space-y-1.5 sm:col-span-2 xl:col-span-1">
          <div className="flex flex-wrap items-center justify-between gap-x-2 gap-y-1">
            <span className="flex items-center gap-2 text-sm font-medium"><CalendarDays className="h-4 w-4 text-muted" aria-hidden /> Date range</span>
            <span className="text-xs text-muted">{windowLabel(query)} · UTC</span>
          </div>
          <div role="group" aria-label="Date range" className="grid grid-cols-4 gap-0.5 rounded-lg border border-border bg-bg p-0.5">
            {periods.map((days) => (
              <button key={days} type="button" aria-pressed={activePeriod === days && !showDates}
                onClick={() => { setShowDates(false); onChange({ ...query, ...recentWindow(days) }); }}
                className={periodClass}>{days} days</button>
            ))}
            <button type="button" aria-expanded={showDates} aria-pressed={showDates || activePeriod === undefined}
              onClick={() => setShowDates(!showDates)} className={periodClass}>Custom</button>
          </div>
        </div>
        <ChoicePicker label="Model" value={query.model} options={[
          { value: "", label: "All models" },
          ...models.map((model) => {
            const label = usageModelName(model);
            return { value: model, label, detail: label !== model ? model : undefined };
          }),
        ]} onChange={(model) => onChange({ ...query, model })} />
        <label className={fieldClass}>Provider
          <Select className="rounded-lg" value={query.provider} onChange={(event) => onChange({ ...query, provider: event.target.value })}>
            <option value="">All providers</option>
            {providers.map((provider) => <option key={provider} value={provider}>{providerLabel(provider)}</option>)}
          </Select>
        </label>
      </div>

      {showDates && <CustomDates key={`${query.from}/${query.to}`} query={query} onApply={(window) => {
        onChange({ ...query, ...window });
        setShowDates(false);
      }} />}

      <div className="mt-4 flex flex-wrap items-center gap-2 border-t border-border pt-3">
        {boundClient && <span className="max-w-full break-all rounded-md bg-surface2 px-2.5 py-1 text-xs text-muted">Client scope · {boundClient}</span>}
        {applied.map(({ label, field }) => <button type="button" key={field} onClick={() => onChange({ ...query, [field]: "" })}
          aria-label={`Remove ${label} filter`} className="inline-flex max-w-full items-center gap-1.5 rounded-md border border-border px-2.5 py-1 text-left text-xs font-medium hover:border-brand/50 focus-visible:outline-brand">
          <span className="break-all">{label}</span><X className="h-3 w-3 shrink-0 text-muted" aria-hidden />
        </button>)}
        {applied.length > 1 && <Button type="button" variant="ghost" size="sm" className="h-7 px-2 text-xs"
          onClick={() => onChange({ ...query, provider: "", model: "", client_ref: "", app_ref: "" })}>Clear filters</Button>}
        <details className="group w-full">
          <summary className="inline-flex cursor-pointer list-none items-center gap-2 rounded-md py-1 pr-2 text-sm font-medium text-muted hover:text-fg focus-visible:outline-brand [&::-webkit-details-marker]:hidden">
            <SlidersHorizontal className="h-4 w-4" aria-hidden /> More filters
            <ChevronDown className="h-3.5 w-3.5 group-open:rotate-180" aria-hidden />
          </summary>
          <form key={JSON.stringify([query.model, query.provider, query.client_ref, query.app_ref])}
            className="mt-3 grid items-end gap-3 sm:grid-cols-2 xl:grid-cols-4" onSubmit={(event) => {
            event.preventDefault();
            const fields = new FormData(event.currentTarget);
            const provider = String(fields.get("provider")).trim();
            if (provider && !isProviderId(provider)) {
              setFilterError("Enter a lowercase provider ID of up to 64 letters, digits, underscores, or hyphens.");
              return;
            }
            setFilterError("");
            onChange({ ...query, provider, model: String(fields.get("model")).trim(),
              client_ref: boundClient ? "" : String(fields.get("client")).trim(), app_ref: String(fields.get("app")).trim() });
          }}>
            <label className={fieldClass}>Model ID
              <Input name="model" defaultValue={query.model} maxLength={64} placeholder="All models" />
            </label>
            <label className={fieldClass}>Provider ID
              <Input name="provider" defaultValue={query.provider} maxLength={64} placeholder="All providers"
                autoCapitalize="none" spellCheck={false} onChange={() => setFilterError("")} />
            </label>
            <label className={fieldClass}>Client reference
              <Input name="client" defaultValue={boundClient ?? query.client_ref} readOnly={Boolean(boundClient)} maxLength={64} placeholder="All clients" />
            </label>
            <label className={fieldClass}>App reference
              <Input name="app" defaultValue={query.app_ref} maxLength={64} placeholder="All apps" />
            </label>
            <Button type="submit" variant="outline" className="justify-self-start rounded-lg">Apply filters</Button>
            {filterError && <p role="alert" className="text-sm text-rose-500 sm:col-span-2 xl:col-span-4">{filterError}</p>}
          </form>
        </details>
      </div>
    </Card>
  );
}

const periodClass = "min-h-9 rounded-md px-1 text-xs font-semibold text-muted hover:bg-surface2 hover:text-fg focus-visible:outline-brand aria-pressed:bg-brand/10 aria-pressed:text-brand sm:text-sm";

function CustomDates({ query, onApply }: {
  query: UsageSelection; onApply: (window: Pick<UsageSelection, "from" | "to">) => void;
}) {
  const [error, setError] = useState("");
  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const fields = new FormData(event.currentTarget);
    const selection = customWindow(String(fields.get("from")), String(fields.get("through")));
    setError(selection.error);
    if (!selection.error) onApply(selection.window);
  }
  return (
    <form onSubmit={submit} className="mt-4 rounded-xl border border-border bg-surface2/50 p-3 sm:p-4">
      <div className="grid items-end gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
        <label className={fieldClass}>Start date (UTC)
          <Input name="from" type="date" required className={dateInputClass} defaultValue={query.from} />
        </label>
        <label className={fieldClass}>Through date (UTC)
          <Input name="through" type="date" required className={dateInputClass} defaultValue={shiftDate(query.to, -1)} />
        </label>
        <Button type="submit" className="justify-self-start rounded-lg">Apply dates</Button>
      </div>
      {error && <p className="mt-2 text-sm text-rose-700 dark:text-rose-300" role="alert">{error}</p>}
    </form>
  );
}

const dateInputClass = "min-w-0 dark:[color-scheme:dark] [&::-webkit-calendar-picker-indicator]:cursor-pointer";
