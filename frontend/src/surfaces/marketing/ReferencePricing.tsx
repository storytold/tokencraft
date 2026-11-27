import { useEffect, useMemo, useState } from "react";
import { Button, Card, Input } from "@/components/ui";
import { providerLabel, usd } from "@/surfaces/comparison/api";
import { modalityNames } from "@/surfaces/requests/modelOptions";
import { CatalogSelection } from "./CatalogSelection";
import {
  downloadReferenceSnapshot, filterReferenceOffers, loadReferenceSnapshot, referenceCatalogUrl,
  referenceOfferPageSize, type ReferenceOffer, type ReferenceSnapshot,
} from "./referenceCatalog";

const cell = "px-3 py-3 align-top sm:px-5";

export function ReferencePricing() {
  const [snapshot, setSnapshot] = useState<ReferenceSnapshot>();
  const [preparing, setPreparing] = useState<boolean>();
  const [failed, setFailed] = useState(false);
  const [retry, setRetry] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    loadReferenceSnapshot(controller.signal).then((result) => {
      if (!controller.signal.aborted) setSnapshot(result);
    }).catch(() => {
      if (!controller.signal.aborted) setFailed(true);
    });
    return () => controller.abort();
  }, [retry]);

  return (
    <section className="space-y-4" aria-labelledby="reference-heading">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 id="reference-heading" className="font-display text-2xl font-bold tracking-tight">ArtCraft catalog</h2>
          <p className="mt-1 text-sm text-muted">Public provider list prices for models with TokenCraft plugin support.</p>
        </div>
        {snapshot && <div className="flex flex-wrap items-center gap-3">
          <a href={referenceCatalogUrl} download className="font-mono text-sm font-semibold text-brand hover:underline" onClick={(event) => {
            event.preventDefault();
            downloadReferenceSnapshot(snapshot);
          }}>Download JSON</a>
          <Button variant="outline" size="sm" type="button" aria-expanded={Boolean(preparing)}
            aria-controls="reference-selection" onClick={() => setPreparing((open) => !open)}>Prepare offers</Button>
        </div>}
      </div>
      {failed && <div className="flex flex-wrap items-center gap-3">
        <p role="alert" className="text-sm text-muted">The ArtCraft catalog is unavailable.</p>
        <Button variant="outline" size="sm" onClick={() => { setFailed(false); setRetry((attempt) => attempt + 1); }}>Retry catalog</Button>
      </div>}
      {!snapshot && !failed && <p role="status" className="text-sm text-muted">Loading the ArtCraft catalog…</p>}
      {snapshot && (
        <>
          <p className="text-xs text-muted">Price snapshot <time dateTime={snapshot.catalog.revision}>{snapshot.catalog.revision.slice(0, 10)}</time></p>
          <div id="reference-selection" hidden={!preparing}>
            {preparing !== undefined && <CatalogSelection catalog={snapshot.catalog} checksum={snapshot.checksum} />}
          </div>
          <ReferenceRates offers={snapshot.catalog.offers} />
        </>
      )}
    </section>
  );
}

function ReferenceRates({ offers }: { offers: ReferenceOffer[] }) {
  const [search, setSearch] = useState("");
  const [page, setPage] = useState(0);
  const matches = useMemo(() => filterReferenceOffers(offers, search), [offers, search]);
  const offset = page * referenceOfferPageSize;
  const current = matches.slice(offset, offset + referenceOfferPageSize);
  return (
    <div className="space-y-3">
      <label className="block max-w-md space-y-1.5 text-sm font-medium">
        <span>Search list prices</span>
        <Input type="search" value={search} onChange={(event) => {
          setSearch(event.target.value);
          setPage(0);
        }} />
      </label>
      <Card className="overflow-hidden">
        <table aria-labelledby="reference-heading" className="w-full table-fixed text-left text-sm">
          <thead className="border-b border-border bg-surface2/60 text-muted">
            <tr>
              <th scope="col" className={`${cell} w-[52%] font-medium sm:w-[60%]`}>Model and plugin</th>
              <th scope="col" className={`${cell} font-medium`}>Provider list rate</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {current.map((offer) => <ReferenceRateRow key={offer.id} offer={offer} />)}
            {!current.length && <tr><td colSpan={2} className={`${cell} text-muted`}>No matching offers.</td></tr>}
          </tbody>
        </table>
      </Card>
      {matches.length > referenceOfferPageSize && (
        <nav className="flex flex-wrap items-center justify-between gap-3" aria-label="Catalog pages">
          <p className="text-sm text-muted" role="status">{offset + 1}–{offset + current.length} of {matches.length} offers</p>
          <div className="flex gap-2">
            <Button type="button" size="sm" variant="outline" aria-label="Previous catalog page"
              disabled={page === 0} onClick={() => setPage((index) => index - 1)}>Previous</Button>
            <Button type="button" size="sm" variant="outline" aria-label="Next catalog page"
              disabled={offset + current.length === matches.length} onClick={() => setPage((index) => index + 1)}>Next</Button>
          </div>
        </nav>
      )}
    </div>
  );
}

function ReferenceRateRow({ offer }: { offer: ReferenceOffer }) {
  return (
    <tr>
      <th scope="row" className={`${cell} break-words font-medium`}>
        {offer.name}
        <code className="mt-1 block break-all text-xs font-normal text-muted">{offer.upstream_model}</code>
        <span className="mt-1 block text-xs font-normal text-muted">{providerLabel(offer.plugin)} · {modalityNames[offer.kind]}</span>
        <span className="block text-xs font-normal text-muted">{offer.preset}</span>
      </th>
      <td className={`${cell} space-y-1 break-words`}>
        {offer.rates.length ? offer.rates.map(({ unit, usd_nanos, terms }, index) => (
          <div key={`${unit}/${index}`}>
            <span className="font-mono font-medium tabular-nums">{usd(usd_nanos)}</span> <span className="text-muted">/ {unit}</span>
            {terms && <p className="text-xs text-muted">{terms}</p>}
          </div>
        )) : <p className="text-sm text-muted">Preview rate unconfirmed</p>}
        <a href={offer.source_url} className="inline-block text-xs font-medium text-brand hover:underline">Rate source<span className="sr-only"> for {offer.name}</span></a>
      </td>
    </tr>
  );
}
