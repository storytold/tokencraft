import assert from "node:assert/strict";
import { createHash, createHmac } from "node:crypto";
import { once } from "node:events";
import { createServer } from "node:http";
import test from "node:test";
import {
  TokenCraft, TokenCraftError, TokenCraftTimeoutError, planOfferComparison, refreshOfferComparison, runOfferComparison,
  verifyWebhook, verifyWorkflowWebhook,
} from "@tokencraft/sdk";

const requestId = "req_01m881w0c8np13xfvetqsbk8m0";
const traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
const image = { model: "studio_image_demo", workload: { kind: "image", count: 1, aspect_ratio: "7:5" } };
const options = { baseUrl: "https://gateway.example/edge/", apiKey: "fixture_key" };

test("unusable error bodies preserve the HTTP status and request ID", async () => {
  const diagnostic = JSON.stringify({ error: { code: "proxy_diagnostic", message: "diagnostic line\n".repeat(5_000) } });
  const headers = { "TokenCraft-Request-Id": requestId };
  let canceled = false;
  const streamed = new ReadableStream({
    start(controller) {
      const bytes = new TextEncoder().encode(diagnostic);
      controller.enqueue(bytes.subarray(0, 32 * 1024));
      controller.enqueue(bytes.subarray(32 * 1024));
    },
    cancel() { canceled = true; },
  });
  const replies = [
    new Response(diagnostic, { status: 502, headers: { ...headers, "Content-Length": String(diagnostic.length) } }),
    new Response(streamed, { status: 502, headers }),
    new Response("<h1>Upstream unavailable</h1>", { status: 502, headers }),
  ];
  const client = new TokenCraft({ ...options, fetch: async () => replies.shift() });
  for (let i = 0; i < 3; i++) {
    await assert.rejects(client.quote(image), { status: 502, code: "http_error", requestId });
  }
  assert.ok(canceled);
});

test("stalled HTTP error bodies are canceled while the known status is returned", { timeout: 4_000 }, async (t) => {
  let closeResponse;
  const closed = new Promise((resolve) => { closeResponse = resolve; });
  const server = createServer((request, response) => {
    request.resume();
    response.once("close", closeResponse);
    response.writeHead(502, { "Content-Length": "32", "TokenCraft-Request-Id": requestId });
    response.write("{");
  });
  server.listen(0, "127.0.0.1");
  t.after(() => {
    server.closeAllConnections();
    return new Promise((resolve) => server.close(resolve));
  });
  await once(server, "listening");
  const client = new TokenCraft({
    ...options, baseUrl: `http://127.0.0.1:${server.address().port}`, timeoutMs: 5_000,
  });
  await assert.rejects(client.quote(image), { status: 502, code: "http_error", requestId });
  await closed;
});

test("caller cancellation interrupts error decoding", async () => {
  let canceled = 0;
  const client = new TokenCraft({ ...options, timeoutMs: 5_000, fetch: async () => {
    const stream = new ReadableStream({
      start(controller) { controller.enqueue(new TextEncoder().encode("{")); },
      cancel() { canceled++; },
    });
    return new Response(stream, { status: 502, headers: { "TokenCraft-Request-Id": requestId } });
  } });
  const controller = new AbortController();
  const reason = new Error("stop waiting on the response");
  const operation = client.quote(image, { signal: controller.signal });
  await Promise.resolve();
  controller.abort(reason);
  await assert.rejects(operation, (error) => error === reason);
  assert.equal(canceled, 1);
});

test("chunked TokenCraft errors retain their UTF-8 details", async () => {
  const message = "Le fichier est prêt.";
  const encoded = new TextEncoder().encode(JSON.stringify({ error: { code: "fixture", message } }));
  const split = encoded.indexOf(0xc3) + 1;
  const stream = new ReadableStream({ start(controller) {
    controller.enqueue(encoded.subarray(0, split));
    controller.enqueue(encoded.subarray(split));
    controller.close();
  } });
  const client = new TokenCraft({ ...options, fetch: async () => new Response(stream, {
    status: 409, headers: { "TokenCraft-Request-Id": requestId },
  }) });
  await assert.rejects(client.quote(image), { status: 409, code: "fixture", requestId, message });
});

test("uses scoped quotes, preserves the base path, and sends no secret for public calls", async () => {
  const seen = [];
  const client = new TokenCraft({
    ...options,
    fetch: async (url, init) => {
      seen.push([url.pathname + url.search, init.headers.get("Authorization"), init.redirect, init.credentials]);
      return Response.json({});
    },
  });
  await client.catalog();
  await client.quote(image);
  await client.quote(image, { clientRef: "studio.alpha:23" });
  await client.quote(image, { scope: "public" });
  assert.throws(() => client.quote(image, { scope: "public", clientRef: "studio.alpha:23" }), TypeError);
  assert.deepEqual(seen, [
    ["/edge/v1/catalog/offers", null, "error", "omit"],
    ["/edge/v1/quotes/scoped", "Bearer fixture_key", "error", "omit"],
    ["/edge/v1/quotes/scoped?client_ref=studio.alpha%3A23", "Bearer fixture_key", "error", "omit"],
    ["/edge/v1/quotes", null, "error", "omit"],
  ]);
  assert.equal(JSON.stringify(client), "{}");
  const anonymous = new TokenCraft({ baseUrl: options.baseUrl, fetch: async () => assert.fail("must not dispatch") });
  await assert.rejects(anonymous.get(requestId), { name: "TokenCraftError", code: "missing_key", requestId });
  for (const baseUrl of ["http://gateway.example", "https://secret@gateway.example", "https://gateway.example?private=1"]) {
    assert.throws(() => new TokenCraft({ baseUrl }), TypeError);
  }
});

test("discovers models with project authentication only on the scoped endpoint", async () => {
  const seen = [];
  const model = { id: "studio_video", text: false, image: false, aspect_ratios: [],
    video: [{ durations_seconds: [5], aspect_ratios: ["16:9"], resolution: "720p" }] };
  const fetch = async (url, init) => {
    seen.push([url.pathname, init.headers.get("Authorization"), init.cache, init.credentials]);
    const providers = url.pathname.endsWith("/scoped") ? ["private-connector"] : undefined;
    return Response.json({ revision: "fixture", models: [model], providers });
  };
  const client = new TokenCraft({ ...options, fetch });
  const scoped = await client.models();
  assert.deepEqual(scoped.models, [model]);
  assert.deepEqual(scoped.providers, ["private-connector"]);
  assert.equal((await client.models({ scope: "public" })).providers, undefined);
  const anonymous = new TokenCraft({ baseUrl: options.baseUrl, fetch });
  await anonymous.models();
  await assert.rejects(anonymous.models({ scope: "project" }), { code: "missing_key" });
  assert.deepEqual(seen, [
    ["/edge/v1/catalog/models/scoped", "Bearer fixture_key", "no-store", "omit"],
    ["/edge/v1/catalog/models", null, undefined, "omit"],
    ["/edge/v1/catalog/models", null, undefined, "omit"],
  ]);
});

test("reuses the supplied intent on explicit replay and preserves scoped filter values", async () => {
  const sent = [];
  const client = new TokenCraft({
    ...options,
    fetch: async (url, init) => {
      sent.push({ url, body: init.body, key: init.headers.get("Idempotency-Key") });
      return Response.json({ id: requestId });
    },
  });
  const quote = { id: "quo_fixture", input: image };
  const submission = {
    idempotencyKey: "fixture-job-123", clientRef: "studio.alpha:23", appRef: "filmcraft",
    webhookSubscriptionId: "whs_fixture",
  };
  assert.throws(() => client.submit(quote, { idempotencyKey: "short" }), TypeError);
  assert.throws(() => client.get("../requests"), TypeError);
  await client.submit(quote, submission);
  await client.submit(quote, submission);
  assert.equal(sent[0].body, sent[1].body);
  assert.equal(sent[0].key, sent[1].key);
  assert.deepEqual(JSON.parse(sent[0].body), {
    quote_id: quote.id, input: image, client_ref: submission.clientRef, app_ref: submission.appRef,
    delivery: { webhook_subscription_id: submission.webhookSubscriptionId },
  });
  const traceId = "4bf92f3577b34da6a3ce929d0e0e4736";
  await client.list({
    clientRef: submission.clientRef, appRef: submission.appRef, after: requestId, model: image.model, traceId,
    provider: "fal", status: "succeeded", costState: "mock_reported", from: "2026-11-24", to: "2026-11-25",
  });
  assert.deepEqual(Object.fromEntries(sent[2].url.searchParams), {
    client_ref: submission.clientRef, app_ref: submission.appRef, after: requestId, model: image.model, trace_id: traceId,
    provider: "fal", status: "succeeded", cost_state: "mock_reported", from: "2026-11-24", to: "2026-11-25",
  });
  await client.list({ clientUnattributed: true, appUnattributed: true, model: image.model, exactModel: true });
  assert.deepEqual(Object.fromEntries(sent[3].url.searchParams), {
    client_unattributed: "true", app_unattributed: "true", model: image.model, exact_model: "true",
  });
});

test("diagnostic history requires confirmation that the scoped filter was applied", async () => {
  let confirm = true;
  const client = new TokenCraft({
    ...options,
    fetch: async (url) => {
      assert.deepEqual(Object.fromEntries(url.searchParams), {
        client_ref: "studio_a", after: requestId, diagnostic: "warnings",
      });
      return Response.json({ requests: [], next_after: null, diagnostic: confirm ? "warnings" : undefined });
    },
  });
  const filters = { clientRef: "studio_a", after: requestId, diagnostic: "warnings" };
  assert.equal((await client.list(filters)).diagnostic, "warnings");
  confirm = false;
  await assert.rejects(client.list(filters), /did not confirm the requested diagnostic/);
});

test("reads the scoped provider path and tolerates older trace responses", async () => {
  const path = {
    admitted_offer_id: "openrouter_fixture",
    attempts: [{
      offer_id: "openrouter_fixture", provider: "openrouter", state: "succeeded",
      step_id: "provider", correlation_id: "gen_fixture",
    }],
    downstream: { status: "reported", provider: "Fixture Provider" },
  };
  const traces = [{ provider_path: path }, {}];
  const client = new TokenCraft({
    ...options,
    fetch: async (url, init) => {
      assert.equal(init.method, "GET");
      assert.equal(url.pathname, `/edge/v1/requests/${requestId}/trace`);
      assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
      return Response.json(traces.shift());
    },
  });
  assert.deepEqual(await client.providerPath(requestId), path);
  assert.equal(await client.providerPath(requestId), null);
  assert.equal(traces.length, 0);
});

test("reads the trace, its comparison, and OTLP with the same scoped key", async () => {
  const step = { id: "provider", parent_id: "routing", kind: "interval", status: "Response started" };
  const evidence = { request_id: requestId, status: "succeeded", steps: [step] };
  const snapshot = { revision: "trace-v1", evidence };
  const otlp = { resourceSpans: [{ scopeSpans: [{ spans: [{ startTimeUnixNano: "1795629600000000000" }] }] }] };
  const baseline = { provider: "example-custom", offer_id: "image_exact", request_ids: [], timings: [] };
  const outbound = [];
  const client = new TokenCraft({ ...options, fetch: async (url, init) => {
    assert.equal(init.method, "GET");
    assert.equal(init.redirect, "error");
    assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
    outbound.push(url.pathname);
    return Response.json(url.pathname.endsWith("/otlp") ? otlp
      : url.pathname.endsWith("/baseline") ? baseline : snapshot);
  } });
  assert.deepEqual(await client.trace(requestId), snapshot);
  assert.deepEqual(await client.traceOtlp(requestId), otlp);
  assert.deepEqual(await client.traceBaseline(requestId), baseline);
  assert.deepEqual(outbound, [
    `/edge/v1/requests/${requestId}/trace`, `/edge/v1/requests/${requestId}/trace/otlp`,
    `/edge/v1/requests/${requestId}/trace/baseline`,
  ]);
});

test("carries the gateway error code and receipt header without retrying a submission", async () => {
  let calls = 0;
  const client = new TokenCraft({
    ...options,
    fetch: async () => {
      calls++;
      return Response.json({ error: { code: "conflict", message: "quote changed" } }, {
        status: 409, headers: { "TokenCraft-Request-Id": requestId },
      });
    },
  });
  await assert.rejects(client.submit({ id: "quo_fixture", input: image }, { idempotencyKey: "fixture-job" }), (error) =>
    error instanceof TokenCraftError && error.status === 409 && error.code === "conflict" && error.requestId === requestId);
  assert.equal(calls, 1);
});

test("opt-in submission retries reuse the serialized intent and idempotency key", async () => {
  const quote = { id: "quo_pinned", input: structuredClone(image), pinned_offer_id: "fal_image" };
  const submission = { idempotencyKey: "fixture-retry", clientRef: "customer-1", appRef: "studio", imagePrompt: "Violet kite", traceparent };
  const sent = [];
  const client = new TokenCraft({ ...options, submitMaxAttempts: 3, fetch: async (url, init) => {
    sent.push({ url: url.href, method: init.method, headers: [...init.headers], body: init.body });
    if (sent.length === 1) {
      quote.input.workload.count = 2;
      submission.idempotencyKey = "fixture-changed";
      submission.traceparent = traceparent.replace("01", "00");
      init.headers.set("Idempotency-Key", "fixture-changed");
      init.headers.delete("traceparent");
      url.pathname = "/changed";
      return Response.json({}, { status: 503, headers: { "Retry-After": "Sunday, 06-Nov-94 08:49:37 GMT" } });
    }
    if (sent.length === 2) return Response.json({}, { status: 429, headers: { "Retry-After": "0" } });
    return Response.json({ id: requestId }, { status: 202 });
  } });
  assert.equal((await client.submit(quote, submission)).id, requestId);
  assert.deepEqual(sent, [sent[0], sent[0], sent[0]]);
  assert.ok(sent[0].headers.some(([key, value]) => key === "idempotency-key" && value === "fixture-retry"));
  assert.ok(sent[0].headers.some(([key, value]) => key === "traceparent" && value === traceparent));
  const body = JSON.parse(sent[0].body);
  assert.equal(body.input.workload.count, 1);
  assert.equal(body.pinned_offer_id, "fal_image");
  assert.equal(body.client_ref, "customer-1");
  assert.equal(body.app_ref, "studio");
});

test("opt-in submission recovers a receipt after connection loss", async (t) => {
  const sent = [];
  const receipt = JSON.stringify({ id: requestId });
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    sent.push({
      method: request.method, path: request.url, key: request.headers["idempotency-key"],
      authorization: request.headers.authorization, traceparent: request.headers.traceparent,
      body: Buffer.concat(chunks).toString(),
    });
    if (sent.length === 1) {
      request.socket.destroy();
      return;
    }
    response.writeHead(sent.length === 2 ? 202 : 200, {
      "Content-Type": "application/json", "Content-Length": Buffer.byteLength(receipt),
      "TokenCraft-Request-Id": requestId, Connection: "close",
    });
    response.end(sent.length === 2 ? receipt.slice(0, -8) : receipt);
  });
  server.listen(0, "127.0.0.1");
  t.after(() => {
    server.closeAllConnections();
    return new Promise((resolve) => server.close(resolve));
  });
  await once(server, "listening");
  const client = new TokenCraft({
    ...options, baseUrl: `http://127.0.0.1:${server.address().port}/edge/`, submitMaxAttempts: 3, timeoutMs: 2000,
  });
  const quote = { id: "quo_fixture", input: image, pinned_offer_id: "fal_image" };
  const submission = { idempotencyKey: "fixture-recover", clientRef: "customer-1", traceparent };
  assert.equal((await client.submit(quote, submission)).id, requestId);
  assert.deepEqual(sent, [sent[0], sent[0], sent[0]]);
  assert.equal(sent[0].method, "POST");
  assert.equal(sent[0].path, "/edge/v1/requests");
  assert.equal(sent[0].key, "fixture-recover");
  assert.equal(sent[0].authorization, "Bearer fixture_key");
  assert.equal(sent[0].traceparent, traceparent);
  assert.equal(JSON.parse(sent[0].body).client_ref, "customer-1");
});

test("submission leaves complete invalid receipts terminal", async () => {
  let calls = 0;
  const client = new TokenCraft({ ...options, submitMaxAttempts: 3, fetch: async () => {
    calls++;
    return new Response("{", { status: 202, headers: { "TokenCraft-Request-Id": requestId } });
  } });
  const quote = { id: "quo_fixture", input: image };
  await assert.rejects(client.submit(quote, { idempotencyKey: "fixture-invalid" }), {
    code: "invalid_response", requestId,
  });
  assert.equal(calls, 1);
});

test("validates caller trace context and keeps it local to the supplied submission", async () => {
  const seen = [];
  const client = new TokenCraft({ ...options, fetch: async (_, init) => {
    seen.push([init.headers.get("traceparent"), init.headers.get("tracestate"), init.headers.get("baggage")]);
    return Response.json({ id: requestId });
  } });
  const quote = { id: "quo_fixture", input: image };
  const future = traceparent.replace(/^00-/, "01-") + "-extension";
  for (const invalid of [
    traceparent.replace(/^00-/, "ff-"),
    traceparent.replace("00f067aa0ba902b7", "0".repeat(16)),
    traceparent.replace("4bf92f3577b34da6a3ce929d0e0e4736", "0".repeat(32)),
    traceparent + ", " + traceparent,
    traceparent + "-extra",
    future + "a".repeat(256),
    traceparent + "\r\nX-Other: discarded",
  ]) {
    assert.throws(() => client.submit(quote, { idempotencyKey: "fixture-linked", traceparent: invalid }), TypeError);
  }
  await client.submit(quote, { idempotencyKey: "fixture-linked", traceparent: future });
  await client.catalog();
  await client.submit(quote, { idempotencyKey: "fixture-unlinked" });
  assert.deepEqual(seen, [[future, null, null], [null, null, null], [null, null, null]]);
});

test("submission preserves terminal gateway errors and enforces the attempt limit", async () => {
  const quote = { id: "quo_fixture", input: image };
  for (const [status, hint, maxAttempts, expectedCalls] of [
    [503, "0", undefined, 1],
    [503, undefined, 3, 1],
    [503, "0, 1", 3, 1],
    [503, "Sun, 06 Nov 1994 08:49:37 GMT, 0", 3, 1],
    [503, "2026-01-01T00:00:00Z", 3, 1],
    [503, "Mon, 06 Nov 1994 08:49:37 GMT", 3, 1],
    [409, "0", 3, 1],
    [503, "0", 2, 2],
  ]) {
    let calls = 0;
    const client = new TokenCraft({ ...options, submitMaxAttempts: maxAttempts, fetch: async () => {
      calls++;
      const headers = hint === undefined ? {} : { "Retry-After": hint };
      return Response.json({ error: { code: "fixture", message: "try later" } }, { status, headers });
    } });
    await assert.rejects(client.submit(quote, { idempotencyKey: "fixture-retry" }), {
      name: "TokenCraftError", status, code: "fixture",
    });
    assert.equal(calls, expectedCalls);
  }
  assert.throws(() => new TokenCraft({ ...options, submitMaxAttempts: 9 }), RangeError);
});

test("submission retry delays honor the total deadline and caller cancellation", async () => {
  let calls = 0;
  const controller = new AbortController();
  const reason = new Error("client canceled");
  const client = new TokenCraft({ ...options, submitMaxAttempts: 3, timeoutMs: 30, fetch: async () => {
    if (++calls === 2) setTimeout(() => controller.abort(reason), 0);
    return Response.json({}, { status: 503, headers: {
      "Retry-After": new Date(Date.now() + 60_000).toUTCString(),
      "TokenCraft-Request-Id": "invalid",
      "X-Request-Id": requestId,
    } });
  } });
  const quote = { id: "quo_fixture", input: image };
  await assert.rejects(client.submit(quote, { idempotencyKey: "fixture-retry" }), (error) =>
    error instanceof TokenCraftTimeoutError && error.requestId === requestId);
  assert.equal(calls, 1);
  await assert.rejects(client.submit(quote, { idempotencyKey: "fixture-retry", signal: controller.signal }),
    (error) => error === reason);
  assert.equal(calls, 2);
});

test("wait polls in-flight requests and exposes uncertain provider acceptance to the caller", async () => {
  const states = ["queued", "running", "awaiting_reconciliation"];
  const client = new TokenCraft({ ...options, fetch: async () => Response.json({ status: states.shift() }) });
  assert.equal((await client.wait(requestId, { pollIntervalMs: 1 })).status, "awaiting_reconciliation");
  assert.equal(states.length, 0);
});

test("wait reuses the validator across a rollout response and an empty 304", async () => {
  const reads = [
    { validator: null, response: Response.json({ status: "queued" }, { headers: { ETag: 'W/"queued"' } }) },
    { validator: 'W/"queued"', response: Response.json({}, { status: 503, headers: {
      ETag: '"wrong"', "Retry-After": "Sun Nov  6 08:49:37 1994",
    } }) },
    { validator: 'W/"queued"', response: new Response(null, { status: 304 }) },
    { validator: 'W/"queued"', response: Response.json({ status: "succeeded" }) },
  ];
  const client = new TokenCraft({ ...options, fetch: async (_, init) => {
    const next = reads.shift();
    assert.equal(init.headers.get("If-None-Match"), next.validator);
    return next.response;
  } });
  assert.equal((await client.wait(requestId, { pollIntervalMs: 1 })).status, "succeeded");
  assert.equal(reads.length, 0);
});

test("wait reports a 503 without a retry hint", async () => {
  const client = new TokenCraft({ ...options, fetch: async () => Response.json({}, { status: 503 }) });
  await assert.rejects(client.wait(requestId), { name: "TokenCraftError", status: 503, requestId });
});

test("the polling deadline still applies while respecting Retry-After", async () => {
  let reads = 0;
  const client = new TokenCraft({ ...options, fetch: async () => {
    reads++;
    const later = new Date(Date.now() + 60_000).toUTCString();
    return Response.json({ status: "running" }, { headers: { "Retry-After": later } });
  } });
  await assert.rejects(client.wait(requestId, { pollIntervalMs: 1, timeoutMs: 40 }), TokenCraftTimeoutError);
  assert.equal(reads, 1);
});

test("an overall wait deadline aborts the active read and identifies the request", async () => {
  let aborted = false;
  const client = new TokenCraft({
    ...options,
    fetch: async (_, { signal }) => new Promise((_, reject) => {
      signal.addEventListener("abort", () => { aborted = true; reject(signal.reason); }, { once: true });
    }),
  });
  await assert.rejects(client.wait(requestId, { timeoutMs: 20 }), (issue) =>
    issue instanceof TokenCraftTimeoutError && issue.requestId === requestId && issue.timeoutMs === 20);
  assert.equal(aborted, true);
});

test("caller abort propagates through polling", async () => {
  const controller = new AbortController();
  const reason = new Error("caller disconnected");
  const client = new TokenCraft({
    ...options,
    fetch: async () => {
      controller.abort(reason);
      return Response.json({ status: "queued" });
    },
  });
  await assert.rejects(client.wait(requestId, { signal: controller.signal }), (issue) => issue === reason);
});

test("a deadline during body read preserves an already received request ID", async () => {
  const client = new TokenCraft({
    ...options, timeoutMs: 20,
    fetch: async (_, { signal }) => new Response(new ReadableStream({
      start(stream) {
        signal.addEventListener("abort", () => stream.error(signal.reason), { once: true });
      },
    }), { status: 202, headers: { "TokenCraft-Request-Id": requestId } }),
  });
  await assert.rejects(client.submit({ id: "quo_fixture", input: image }, { idempotencyKey: "fixture-job" }), (issue) =>
    issue instanceof TokenCraftTimeoutError && issue.requestId === requestId);
});

test("verifies exact webhook bytes during signing-key overlap and rejects tampered or expired requests", async () => {
  const now = 1_795_565_000;
  const first = Buffer.alloc(32, 23);
  const second = Buffer.alloc(32, 42);
  const secret = (key) => `whsec_${key.toString("base64")}`;
  const id = requestId.replace(/^req_/, "whd_");
  const event = {
    id, schema_version: 1, occurred_at: new Date(now * 1000).toISOString(), sequence: 3,
    type: "request.succeeded", data: { request_id: requestId },
  };
  const body = Buffer.from(JSON.stringify(event, null, 2));
  const sign = (key, value) => "v1," + createHmac("sha256", key).update(`${id}.${now}.`).update(value).digest("base64");
  const headers = {
    "Webhook-Id": id, "Webhook-Timestamp": String(now),
    "Webhook-Signature": [sign(second, body), sign(first, body)].join(" "),
  };
  assert.deepEqual(await verifyWebhook(body, headers, secret(first), { now }), event);
  assert.deepEqual(await verifyWebhook(body, headers, [secret(second), secret(first)], { now }), event);
  const tagged = { ...event, data: { ...event.data, client_ref: "studio:customer-42" } };
  const taggedBody = Buffer.from(JSON.stringify(tagged));
  assert.deepEqual(await verifyWebhook(taggedBody, { ...headers, "Webhook-Signature": sign(first, taggedBody) }, secret(first), { now }), tagged);
  const malformed = Buffer.from(JSON.stringify({ ...event, data: { ...event.data, client_ref: 42 } }));
  await assert.rejects(verifyWebhook(malformed, { ...headers, "Webhook-Signature": sign(first, malformed) }, secret(first), { now }), { code: "payload" });
  await assert.rejects(verifyWebhook(Buffer.from(JSON.stringify(event)), headers, secret(first), { now }), { code: "signature" });
  await assert.rejects(verifyWebhook(body, headers, secret(first), { now: now + 301 }), { code: "timestamp" });
  await assert.rejects(verifyWebhook(body, headers, secret(Buffer.alloc(32)), { now }), { code: "signature" });
  await assert.rejects(verifyWebhook(body, headers, undefined, { now }), { code: "secret" });
  const mismatched = Buffer.from(JSON.stringify({ ...event, id: "another_event" }));
  await assert.rejects(verifyWebhook(mismatched, { ...headers, "Webhook-Signature": sign(first, mismatched) }, secret(first), { now }), { code: "payload" });
});


test("verifies workflow terminal events without changing the request event contract", async () => {
  const now = 1_795_565_000;
  const key = Buffer.alloc(32, 23);
  const secret = `whsec_${key.toString("base64")}`;
  const id = requestId.replace("req_", "whd_");
  const project = requestId.replace("req_", "proj_");
  const workflow = requestId.replace("req_", "wf_");
  const run = requestId.replace("req_", "wfr_");
  const event = {
    id, schema_version: 1, type: "workflow.run.expired", occurred_at: new Date(now * 1000).toISOString(), sequence: 4,
    data: { run_id: run, workflow_id: workflow, project_id: project, workflow_version: 2, status: "expired",
      fetch_url: `/v1/projects/${project}/workflow-endpoints/${workflow}/runs/${run}` },
  };
  const headers = (body) => ({
    "webhook-id": id, "webhook-timestamp": String(now),
    "webhook-signature": "v1," + createHmac("sha256", key).update(`${id}.${now}.`).update(body).digest("base64"),
  });
  const body = Buffer.from(JSON.stringify(event, null, 2));
  assert.deepEqual(await verifyWorkflowWebhook(body, headers(body), secret, { now }), event);
  await assert.rejects(verifyWebhook(body, headers(body), secret, { now }), { code: "payload" });

  const tagged = { ...event, data: { ...event.data, client_ref: "studio:customer-42" } };
  const taggedBody = Buffer.from(JSON.stringify(tagged));
  await assert.rejects(verifyWorkflowWebhook(taggedBody, headers(body), secret, { now }), { code: "signature" });
  assert.deepEqual(await verifyWorkflowWebhook(taggedBody, headers(taggedBody), secret, { now }), tagged);
  const malformed = Buffer.from(JSON.stringify({ ...event, data: { ...event.data, client_ref: "studio\n" } }));
  await assert.rejects(verifyWorkflowWebhook(malformed, headers(malformed), secret, { now }), { code: "payload" });

  const mismatched = Buffer.from(JSON.stringify({ ...event, data: { ...event.data, fetch_url: "/wrong-run" } }));
  await assert.rejects(verifyWorkflowWebhook(mismatched, headers(mismatched), secret, { now }), { code: "payload" });
});

test("checks equivalent pinned quotes and combined estimates before submitting comparison samples", async () => {
  let downstreamCalls = 0;
  let secondRatio = "7:5";
  const client = new TokenCraft({
    ...options,
    fetch: async (url, init) => {
      if (!url.pathname.endsWith("/v1/quotes/scoped")) {
        downstreamCalls++;
        return Response.json({});
      }
      const offer = url.searchParams.get("pinned_offer_id");
      assert.equal(url.searchParams.get("client_ref"), "studio_a");
      return Response.json({
        id: `quo_${offer}`, input: JSON.parse(init.body), currency: "USD",
        catalog_revision: "fixture", pinned_offer_id: offer, selected_offer_id: offer,
        candidates: [{
          offer_id: offer, reason: "eligible", estimated_usd_nanos: 7,
          applied_aspect_ratio: offer === "offer_a" ? "7:5" : secondRatio,
        }],
      });
    },
  });
  const settings = {
    runId: "fixture-compare",
    input: image,
    offerIds: ["offer_a", "offer_b"],
    prompts: ["identical input", "one more shared input"],
    maxEstimatedUsdNanos: 28,
    clientRef: "studio_a",
  };
  await assert.rejects(planOfferComparison(client, { ...settings, maxEstimatedUsdNanos: 27 }), RangeError);
  secondRatio = "4:3";
  await assert.rejects(planOfferComparison(client, settings), /quoted output shape/);
  secondRatio = "7:5";
  const plan = await planOfferComparison(client, settings);
  assert.equal(plan.estimatedUsdNanos, 28);
  await assert.rejects(runOfferComparison(client, { ...plan, maxEstimatedUsdNanos: 27 }), RangeError);
  assert.equal(downstreamCalls, 0);
});


test("keeps an execution slot occupied when an accepted comparison request remains unresolved", async () => {
  const submitted = [];
  const client = new TokenCraft({
    ...options,
    fetch: async (url, init) => {
      if (url.pathname.endsWith("/v1/quotes/scoped")) {
        const offer = url.searchParams.get("pinned_offer_id");
        return Response.json({
          id: `quo_${offer}`, input: JSON.parse(init.body), currency: "USD",
          catalog_revision: "fixture", pinned_offer_id: offer, selected_offer_id: offer,
          candidates: [{ offer_id: offer, reason: "eligible", estimated_usd_nanos: 1 }],
        });
      }
      if (url.pathname.endsWith("/v1/requests")) {
        if (init.method === "GET") return Response.json({ requests: [], next_after: null });
        const body = JSON.parse(init.body);
        assert.equal(body.quote_id, `quo_${body.pinned_offer_id}`);
        submitted.push(init.headers.get("Idempotency-Key"));
        return Response.json({ id: requestId });
      }
      return Response.json({ id: requestId, status: "awaiting_reconciliation", events: [] });
    },
  });
  const plan = await planOfferComparison(client, {
    runId: "fixture-unresolved",
    input: image,
    offerIds: ["offer_a", "offer_b"],
    prompts: [null, null, null],
    maxEstimatedUsdNanos: 6,
  });
  const report = await runOfferComparison(client, plan);
  assert.equal(new Set(submitted).size, 4);
  assert.deepEqual(report.map(arm => arm.statuses), [
    { awaiting_reconciliation: 2, not_started: 1 },
    { awaiting_reconciliation: 2, not_started: 1 },
  ]);
});

test("refreshes saved comparison costs and native charges through scoped request reads", async () => {
  const quotes = new Map();
  const requests = new Map();
  const reads = [];
  let refreshing = false;
  let denyLast = true;
  const observedAt = "2026-11-25T00:00:01Z";
  const client = new TokenCraft({
    ...options,
    fetch: async (url, init) => {
      if (!refreshing && url.pathname.endsWith("/v1/quotes/scoped")) {
        const offer = url.searchParams.get("pinned_offer_id");
        const quote = {
          id: `quo_${offer}`, input: JSON.parse(init.body), currency: "USD",
          catalog_revision: "fixture", pinned_offer_id: offer, selected_offer_id: offer,
          candidates: [{ offer_id: offer, reason: "eligible", estimated_usd_nanos: 7 }],
        };
        quotes.set(quote.id, quote);
        return Response.json(quote);
      }
      if (!refreshing && url.pathname.endsWith("/v1/requests")) {
        if (init.method === "GET") return Response.json({ requests: [], next_after: null });
        const body = JSON.parse(init.body);
        const id = requestId.slice(0, -1) + (requests.size + 1);
        const first = init.headers.get("Idempotency-Key").endsWith(":0");
        const earlierCharge = body.pinned_offer_id === "offer_a" && first;
        requests.set(id, {
          id, client_ref: body.client_ref, app_ref: body.app_ref, provider: "fixture-provider",
          status: "succeeded", cost_state: "provider_reported", quote: quotes.get(body.quote_id),
          events: [
            { kind: "accepted", at: "2026-11-25T00:00:00Z" },
            { kind: "provider_completed", at: observedAt },
          ],
          mock_result: { kind: "image", count: 1, aspect_ratio: "7:5" },
          upstream: {
            reported_usd_nanos: "8",
            provider_charge: earlierCharge ? { unit: "credits", quantity: "0.17", observed_at: observedAt } : undefined,
          },
        });
        return Response.json({ id });
      }
      const id = url.pathname.split("/").at(-1);
      assert.equal(init.method, "GET");
      assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
      assert.ok(requests.has(id));
      const trace = requests.get(id);
      if (!refreshing) return Response.json(trace);
      reads.push(id);
      if (id.endsWith("4") && denyLast) return Response.json({ ...trace, client_ref: "wrong_client" });
      const sameArm = trace.quote.selected_offer_id === "offer_a";
      return Response.json({
        ...trace, events: [], mock_result: null,
        cost_state: sameArm ? "supplier_settled" : "estimated",
        supplier_settlement: sameArm ? { revision: 1, amount_usd_nanos: "9", recorded_at: observedAt } : undefined,
        upstream: {
          provider_charge: {
            unit: sameArm ? "credits" : "units",
            quantity: sameArm ? (id.endsWith("1") ? "0.17" : "0.000000001") : "0",
            observed_at: observedAt,
          },
        },
      });
    },
  });

  const plan = await planOfferComparison(client, {
    runId: "fixture-refresh", input: image, offerIds: ["offer_a", "offer_b"], prompts: ["one", "two"],
    maxEstimatedUsdNanos: 28, clientRef: "studio_a", appRef: "fixture",
  });
  const report = await runOfferComparison(client, plan);
  assert.deepEqual(report[0].providerCharges, [
    { provider: "fixture-provider", unit: "credits", quantity: "0.17", observations: 1 },
  ]);
  assert.equal(report[1].providerCharges, undefined);
  assert.equal(report[0].spentUsdNanos.provider_reported, "16");

  refreshing = true;
  const savedPlan = JSON.parse(JSON.stringify(plan));
  for (const quote of savedPlan.quotes) quote.expires_at = "2000-01-01T00:00:00Z";
  const savedReport = JSON.parse(JSON.stringify(report));
  const wrongPlan = { ...savedPlan, runId: "unrelated-run" };
  await assert.rejects(refreshOfferComparison(client, wrongPlan, savedReport), /report does not match/);
  assert.deepEqual(reads, []);
  const refreshed = await refreshOfferComparison(client, savedPlan, savedReport);
  assert.deepEqual(reads.sort(), [...requests.keys()].sort());
  assert.deepEqual(refreshed[0].spentUsdNanos, { supplier_settled: "18", provider_reported: "0", mock_reported: "0" });
  assert.deepEqual(refreshed[0].providerCharges, [
    { provider: "fixture-provider", unit: "credits", quantity: "0.170000001", observations: 2 },
  ]);
  assert.equal(refreshed[0].medianCompletionMs, 1000);
  assert.deepEqual(refreshed[0].samples.map(row => row.output), report[0].samples.map(row => row.output));
  assert.equal(refreshed[1].samples[1].refreshErrorCode, "trace_evidence_mismatch");
  assert.equal(refreshed[1].spentUsdNanos.provider_reported, "16");
  assert.equal(savedReport[1].samples[1].refreshErrorCode, undefined);
  assert.equal(savedReport[0].spentUsdNanos.provider_reported, "16");

  denyLast = false;
  const retry = await refreshOfferComparison(client, savedPlan, refreshed);
  assert.equal(retry[1].samples[1].refreshErrorCode, undefined);
  assert.deepEqual(retry[1].providerCharges, [
    { provider: "fixture-provider", unit: "units", quantity: "0", observations: 2 },
  ]);
});


test("previews routing with authenticated request scope and an explicit price order", async () => {
  const client = new TokenCraft({
    ...options,
    fetch: async (url, init) => {
      assert.equal(url.pathname, `/edge/v1/requests/${requestId}/route-preview`);
      assert.equal(init.method, "POST");
      assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
      assert.equal(init.headers.get("Idempotency-Key"), null);
      assert.deepEqual(JSON.parse(init.body), { max_total_usd_nanos: 0, preferred_providers: [] });
      return Response.json({ observed_at: "2026-11-25T00:00:00Z", recorded: {}, current: null, unavailable_reason: "pinned_offer_unavailable" });
    },
  });
  const preview = await client.previewRoute(requestId, { maxTotalUsdNanos: 0, preferredProviders: [] });
  assert.equal(preview.current, null);
  assert.equal(preview.unavailable_reason, "pinned_offer_unavailable");
});

test("keeps metadata, warm content, and archived retrieval explicit", async () => {
  const progress = { state: "pending", ready_objects: 1, total_objects: 3,
    requested_at: "2026-12-25T00:00:00Z", retry_at: "2026-12-26T00:00:00Z" };
  const metadata = { phase: "archived", media: true, archive_at: null, expires_at: null, restore: progress };
  const saved = { phase: "available", archive_at: null, expires_at: null, result: { text: "Synthetic result" } };
  const replies = [Response.json(metadata), Response.json(saved), Response.json({
    error: { code: "archive_retrieval_required", message: "Restore the object first." },
  }, { status: 409 })];
  const routes = [];
  const client = new TokenCraft({ ...options, fetch: async (url, init) => {
    assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
    assert.equal(init.cache, "no-store");
    assert.equal(init.body, undefined);
    routes.push([init.method, url.pathname + url.search]);
    return replies.shift();
  } });
  assert.deepEqual(await client.contentMetadata(requestId), metadata);
  assert.deepEqual(await client.warmContent(requestId), saved);
  await assert.rejects(client.retrieveContent(requestId), { code: "archive_retrieval_required", requestId, status: 409 });
  assert.deepEqual(routes, [
    ["GET", `/edge/v1/requests/${requestId}/content?metadata_only=true`],
    ["GET", `/edge/v1/requests/${requestId}/content`],
    ["POST", `/edge/v1/requests/${requestId}/content/retrieve`],
  ]);
});

test("downloads only bounded artifacts matching their saved references", async () => {
  const bytes = new TextEncoder().encode("abc");
  const reference = { slot: 1, byte_length: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") };
  const binary = (body, headers = {}) => new Response(body, { headers: {
    "Content-Type": "image/png", "X-TokenCraft-Content-Sha256": reference.sha256, ...headers,
  } });
  const oversized = new ReadableStream({ start(controller) {
    controller.enqueue(bytes);
    controller.enqueue(Uint8Array.of(4));
    controller.close();
  } });
  const replies = [binary(bytes), binary(bytes), binary("abd"), binary(oversized),
    binary(bytes, { "Content-Length": "4" }), binary(bytes, { "Content-Type": "text/html" }),
    Response.json({ error: { code: "archive_retrieval_required" } }, { status: 409 })];
  const seen = [];
  const client = new TokenCraft({ ...options, fetch: async (url, init) => {
    seen.push(url.pathname);
    assert.equal(url.origin, "https://gateway.example");
    assert.equal(init.headers.get("Accept"), "application/octet-stream");
    assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
    assert.equal(init.redirect, "error");
    return replies.shift();
  } });
  for (const invalid of [{ ...reference, slot: 8 }, { ...reference, byte_length: 8 * 1024 * 1024 + 1 }]) {
    await assert.rejects(client.downloadArtifact(requestId, invalid), TypeError);
  }
  assert.equal(seen.length, 0);
  assert.deepEqual(await client.downloadArtifact(requestId, reference), { contentType: "image/png", bytes });
  const location = { location: "archived" };
  assert.deepEqual(await client.downloadArtifact(requestId, reference, location), { contentType: "image/png", bytes });
  assert.equal(seen[0], `/edge/v1/requests/${requestId}/artifacts/1`);
  assert.equal(seen[1], `/edge/v1/requests/${requestId}/content/artifacts/1`);
  for (let i = 0; i < 4; i++) {
    await assert.rejects(client.downloadArtifact(requestId, reference, location), { code: "invalid_artifact", requestId });
  }
  await assert.rejects(client.downloadArtifact(requestId, reference, location), { code: "archive_retrieval_required", requestId });
});

test("opted-in downloads retry scoped backpressure and verify the returned bytes", async () => {
  const bytes = new TextEncoder().encode("abc");
  const reference = { slot: 1, byte_length: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") };
  const project = requestId.replace("req_", "proj_");
  const workflow = requestId.replace("req_", "wf_");
  const run = requestId.replace("req_", "wfr_");
  const routes = [
    `/edge/v1/requests/${requestId}/artifacts/1`,
    `/edge/v1/requests/${requestId}/content/artifacts/1`,
    `/edge/v1/projects/${project}/workflow-endpoints/${workflow}/runs/${run}/outputs/captions`,
  ];
  const seen = [];
  const client = new TokenCraft({ ...options, downloadMaxAttempts: 2, fetch: async (url, init) => {
    const index = seen.length;
    seen.push(url.pathname);
    assert.equal(init.method, "GET");
    assert.equal(init.body, undefined);
    assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
    assert.equal(init.headers.get("Accept"), "application/octet-stream");
    assert.equal(init.cache, "no-store");
    if (index % 2 === 0) {
      return Response.json({ error: { code: "artifact_download_capacity" } }, {
        status: index === 2 ? 429 : 503,
        headers: { "Retry-After": index === 4 ? "Sun, 06 Nov 1994 08:49:37 GMT" : "0" },
      });
    }
    return new Response(bytes, { headers: {
      "Content-Type": index === 5 ? "text/vtt" : "image/png",
      "X-TokenCraft-Content-Sha256": reference.sha256,
    } });
  } });
  assert.deepEqual(await client.downloadArtifact(requestId, reference), { contentType: "image/png", bytes });
  assert.deepEqual(await client.downloadArtifact(requestId, reference, { location: "archived" }), { contentType: "image/png", bytes });
  assert.deepEqual(await client.workflow(project, workflow).downloadOutput(run, "captions"), { contentType: "text/vtt", bytes });
  assert.deepEqual(seen, routes.flatMap((route) => [route, route]));
});

test("downloads honor attempt limits, valid retry hints, and cancellation", async () => {
  const reference = { slot: 0, byte_length: 3, sha256: "a".repeat(64) };
  for (const [downloadMaxAttempts, status, hint, expectedCalls] of [
    [undefined, 503, "0", 1], [2, 503, undefined, 1], [2, 503, "0, 1", 1],
    [2, 409, "0", 1], [2, 503, "0", 2],
  ]) {
    let calls = 0;
    const client = new TokenCraft({ ...options, downloadMaxAttempts, submitMaxAttempts: 3, fetch: async () => {
      calls++;
      return Response.json({ error: { code: "fixture" } }, { status, headers: hint ? { "Retry-After": hint } : {} });
    } });
    await assert.rejects(client.downloadArtifact(requestId, reference), { code: "fixture", status, requestId });
    assert.equal(calls, expectedCalls);
  }
  assert.throws(() => new TokenCraft({ ...options, downloadMaxAttempts: 9 }), RangeError);

  let transportCalls = 0;
  const transport = new TokenCraft({ ...options, downloadMaxAttempts: 2, fetch: async () => {
    transportCalls++;
    throw new TypeError("fixture connection closed");
  } });
  await assert.rejects(transport.downloadArtifact(requestId, reference), { code: "transport_error", requestId });
  assert.equal(transportCalls, 1);

  const controller = new AbortController();
  const reason = new Error("stop waiting for output");
  let calls = 0;
  const client = new TokenCraft({ ...options, downloadMaxAttempts: 2, timeoutMs: 50, fetch: async () => {
    if (++calls === 2) setTimeout(() => controller.abort(reason), 0);
    return Response.json({}, { status: 503, headers: { "Retry-After": "60" } });
  } });
  await assert.rejects(client.downloadArtifact(requestId, reference), { name: "TokenCraftTimeoutError", requestId });
  await assert.rejects(client.downloadArtifact(requestId, reference, { signal: controller.signal }), (error) => error === reason);
  assert.equal(calls, 2);
});

test("published workflows replay the same intent and poll through reconciliation", async () => {
  const project = requestId.replace("req_", "proj_");
  const workflow = requestId.replace("req_", "wf_");
  const run = requestId.replace("req_", "wfr_");
  const prior = run.replace(/0$/, "1");
  const path = `/v1/projects/${project}/workflow-endpoints/${workflow}/runs`;
  const statusUrl = `${path}/${run}`;
  const status = (state) => ({ id: run, status: state, workflow_version: 3, steps: [],
    draft_revision: 8, created_at: "2026-11-26T00:00:00Z", client_ref: "studio.23", error: null });
  const responses = [
    Response.json({}, { status: 429, headers: { "Retry-After": "0" } }),
    Response.json({ id: run, status_url: statusUrl, workflow_version: 3, replayed: false }, { status: 202 }),
    Response.json(status("reconciling"), { headers: { ETag: '"first"' } }),
    new Response(null, { status: 304 }),
    Response.json(status("expired")),
    new Response(null, { status: 204 }),
  ];
  const received = [];
  const client = new TokenCraft({ ...options, submitMaxAttempts: 2, fetch: async (url, init) => {
    assert.equal(url.origin, "https://gateway.example");
    assert.equal(init.redirect, "error");
    assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
    received.push([url.pathname, init.method, init.headers.get("Idempotency-Key"), init.headers.get("If-None-Match"), init.body]);
    return responses.shift();
  } });
  const endpoint = client.workflow(project, workflow);
  const receipt = await endpoint.invoke({ brief: "Test shot" }, {
    idempotencyKey: "workflow-key-a", clientRef: "studio.23", reuseRunId: prior,
  });
  assert.deepEqual(received[0], received[1]);
  assert.equal(received[0][0], `/edge${path}`);
  assert.equal(received[0][2], "workflow-key-a");
  assert.deepEqual(JSON.parse(received[0][4]), { inputs: { brief: "Test shot" }, client_ref: "studio.23", reuse_run_id: prior });
  assert.deepEqual(await endpoint.wait(receipt.id, { pollIntervalMs: 1, timeoutMs: 500 }), status("expired"));
  assert.deepEqual(received.slice(2, 5).map((entry) => entry[3]), [null, '"first"', '"first"']);
  await endpoint.cancel(run);
  assert.deepEqual(received[5], [`/edge${statusUrl}/cancel`, "POST", null, null, undefined]);
});

test("published workflow uploads scope binary bodies and named output downloads check size and digest", async () => {
  const project = requestId.replace("req_", "proj_");
  const workflow = requestId.replace("req_", "wf_");
  const run = requestId.replace("req_", "wfr_");
  const path = `/v1/projects/${project}/workflow-endpoints/${workflow}`;
  const uploadId = requestId.replace("req_", "wfu_");
  const sha = createHash("sha256").update("abc").digest("hex");
  let calls = 0;
  const client = new TokenCraft({ ...options, fetch: async (url, init) => {
    assert.equal(url.origin, "https://gateway.example");
    assert.equal(init.cache, "no-store");
    assert.equal(init.headers.get("Authorization"), "Bearer fixture_key");
    calls += 1;
    if (calls === 1) {
      assert.equal(url.pathname, `/edge${path}/uploads`);
      assert.equal(init.headers.get("Content-Type"), "text/plain");
      assert.equal(init.headers.get("X-TokenCraft-Content-Sha256"), sha);
      assert.equal(Buffer.from(init.body).toString(), "abc");
      return Response.json({ upload_id: uploadId, expires_at: "2026-11-26T00:00:00Z" }, { status: 201 });
    }
    if (calls === 2) {
      assert.deepEqual(JSON.parse(init.body), { inputs: { document: { upload_id: uploadId } } });
      return Response.json({ id: run, status_url: "https://outside.example/run", workflow_version: 1, replayed: false }, { status: 202 });
    }
    assert.equal(url.pathname, `/edge${path}/runs/${run}/outputs/captions`);
    assert.equal(init.headers.get("Accept"), "application/octet-stream");
    const headers = { "Content-Type": "text/vtt", "X-TokenCraft-Content-Sha256": sha };
    if (calls === 5) headers["Content-Length"] = String(32 * 1024 * 1024 + 1);
    return new Response(calls === 4 ? "bad" : "abc", { headers });
  } });
  const endpoint = client.workflow(project, workflow);
  assert.throws(() => client.workflow(`${project}/..`, workflow), TypeError);
  await assert.rejects(endpoint.downloadOutput(run, "../secret"), TypeError);
  const bytes = new TextEncoder().encode("abc");
  const uploading = endpoint.upload("text/plain", bytes);
  bytes.fill(0);
  const document = await uploading;
  await assert.rejects(endpoint.invoke({ document }, { idempotencyKey: "workflow-fixture" }), { code: "invalid_response" });
  const valid = await endpoint.downloadOutput(run, "captions");
  assert.equal(valid.contentType, "text/vtt");
  assert.equal(Buffer.from(valid.bytes).toString(), "abc");
  await assert.rejects(endpoint.downloadOutput(run, "captions"), { code: "invalid_artifact" });
  await assert.rejects(endpoint.downloadOutput(run, "captions"), { code: "invalid_artifact" });
  assert.equal(calls, 5);
});
