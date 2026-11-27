import { useMemo, useRef, useState, type FormEvent } from "react";
import { Button, Card, Input, Select } from "@/components/ui";
import { downloadBlob } from "@/lib/download";
import { providerLabel } from "@/surfaces/comparison/api";
import type { Modality } from "@/surfaces/requests/modelOptions";
import { canPrepare, filterReferenceOffers, referenceOfferPageSize, type ReferenceCatalog, type ReferenceOffer } from "./referenceCatalog";
import { bindingTemplates, catalogIdentifier, referencePlugin } from "./referencePlugins";

const rateFields = {
  image: [["usd_nanos_per_image", "USD per image"]],
  video: [["usd_nanos_per_second", "USD per video second"]],
  audio: [["usd_nanos_per_second", "USD per audio second"]],
  mesh: [["usd_nanos_per_mesh", "USD per mesh"]],
  text: [
    ["usd_nanos_per_1000_input_tokens", "USD per 1,000 input tokens"],
    ["usd_nanos_per_1000_output_tokens", "USD per 1,000 output tokens"],
  ],
} as const satisfies Record<Modality, readonly (readonly [string, string])[]>;
const field = "block min-w-0 space-y-1.5 text-sm font-medium";

type Draft = {
  row: number;
  offer: ReferenceOffer;
  binding: string;
  id: string;
  model: string;
  rates: Record<string, string>;
};

function nanoUsd(value: string) {
  const amount = value.trim();
  if (!/^(?:(?:0|1)(?:\.\d{1,9})?|\.\d{1,9})$/.test(amount)) return undefined;
  const [whole, fraction = ""] = amount.split(".");
  const nanos = Number(whole) * 1_000_000_000 + Number(fraction.padEnd(9, "0"));
  return nanos <= 1_000_000_000 ? nanos : undefined;
}

function selectionOffers(drafts: Draft[]) {
  const ids = new Set<string>();
  return drafts.map(({ offer, binding, id, model, rates: input }) => {
    const localBinding = binding.trim();
    const localId = id.trim();
    const localModel = model.trim();
    if (!catalogIdentifier.test(localBinding) || (localId && !catalogIdentifier.test(localId))
      || (localModel && (!catalogIdentifier.test(localModel) || localModel === "auto" || localModel === "advise"))) {
      throw new Error(`Check the binding and local IDs for ${offer.name}. Use up to 64 letters, digits, dots, underscores, hyphens, or colons. Start with a letter or digit. “auto” and “advise” are reserved model IDs.`);
    }
    const effectiveId = localId || offer.id;
    if (ids.has(effectiveId)) throw new Error(`Choose a distinct offer ID for “${effectiveId}”.`);
    ids.add(effectiveId);
    const rates = Object.fromEntries(rateFields[offer.kind].map(([key, label]) => {
      const value = nanoUsd(input[key] ?? "");
      if (value === undefined) throw new Error(`Enter ${label} for ${offer.name} from $0 to $1 with up to nine decimal places.`);
      return [key, value];
    }));
    return { reference: offer.id, binding: localBinding, id: localId || undefined, model: localModel || undefined, rates };
  });
}

export function CatalogSelection({ catalog, checksum }: { catalog: ReferenceCatalog; checksum?: string }) {
  const nextRow = useRef(0);
  const [search, setSearch] = useState("");
  const [chosen, setChosen] = useState("");
  const [drafts, setDrafts] = useState<Draft[]>([]);
  const [error, setError] = useState("");
  const available = useMemo(() => catalog.offers.filter(canPrepare), [catalog.offers]);
  const matching = useMemo(() => filterReferenceOffers(available, search), [available, search]);
  const choices = matching.slice(0, referenceOfferPageSize);
  const protocols = new Set(drafts.map(({ offer }) => referencePlugin(offer.plugin)?.protocol));

  function add() {
    const offer = choices.find((item) => item.id === chosen);
    if (!offer || drafts.length >= 64) return;
    const row = nextRow.current++;
    setDrafts((items) => [...items, { row, offer, binding: "", id: "", model: "", rates: {} }]);
    setChosen("");
    setError("");
  }

  function update(next: Draft) {
    setDrafts((items) => items.map((draft) => draft.row === next.row ? next : draft));
    setError("");
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!checksum || !drafts.length) return;
    try {
      const selection = {
        version: 1, reference_revision: catalog.revision,
        reference_sha256: checksum, offers: selectionOffers(drafts),
      };
      downloadBlob(new Blob([JSON.stringify(selection, null, 2) + "\n"], { type: "application/json" }), "selection.json");
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Unable to prepare this selection.");
    }
  }

  function downloadBindings(protocol: "queue" | "chat") {
    try {
      const templates = bindingTemplates(drafts.map(({ binding, offer }) => ({ binding, plugin: offer.plugin, name: offer.name })));
      const name = protocol === "queue" ? "connector-bindings.json" : "chat-bindings.json";
      downloadBlob(new Blob([JSON.stringify(templates[protocol], null, 2) + "\n"], { type: "application/json" }), name);
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Unable to prepare binding templates.");
    }
  }

  return (
    <Card className="min-w-0 space-y-4 p-4 sm:p-5">
      <h3 className="font-display text-xl font-semibold">Prepare self-hosted offers</h3>
      {!checksum ? <p className="text-sm text-muted">Open Pricing over HTTPS or localhost to pin the catalog checksum.</p>
        : !available.length ? <p className="text-sm text-muted">This snapshot has no offers with importable presets.</p>
        : <form className="space-y-4" onSubmit={save}>
          <p className="text-sm text-muted">Choose offers, name your local bindings, and enter the USD rates your gateway should use.</p>
          {available.length > referenceOfferPageSize && <label className={field}>
            <span>Search importable offers</span>
            <Input type="search" value={search} onChange={(event) => {
              setSearch(event.target.value);
              setChosen("");
            }} />
          </label>}
          <div className="flex flex-wrap items-end gap-2">
            <label className={`${field} w-full sm:w-auto sm:max-w-sm sm:flex-1`}>
              <span>Offer to add</span>
              <Select value={chosen} disabled={!choices.length} onChange={(event) => setChosen(event.target.value)}>
                <option value="">{choices.length ? "Choose an offer" : "No matching offers"}</option>
                {choices.map((offer) => <option key={offer.id} value={offer.id}>{offer.name} · {providerLabel(offer.plugin)}</option>)}
              </Select>
            </label>
            <Button type="button" variant="outline" onClick={add} disabled={!chosen || drafts.length >= 64}>Add offer</Button>
          </div>
          {matching.length > referenceOfferPageSize &&
            <p className="text-xs text-muted" role="status">Showing {choices.length} of {matching.length} importable offers. Refine your search to find more.</p>}
          {drafts.length >= 64 && <p className="text-sm text-muted">The selection has reached the gateway's 64-offer limit.</p>}
          {drafts.map((draft) => <fieldset key={draft.row} className="min-w-0 space-y-3 rounded-lg border border-border p-3 sm:p-4">
            <legend className="max-w-full break-words px-2 font-medium">{draft.offer.name}</legend>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-xs text-muted">{providerLabel(draft.offer.plugin)} · {draft.offer.preset}</p>
              <Button variant="ghost" size="sm" type="button" aria-label={`Remove ${draft.offer.name}`} onClick={() => {
                setDrafts((items) => items.filter((item) => item.row !== draft.row));
                setError("");
              }}>Remove</Button>
            </div>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              <label className={field}><span>Connector binding</span>
                <Input required maxLength={64} placeholder={`my-${draft.offer.plugin}`} value={draft.binding}
                  onChange={(event) => update({ ...draft, binding: event.target.value })} />
              </label>
              <label className={field}><span>Local offer ID <span className="text-muted">(optional)</span></span>
                <Input maxLength={64} placeholder={draft.offer.id} value={draft.id}
                  onChange={(event) => update({ ...draft, id: event.target.value })} />
              </label>
              <label className={field}><span>Local model ID <span className="text-muted">(optional)</span></span>
                <Input maxLength={64} placeholder={draft.offer.model} value={draft.model}
                  onChange={(event) => update({ ...draft, model: event.target.value })} />
              </label>
              {rateFields[draft.offer.kind].map(([key, label]) => <label key={key} className={field}><span>{label}</span>
                <Input required inputMode="decimal" autoComplete="off" value={draft.rates[key] ?? ""}
                  onChange={(event) => update({ ...draft, rates: { ...draft.rates, [key]: event.target.value } })} />
              </label>)}
            </div>
          </fieldset>)}
          {error && <p role="alert" className="text-sm text-rose-700 dark:text-rose-300">{error}</p>}
          {drafts.length > 0 && <div className="space-y-3">
            <Button type="submit">Download selection</Button>
            <p className="text-sm text-muted">Use the selection with the catalog JSON downloaded from this page.</p>
            <pre className="whitespace-pre-wrap break-words rounded-lg bg-surface2 p-3 text-xs"><code>{"api-gateway catalog prepare --reference artcraft-v1.json --selection selection.json > catalog.next.json"}</code></pre>
            <p className="text-sm text-muted">Pass <code>--base catalog.json</code> to keep existing offers.</p>
            <details className="rounded-lg border border-border p-3 sm:p-4">
              <summary className="cursor-pointer text-sm font-semibold">Plugin binding templates</summary>
              <p className="mt-2 text-sm text-muted">Start from the selected plugins' localhost defaults. Review the endpoints and merge the entries into any existing binding files.</p>
              <div className="mt-3 flex flex-wrap gap-2">
                {protocols.has("queue") && <Button type="button" variant="outline" size="sm" onClick={() => downloadBindings("queue")}>Download queue bindings</Button>}
                {protocols.has("chat") && <Button type="button" variant="outline" size="sm" onClick={() => downloadBindings("chat")}>Download chat bindings</Button>}
              </div>
            </details>
          </div>}
        </form>}
    </Card>
  );
}
