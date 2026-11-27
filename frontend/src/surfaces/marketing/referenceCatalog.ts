import { downloadBlob } from "../../lib/download.ts";
import { modalityNames, type Modality } from "../requests/modelOptions.ts";
import { referencePlugin } from "./referencePlugins.ts";

export const referenceCatalogUrl = "/catalog/artcraft-v1.json";
export const referenceCatalogFile = "artcraft-v1.json";
export const referenceOfferPageSize = 50;

// Match the reference catalog limits enforced by the gateway's catalog CLI.
const maxReferenceBytes = 4 * 1024 * 1024;
const maxReferenceOffers = 4_096;

export type ReferenceOffer = {
  id: string;
  model: string;
  name: string;
  plugin: string;
  upstream_model: string;
  kind: Modality;
  preset: string;
  capabilities?: unknown;
  rates: { usd_nanos: number; unit: string; terms?: string }[];
  source_url: string;
};

export type ReferenceCatalog = { version: 1; revision: string; offers: ReferenceOffer[] };
export type ReferenceSnapshot = { catalog: ReferenceCatalog; bytes: ArrayBuffer; checksum?: string };

export async function loadReferenceSnapshot(signal: AbortSignal): Promise<ReferenceSnapshot> {
  const response = await fetch(referenceCatalogUrl, { credentials: "omit", cache: "no-cache", signal });
  if (!response.ok || !response.body) {
    void response.body?.cancel().catch(() => {});
    throw new Error("Catalog unavailable");
  }
  const bytes = await readReferenceBytes(response.body);
  signal.throwIfAborted();
  const catalog = parseReferenceCatalog(bytes);
  const hash = await globalThis.crypto?.subtle?.digest("SHA-256", bytes).catch(() => undefined);
  signal.throwIfAborted();
  const checksum = hash && Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, "0")).join("");
  return { catalog, bytes, checksum };
}

async function readReferenceBytes(body: ReadableStream<Uint8Array>): Promise<ArrayBuffer> {
  // Fetch exposes decoded bytes even when Content-Length describes a compressed response.
  const reader = body.getReader();
  let bytes = new Uint8Array(0);
  let used = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return bytes.slice(0, used).buffer;
      const required = used + value.byteLength;
      if (required > maxReferenceBytes) {
        void reader.cancel().catch(() => {});
        throw new Error("Catalog exceeds 4 MiB");
      }
      if (required > bytes.length) {
        const expanded = new Uint8Array(Math.min(maxReferenceBytes, Math.max(bytes.length * 2, required)));
        expanded.set(bytes);
        bytes = expanded;
      }
      bytes.set(value, used);
      used = required;
    }
  } finally {
    reader.releaseLock();
  }
}

export function downloadReferenceSnapshot(snapshot: ReferenceSnapshot) {
  downloadBlob(new Blob([snapshot.bytes], { type: "application/json" }), referenceCatalogFile);
}

export function parseReferenceCatalog(bytes: ArrayBuffer): ReferenceCatalog {
  if (bytes.byteLength > maxReferenceBytes) throw new Error("Catalog exceeds 4 MiB");
  const catalog = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)) as
    (ReferenceCatalog & { publisher?: string; currency?: string }) | null;
  if (catalog?.version !== 1 || catalog.publisher !== "ArtCraft" || catalog.currency !== "USD"
    || typeof catalog.revision !== "string" || !catalog.revision || !Array.isArray(catalog.offers)
    || !catalog.offers.length || catalog.offers.length > maxReferenceOffers) {
    throw new Error("Unsupported catalog format");
  }

  const ids = new Set<string>();
  for (const offer of catalog.offers) {
    if (!offer || [offer.id, offer.model, offer.name, offer.plugin, offer.upstream_model, offer.preset, offer.source_url].some(
      (value) => typeof value !== "string" || !value,
    ) || ids.has(offer.id) || !Object.hasOwn(modalityNames, offer.kind) || !Array.isArray(offer.rates)
      || offer.rates.some((rate) => !rate || !Number.isSafeInteger(rate.usd_nanos) || rate.usd_nanos < 0
        || typeof rate.unit !== "string" || (rate.terms !== undefined && typeof rate.terms !== "string"))) {
      throw new Error("Invalid reference offer");
    }
    const source = new URL(offer.source_url);
    if (source.protocol !== "https:" || source.username || source.password) throw new Error("Invalid rate source");
    ids.add(offer.id);
  }
  return catalog;
}

export function canPrepare(offer: ReferenceOffer) {
  if (!offer.capabilities || typeof offer.capabilities !== "object" || Array.isArray(offer.capabilities)) return false;
  return referencePlugin(offer.plugin)?.kinds.includes(offer.kind) ?? false;
}

export function filterReferenceOffers(offers: ReferenceOffer[], search: string) {
  const query = search.trim().toLowerCase();
  if (!query) return offers;
  return offers.filter((offer) => [offer.name, offer.id, offer.model, offer.upstream_model, offer.plugin,
    modalityNames[offer.kind], offer.preset].some((value) => value.toLowerCase().includes(query)));
}
