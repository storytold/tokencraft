import { ChevronDown, ChevronUp, X } from "lucide-react";
import { Button, Select } from "@/components/ui";
import { providerLabel, type Provider } from "./api";

export type ProviderOrderValue = {
  mode: "inherit" | "cheapest" | "ordered";
  order: Provider[];
};

export function providerOrderValue(order?: Provider[] | null): ProviderOrderValue {
  return { mode: order == null ? "inherit" : order.length ? "ordered" : "cheapest", order: order ?? [] };
}

export function ProviderOrder({ value, onChange, inheritLabel, providers, label = "Route ranking" }: {
  value: ProviderOrderValue;
  onChange: (value: ProviderOrderValue) => void;
  inheritLabel: string;
  providers?: readonly Provider[];
  label?: string;
}) {
  const { mode, order } = value;
  const available = (providers ?? []).filter((provider) => !order.includes(provider));
  function changeOrder(next: Provider[]) {
    onChange({ ...value, order: next });
  }

  function move(index: number, target: number) {
    const next = [...order];
    const [provider] = next.splice(index, 1);
    next.splice(target, 0, provider);
    changeOrder(next);
  }

  return (
    <div className="space-y-2">
      <label className="block space-y-1 text-sm font-medium">{label}
        <Select value={mode} onChange={(event) => onChange({ ...value, mode: event.target.value as ProviderOrderValue["mode"] })}>
          <option value="inherit">{inheritLabel}</option>
          <option value="cheapest">Lowest estimated cost</option>
          {(available.length > 0 || order.length > 0) && <option value="ordered">Prioritize providers</option>}
        </Select>
      </label>
      {mode === "ordered" && <>
        {order.length > 0 && <ol className="divide-y divide-border rounded-md border border-border" aria-label="Preferred providers">
          {order.map((provider, index) => (
            <li key={provider} className="flex min-w-0 items-center justify-between gap-2 px-2 py-1 text-sm">
              <span>{providerLabel(provider)}</span>
              <div className="flex shrink-0 gap-1">
                <Button type="button" variant="ghost" size="sm" className="h-8 w-8 p-0"
                  aria-label={`Move ${providerLabel(provider)} earlier`} disabled={index === 0} onClick={() => move(index, index - 1)}>
                  <ChevronUp className="h-4 w-4" aria-hidden />
                </Button>
                <Button type="button" variant="ghost" size="sm" className="h-8 w-8 p-0"
                  aria-label={`Move ${providerLabel(provider)} later`} disabled={index === order.length - 1} onClick={() => move(index, index + 1)}>
                  <ChevronDown className="h-4 w-4" aria-hidden />
                </Button>
                <Button type="button" variant="ghost" size="sm" className="h-8 w-8 p-0"
                  aria-label={`Remove ${providerLabel(provider)}`} onClick={() => changeOrder(order.filter((item) => item !== provider))}>
                  <X className="h-4 w-4" aria-hidden />
                </Button>
              </div>
            </li>
          ))}
        </ol>}
        {available.length > 0 && <Select aria-label="Add preferred provider" value="" onChange={(event) => changeOrder([...order, event.target.value])}>
          <option value="" disabled>Add preferred provider</option>
          {available.map((provider) => <option key={provider} value={provider}>{providerLabel(provider)}</option>)}
        </Select>}
        <p className="text-xs text-muted">Eligible providers follow this order. Other eligible offers follow by estimated cost.</p>
      </>}
    </div>
  );
}
