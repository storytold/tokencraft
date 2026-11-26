import { useEffect, useRef, useState, type FormEvent, type ReactNode } from "react";
import { Download, Plus, RefreshCw } from "lucide-react";
import { Badge, Button, Card, Input } from "@/components/ui";
import { downloadBlob } from "@/lib/download";
import { errorMessage, isGatewayEndpointUnavailable } from "@/lib/gateway";
import { comparisonApi, type Catalog } from "@/surfaces/comparison/api";
import type { Project } from "../api";
import { ConnectionSharing, RoutingHistory, providerTitle } from "./ProviderAccess";
import { ProjectDisclosure } from "../ProjectDisclosure";
import { ConnectorProviderKeys } from "../ProjectProviderKeys";
import { Field, RateInputs } from "./fields";
import { ProviderEditor } from "./ProviderEditor";
import {
  kindLabel, providerOnboarding, rateUnit, readRates, usdNanos,
  type Change, type Inventory, type Offer, type Preview, type Pricing, type ProviderScope,
} from "./provider-onboarding";


type Editor = { kind: "connect" } | { kind: "models" | "price"; offer: Offer };
type ReviewedChange = { preview: Preview; evidence: unknown[] };

export function ProviderManagement({ scope, projects, children }: { scope: ProviderScope; projects: Project[]; children?: ReactNode }) {
  const { account_id } = scope;
  const [sharing, setSharing] = useState<Offer>();
  const [notice, setNotice] = useState("");
  const [catalog, setCatalog] = useState<Catalog>();
  const [inventory, setInventory] = useState<Inventory>();
  const [refresh, setRefresh] = useState(0);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [filter, setFilter] = useState("");
  const [editor, setEditor] = useState<Editor>();
  const [review, setReview] = useState<ReviewedChange>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const request = useRef<AbortController | undefined>(undefined);
  const editorSection = useRef<HTMLDivElement>(null);
  const reviewSection = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const controller = new AbortController();
    async function load() {
      try {
        const result = await providerOnboarding.inventory({ account_id }, controller.signal);
        if (controller.signal.aborted) return;
        setInventory(result);
        setCatalog(undefined);
      } catch (failure) {
        if (controller.signal.aborted) return;
        setInventory(undefined);
        if (isGatewayEndpointUnavailable(failure)) {
          try {
            const result = await comparisonApi.catalog(controller.signal);
            if (!controller.signal.aborted) setCatalog(result);
          } catch (error) {
            if (!controller.signal.aborted) setLoadError(errorMessage(error));
          }
        } else {
          setLoadError(errorMessage(failure));
        }
      } finally {
        if (!controller.signal.aborted) setLoading(false);
      }
    }
    void load();
    return () => controller.abort();
  }, [account_id, refresh]);

  useEffect(() => () => request.current?.abort(), []);
  useEffect(() => { if (editor) editorSection.current?.focus(); }, [editor]);
  useEffect(() => { if (review) reviewSection.current?.focus(); }, [review]);

  function clearPreview() {
    setReview(undefined);
    setError("");
  }

  function selectEditor(next?: Editor) {
    request.current?.abort();
    setBusy(false);
    clearPreview();
    setEditor(next);
  }

  async function preview(change: Change, evidence: unknown[], projects: string[] | null = null) {
    if (!inventory || busy) return;
    const controller = new AbortController();
    request.current = controller;
    clearPreview();
    setBusy(true);
    try {
      const result = await providerOnboarding.preview(scope, inventory.base_revision, change, projects, controller.signal);
      if (!controller.signal.aborted) setReview({ preview: result, evidence });
    } catch (failure) {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  function reload() {
    setLoading(true);
    setLoadError("");
    clearPreview();
    setEditor(undefined);
    setSharing(undefined);
    setRefresh((value) => value + 1);
  }

  const canFilter = (inventory?.offers.length ?? catalog?.offers.length ?? 0) > 6;
  const query = canFilter ? filter.trim().toLowerCase() : "";
  const publicOffers = catalog?.offers.filter((offer) => [offer.id, offer.model,
    providerTitle(offer),
  ].some((value) => value.toLowerCase().includes(query)));
  const offers = inventory?.offers.filter((offer) => [
    offer.id, offer.model, providerTitle(offer), offer.binding, kindLabel[offer.pricing.kind],
  ].some((value) => value?.toLowerCase().includes(query))) ?? [];

  const firstOfferByBinding = new Map<string, string>();
  for (const offer of offers) {
    if (offer.binding && !firstOfferByBinding.has(offer.binding)) {
      firstOfferByBinding.set(offer.binding, offer.id);
    }
  }

  const firstOfferByConnection = new Map<string, string>();
  for (const offer of offers) {
    if (!firstOfferByConnection.has(offer.connection_id)) firstOfferByConnection.set(offer.connection_id, offer.id);
  }

  return (
    <div className="space-y-3">
      <Card className="p-4 sm:p-5">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h2 className="font-display text-xl font-semibold">Connections and models</h2>
            <p className="mt-1 text-sm text-muted">Connect plugins and custom providers, review their models, and manage project sharing.</p>
          </div>
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" size="sm" disabled={loading || busy} onClick={reload}>
              <RefreshCw size={15} aria-hidden="true" />Refresh
            </Button>
            {inventory && <Button type="button" size="sm" disabled={loading || busy} onClick={() => selectEditor({ kind: "connect" })}>
              <Plus size={16} aria-hidden="true" />Connect provider
            </Button>}
          </div>
        </div>
        {notice && <p role="status" className="mt-3 text-sm text-muted">{notice}</p>}
        {loading && <p className="mt-3 text-sm text-muted" role="status">Reading provider offers…</p>}
        {loadError && <p className="mt-3 text-sm text-rose-600 dark:text-rose-300" role="alert">{loadError}</p>}
        {publicOffers && (
          <div className="mt-4 space-y-3">
            <p className="text-sm text-muted">Connector setup requires a gateway with provider onboarding support.</p>
            {canFilter && <Field label="Filter offers">
              <Input type="search" placeholder="Provider or model" value={filter}
                onChange={(event) => setFilter(event.target.value)} />
            </Field>}
            {publicOffers.length ? (
              <ul className="max-h-64 divide-y divide-border overflow-y-auto rounded-lg border border-border">
                {publicOffers.map((offer) => (
                  <li key={offer.id} className="flex flex-wrap items-center justify-between gap-2 p-3 text-sm">
                    <div className="min-w-0">
                      <p className="break-all font-semibold">{offer.model}</p>
                      <p className="text-muted">{providerTitle(offer)}</p>
                    </div>
                    <Badge>{kindLabel[offer.pricing.kind]}</Badge>
                  </li>
                ))}
              </ul>
            ) : <p className="text-sm text-muted">{query ? "No offers match your filter." : "This gateway has no active offers."}</p>}
          </div>
        )}
        {inventory && (
          <div className="mt-4 space-y-3">
            {inventory.active_revision !== inventory.base_revision && <p className="text-sm text-muted">
              A verified rate feed is active. Editing a rate prepares a change to the reviewed catalog.
            </p>}
            {canFilter && <Field label="Filter offers">
              <Input type="search" placeholder="Provider, model, binding, or output" value={filter}
                onChange={(event) => setFilter(event.target.value)} />
            </Field>}
            {offers.length ? (
              <ul className="max-h-[28rem] divide-y divide-border overflow-y-auto rounded-lg border border-border">
                {offers.map((offer) => (
                  <li key={offer.id} className="flex flex-wrap items-center justify-between gap-2 p-3 text-sm">
                    <div className="min-w-0 flex-[1_1_13rem]">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="min-w-0 break-all font-semibold">{offer.model}</span>
                        <Badge>{kindLabel[offer.pricing.kind]}</Badge>
                        {!offer.allowed_by_policy && <Badge tone="amber">Blocked by organization allowlist</Badge>}
                      </div>
                      <p className="break-words text-muted">{providerTitle(offer)}{offer.binding && ` · ${offer.binding}`}</p>
                      <p className="break-words"><Rate price={offer.pricing} /></p>
                      {firstOfferByConnection.get(offer.connection_id) === offer.id && inventory.policy_revision !== undefined &&
                        <p className="text-xs text-muted">{offer.selected_projects
                          ? `Shared with ${offer.selected_projects.length} selected ${offer.selected_projects.length === 1 ? "project" : "projects"}`
                          : "Shared with all projects"}</p>}
                    </div>
                    <div className="flex flex-wrap gap-1">
                      {firstOfferByConnection.get(offer.connection_id) === offer.id && inventory.policy_revision !== undefined &&
                        <Button type="button" variant="ghost" size="sm" disabled={busy} aria-label={`Share ${offer.binding ?? providerTitle(offer)} with projects`}
                          onClick={() => setSharing(offer)}>Sharing</Button>}
                      {offer.binding && firstOfferByBinding.get(offer.binding) === offer.id && <Button type="button" variant="ghost" size="sm" disabled={busy}
                        aria-label={`Add models to ${offer.binding}`} onClick={() => selectEditor({ kind: "models", offer })}>Add models</Button>}
                      <Button type="button" variant="outline" size="sm" disabled={busy}
                        aria-label={`Edit price for ${offer.id}`} onClick={() => selectEditor({ kind: "price", offer })}>Edit price</Button>
                    </div>
                  </li>
                ))}
              </ul>
            ) : <p className="text-sm text-muted">{query ? "No offers match your filter." : "This gateway has no active offers."}</p>}
          </div>
        )}
      </Card>
      {sharing && inventory && <ConnectionSharing key={sharing.connection_id} accountId={account_id}
        revision={inventory.policy_revision} offer={sharing} projects={projects} onCancel={() => setSharing(undefined)}
        onSaved={() => { setNotice("Project sharing saved."); reload(); }} />}
      {editor && inventory && (
        <div ref={editorSection} tabIndex={-1} className="outline-none">
          {editor.kind === "price" ? (
            <PriceEditor key={editor.offer.id} offer={editor.offer} busy={busy}
              onPreview={(change) => void preview(change, [])} onCancel={() => selectEditor()} clearPreview={clearPreview} />
          ) : (
            <ProviderEditor key={editor.kind === "models" ? editor.offer.binding : "connect"} scope={scope} projects={projects}
              existing={editor.kind === "models" ? editor.offer : undefined}
              discoveryEnabled={inventory.discovery} busy={busy} onPreview={(change, evidence, projects) => void preview(change, evidence, projects)}
              onCancel={() => selectEditor()} clearPreview={clearPreview} />
          )}
        </div>
      )}
      {error && <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
      {review && (
        <div ref={reviewSection} tabIndex={-1} className="outline-none">
          <ChangeReview review={review} />
        </div>
      )}
      {inventory?.credential_bindings?.length ? (
        <ProjectDisclosure title="Provider credentials" description="Organization defaults" unmountOnClose>
          <ConnectorProviderKeys scope={scope} bindings={inventory.credential_bindings} refresh={refresh} />
        </ProjectDisclosure>
      ) : inventory?.provider_key_management && children}
      <ProjectDisclosure title="Organization routing change history" unmountOnClose>
        <RoutingHistory scope={scope} refresh={inventory?.policy_revision} />
      </ProjectDisclosure>
    </div>
  );
}

function Rate({ price }: { price: Pricing }) {
  const rates = readRates(price);
  return <>{
    price.kind === "text"
      ? `$${rates.amount} input · $${rates.output} output per 1M tokens`
      : `$${rates.amount} / ${rateUnit(price.kind)}`
  }</>;
}

function PriceEditor({ offer, busy, onPreview, onCancel, clearPreview }: {
  offer: Offer;
  busy: boolean;
  onPreview: (change: Change) => void;
  onCancel: () => void;
  clearPreview: () => void;
}) {
  const [rates, setRates] = useState(() => readRates(offer.reviewed_pricing));
  const [error, setError] = useState("");
  const kind = offer.reviewed_pricing.kind;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    setError("");
    try {
      onPreview({
        kind: "update_price", offer_id: offer.id, primary_usd_nanos: usdNanos(rates.amount, kind),
        ...(kind === "text" ? { output_usd_nanos: usdNanos(rates.output, kind) } : {}),
      });
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "Check the price.");
    }
  }

  return (
    <Card className="p-4 sm:p-6">
      <form className="space-y-4" onSubmit={submit}>
        <div className="space-y-1">
          <h3 className="text-xl font-semibold">Edit quote rate</h3>
          <p className="break-all text-sm text-muted">{offer.id}</p>
          <p className="text-sm">Active rate · <Rate price={offer.pricing} /></p>
        </div>
        <fieldset className="min-w-0" disabled={busy}>
          <RateInputs kind={kind} rates={rates} change={(next) => { setRates(next); setError(""); clearPreview(); }} />
        </fieldset>
        {error && <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
        <div className="flex flex-wrap gap-2">
          <Button type="submit" disabled={busy}>{busy ? "Validating…" : "Validate change"}</Button>
          <Button type="button" variant="outline" onClick={onCancel}>Close editor</Button>
        </div>
      </form>
    </Card>
  );
}

function ChangeReview({ review }: { review: ReviewedChange }) {
  const { preview, evidence } = review;
  const download = (value: unknown, filename: string, indent?: number) => downloadBlob(
    new Blob([JSON.stringify(value, null, indent)], { type: "application/json" }), filename,
  );
  return (
    <Card className="space-y-3 p-4 sm:p-6">
      <h3 className="text-xl font-semibold">Change validated</h3>
      <p role="status" className="text-sm text-muted">
        The gateway accepted the configuration format for {preview.changed_offer_ids.length} {preview.changed_offer_ids.length === 1 ? "offer" : "offers"}. Awaiting operator deployment.
      </p>
      <ul className="list-inside list-disc text-sm">
        {preview.catalog.offers.filter((offer) => preview.changed_offer_ids.includes(offer.id)).map((offer) => (
          <li className="break-words" key={offer.id}>{offer.id} · <Rate price={offer.pricing} /></li>
        ))}
      </ul>
      {preview.catalog_patch && <p className="text-sm text-muted">Apply the patch to the reviewed catalog during deployment.</p>}
      {preview.rate_feed_active && <p className="text-sm">Coordinate this change with the verified rate feed before deploying.</p>}
      <div className="flex flex-wrap gap-2">
        <Button type="button" onClick={() => download({ ...preview, discovery_evidence: evidence }, "provider-change.json", 2)}>
          <Download size={16} aria-hidden="true" />Download review bundle
        </Button>
        <Button type="button" variant="outline" onClick={() => download(preview.catalog, preview.catalog_patch ? "provider-catalog-patch.json" : "catalog.json")}>{preview.catalog_patch ? "Download catalog patch" : "Download catalog"}</Button>
      </div>
    </Card>
  );
}
