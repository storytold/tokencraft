import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";
import { filterReferenceOffers, loadReferenceSnapshot, parseReferenceCatalog } from "../frontend/src/surfaces/marketing/referenceCatalog.ts";

const maxBytes = 4 * 1024 * 1024;
const staticCatalog = readFileSync(new URL("../catalog/artcraft-v1.json", import.meta.url));
const source = JSON.parse(staticCatalog);
const encode = (value) => new TextEncoder().encode(JSON.stringify(value)).buffer;

test("pins the exact decoded catalog bytes at the published size limit", async (t) => {
  const bytes = new Uint8Array(maxBytes).fill(32);
  bytes.set(staticCatalog);
  const signal = new AbortController().signal;
  t.mock.method(globalThis, "fetch", (url, options) => {
    assert.equal(url, "/catalog/artcraft-v1.json");
    assert.deepEqual(options, { credentials: "omit", cache: "no-cache", signal });
    return new Response(new ReadableStream({
      start(controller) {
        controller.enqueue(bytes.subarray(0, 15));
        controller.enqueue(bytes.subarray(15));
        controller.close();
      },
    }), { headers: { "Content-Length": "512", "Content-Encoding": "gzip" } });
  });

  const snapshot = await loadReferenceSnapshot(signal);
  assert.deepEqual(new Uint8Array(snapshot.bytes), bytes);
  assert.equal(snapshot.checksum, createHash("sha256").update(bytes).digest("hex"));
  assert.equal(snapshot.catalog.revision, source.revision);
});

test("cancels an oversize stream without waiting for its end", { timeout: 2_000 }, async (t) => {
  let cancelled = false;
  const stream = new ReadableStream({
    start(controller) {
      controller.enqueue(new Uint8Array(maxBytes));
      controller.enqueue(new Uint8Array([32]));
    },
    cancel() { cancelled = true; },
  });
  t.mock.method(globalThis, "fetch", () => new Response(stream));
  await assert.rejects(loadReferenceSnapshot(new AbortController().signal), /Catalog exceeds 4 MiB/);
  assert.equal(cancelled, true);
  assert.equal(stream.locked, false);
  assert.throws(() => parseReferenceCatalog(new ArrayBuffer(maxBytes + 1)), /Catalog exceeds 4 MiB/);
});

test("rejects an unsuccessful HTTP response and releases its body", async (t) => {
  let cancelled = false;
  const stream = new ReadableStream({ cancel() { cancelled = true; } });
  t.mock.method(globalThis, "fetch", () => new Response(stream, { status: 503 }));
  await assert.rejects(loadReferenceSnapshot(new AbortController().signal), /Catalog unavailable/);
  assert.equal(cancelled, true);
});

test("supports all 4,096 offers and matches entries outside the first page", () => {
  const offers = Array.from({ length: 4_096 }, (_, index) => ({
    ...source.offers[0], id: "fixture_" + index, name: "Fixture " + index, upstream_model: "fal-ai/fixture/" + index,
  }));
  const catalog = parseReferenceCatalog(encode({ ...source, offers }));
  assert.equal(catalog.offers.length, 4_096);
  assert.deepEqual(filterReferenceOffers(catalog.offers, "  FAL-AI/FIXTURE/4095  "), [offers.at(-1)]);
  assert.throws(() => parseReferenceCatalog(encode({ ...source, offers: [...offers, { ...offers[0], id: "extra" }] })),
    /Unsupported catalog format/);
  assert.throws(() => parseReferenceCatalog(encode({ ...source, offers: [] })), /Unsupported catalog format/);
});
