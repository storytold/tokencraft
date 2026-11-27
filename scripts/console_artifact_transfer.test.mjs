import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { once } from "node:events";
import { createServer } from "node:http";
import test from "node:test";
import { readArtifact } from "../frontend/src/lib/artifacts.ts";

const path = "/v1/requests/req_fixture/content/artifacts/0?account_id=acct_fixture&project_id=proj_fixture";
const fixtureUrl = "https://gateway.example" + path;
const bytes = Buffer.from("abc");
const digest = createHash("sha256").update(bytes).digest("hex");
const reference = { byte_length: bytes.length, sha256: digest };
const contentHeaders = {
  "Content-Type": "image/png",
  "Content-Length": String(bytes.length),
  "X-TokenCraft-Content-Sha256": digest,
};
const busy = (hint, status = 503) => new Response("busy", {
  status, headers: hint === undefined ? {} : { "Retry-After": hint },
});

test("recovers a scoped transfer interrupted after a valid busy response", { timeout: 5_000 }, async (t) => {
  const seen = [];
  const arrivalTimes = [];
  const server = createServer((request, response) => {
    seen.push({ method: request.method, url: request.url, accept: request.headers.accept });
    arrivalTimes.push(performance.now());
    switch (seen.length) {
      case 1:
        response.writeHead(503, { "Retry-After": "1" });
        response.end();
        break;
      case 2:
        response.writeHead(200, { ...contentHeaders, Connection: "close" });
        response.write(bytes.subarray(0, 1), () => request.socket.end());
        break;
      default:
        response.writeHead(200, contentHeaders);
        response.end(bytes);
    }
  });
  server.listen(0, "127.0.0.1");
  t.after(() => {
    server.closeAllConnections();
    return new Promise((resolve) => server.close(resolve));
  });
  await once(server, "listening");

  const signal = new AbortController().signal;
  const notices = [];
  const realFetch = globalThis.fetch;
  let transferSignal;
  t.mock.method(globalThis, "fetch", (url, init) => {
    assert.equal(init.credentials, "include");
    assert.equal(init.redirect, "error");
    assert.equal(init.cache, "no-store");
    transferSignal ??= init.signal;
    assert.equal(init.signal, transferSignal);
    return realFetch(url, init);
  });
  const file = await readArtifact(`http://127.0.0.1:${server.address().port}${path}`,
    signal, (reason) => notices.push(reason), reference, "image/png");
  assert.deepEqual(Buffer.from(await file.arrayBuffer()), bytes);
  assert.equal(file.type, "image/png");
  assert.deepEqual(notices, ["busy", undefined, "interrupted", undefined]);
  assert.ok(arrivalTimes[1] - arrivalTimes[0] >= 990, "retry must observe the gateway delay");
  assert.deepEqual(seen, Array.from({ length: 3 }, () => ({
    method: "GET", url: path, accept: "application/octet-stream",
  })));
});

test("retries a connection failure before headers and stops at the total attempt limit", async (t) => {
  const notices = [];
  const fetch = t.mock.method(globalThis, "fetch", async () => {
    throw new TypeError("connection closed");
  });
  await assert.rejects(readArtifact(fixtureUrl, new AbortController().signal,
    (reason) => notices.push(reason), reference), /connection was interrupted/);
  assert.equal(fetch.mock.callCount(), 3);
  assert.deepEqual(notices, ["interrupted", undefined, "interrupted", undefined]);
});

test("retries short valid HTTP-date and numeric hints within one attempt budget", async (t) => {
  const replies = [busy("Sun, 06 Nov 1994 08:49:37 GMT", 429), busy("0"), busy("0")];
  const fetch = t.mock.method(globalThis, "fetch", async () => replies.shift());
  const notices = [];
  await assert.rejects(readArtifact(fixtureUrl, new AbortController().signal,
    (reason) => notices.push(reason), reference), /Downloads are busy/);
  assert.equal(fetch.mock.callCount(), 3);
  assert.deepEqual(notices, ["busy", undefined, "busy", undefined]);
});

test("keeps invalid hints, scope failures, and corrupted bytes terminal", async (t) => {
  const responses = [
    [() => busy(), /temporarily unavailable/],
    [() => busy("0, 1"), /Downloads are busy/],
    [() => busy("6"), /Downloads are busy/],
    [() => busy("0", 403), /does not have access/],
    [() => busy("0", 409), /restored from cold storage/],
    [() => new Response("abd", { headers: contentHeaders }), /failed verification/],
  ];
  let reply;
  const fetch = t.mock.method(globalThis, "fetch", async () => reply());
  for (const [response, problem] of responses) {
    reply = response;
    const before = fetch.mock.callCount();
    await assert.rejects(readArtifact(fixtureUrl, new AbortController().signal,
      () => assert.fail("terminal responses cannot retry"), reference), problem);
    assert.equal(fetch.mock.callCount(), before + 1);
  }
});

test("cancellation and the overall deadline stop capacity waits without another request", async (t) => {
  let deadline;
  t.mock.method(AbortSignal, "timeout", (ms) => {
    assert.equal(ms, 60_000);
    deadline = new AbortController();
    return deadline.signal;
  });
  const fetch = t.mock.method(globalThis, "fetch", async () => busy("5"));
  for (const callerCancels of [true, false]) {
    const before = fetch.mock.callCount();
    const caller = new AbortController();
    const failure = callerCancels ? new Error("cancel clicked") : new DOMException("deadline reached", "TimeoutError");
    const transfer = readArtifact(fixtureUrl, caller.signal, (reason) => {
      assert.equal(reason, "busy");
      if (callerCancels) caller.abort(failure);
      else deadline.abort(failure);
    }, reference);
    await assert.rejects(transfer, (cause) => cause === failure);
    assert.equal(fetch.mock.callCount(), before + 1);
  }
});
