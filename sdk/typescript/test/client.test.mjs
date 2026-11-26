import assert from "node:assert/strict";
import { createHash, createHmac } from "node:crypto";
import test from "node:test";
import {
  TokenCraft, TokenCraftError, TokenCraftTimeoutError, planOfferComparison, refreshOfferComparison, runOfferComparison, verifyWebhook,
} from "@tokencraft/sdk";

const requestId = "req_01m881w0c8np13xfvetqsbk8m0";
const traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
const image = { model: "studio_image_demo", workload: { kind: "image", count: 1, aspect_ratio: "7:5" } };
const options = { baseUrl: "https://gateway.example/edge/", apiKey: "fixture_key" };

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
    return Response.json({ revision: "fixture", models: [model] });
  };
  const client = new TokenCraft({ ...options, fetch });
  assert.deepEqual((await client.models()).models, [model]);
  await client.models({ scope: "public" });
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
  await assert.rejects(verifyWebhook(Buffer.from(JSON.stringify(event)), headers, secret(first), { now }), { code: "signature" });
  await assert.rejects(verifyWebhook(body, headers, secret(first), { now: now + 301 }), { code: "timestamp" });
  await assert.rejects(verifyWebhook(body, headers, secret(Buffer.alloc(32)), { now }), { code: "signature" });
  await assert.rejects(verifyWebhook(body, headers, undefined, { now }), { code: "secret" });
  const mismatched = Buffer.from(JSON.stringify({ ...event, id: "another_event" }));
  await assert.rejects(verifyWebhook(mismatched, { ...headers, "Webhook-Signature": sign(first, mismatched) }, secret(first), { now }), { code: "payload" });
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
