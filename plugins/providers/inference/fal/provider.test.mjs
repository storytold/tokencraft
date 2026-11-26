import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, randomBytes, sign } from 'node:crypto';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { queueServer, readJson, send } from './queue.mjs';
import { fal } from './provider.mjs';
import { falWebhooks } from './fal-webhooks.mjs';

const model = 'fal-ai/flux/dev';
const authorization = 'Key test-vendor-key';
const jobInput = { kind: 'image', prompt: 'a square', count: 1, aspect_ratio: '1:1' };
const submitPath = '/fal-ai/flux/dev';
const statusPath = '/fal-ai/flux/requests/vendor-task/status';
const resultPath = '/fal-ai/flux/requests/vendor-task';
const artifactType = 'image/png';
const acknowledgement = { request_id: 'vendor-task' };
const statusReply = { status: 'COMPLETED' };
const resultReply = (url) => ({ images: [{ url }] });
const check = (body) => {
  assert.equal(body.num_images, 1);
  assert.deepEqual(body.image_size, { width: 1024, height: 1024 });
  assert.equal(body.output_format, 'png');
};

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return `http://127.0.0.1:${server.address().port}`;
}

async function close(server) {
  if (!server?.listening) return;
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}

test('retries a confirmed throttle once across a restart', async () => {
  const state = await mkdtemp(join(tmpdir(), 'tokencraft-plugin-retry-'));
  const calls = new Map();
  let plugin;
  const vendor = createServer(async (request, response) => {
    assert.equal(request.url, submitPath);
    assert.equal(request.headers.authorization, authorization);
    const body = await readJson(request);
    const prompt = body.prompt;
    const attempt = (calls.get(prompt) ?? 0) + 1;
    calls.set(prompt, attempt);
    if (prompt === 'ambiguous') return send(response, 502, {});
    if (prompt === 'always throttled' || attempt === 1) return send(response, 429, {});
    return send(response, 200, acknowledgement);
  });
  try {
    const origin = await listen(vendor);
    const provider = fal({ origin, key: 'test-vendor-key', artifactOrigins: [origin] });
    const start = async () => {
      plugin = await queueServer({ state, token: 'test-gateway-key', provider });
      return listen(plugin);
    };
    let base = await start();
    const submit = async (id, prompt) => (await (await fetch(`${base}/v1/jobs`, {
      method: 'POST', headers: { authorization: 'Bearer test-gateway-key', 'idempotency-key': id },
      body: JSON.stringify({ version: 1, request_id: id, model, input: { ...jobInput, prompt } }),
    })).json()).outcome;
    const unavailable = { status: 'unavailable', retry_after_ms: 2_000 };
    assert.deepEqual(await submit('recover', 'recover'), unavailable);
    assert.deepEqual(await submit('recover', 'recover'), unavailable);
    assert.equal(calls.get('recover'), 1);
    assert.deepEqual(await submit('limited', 'always throttled'), unavailable);
    assert.deepEqual(await submit('unknown', 'ambiguous'), { status: 'uncertain' });

    await close(plugin);
    base = await start();
    await new Promise((resolve) => setTimeout(resolve, 2_050));
    const concurrent = await Promise.all(Array.from({ length: 4 }, () => submit('recover', 'recover')));
    assert.ok(concurrent.some((outcome) => outcome.status === 'accepted'));
    assert.deepEqual(await submit('recover', 'recover'), { status: 'accepted', handle: 'recover' });
    assert.deepEqual(await submit('limited', 'always throttled'), { status: 'unavailable' });
    assert.deepEqual(await submit('limited', 'always throttled'), { status: 'unavailable' });
    assert.deepEqual(await submit('unknown', 'ambiguous'), { status: 'uncertain' });
    assert.deepEqual(Object.fromEntries(calls), { recover: 2, 'always throttled': 2, ambiguous: 1 });
  } finally {
    await close(plugin);
    await close(vendor);
    await rm(state, { recursive: true });
  }
});

test('maps queued work, persists the handle on restart, and fetches artifact bytes', async () => {
  const state = await mkdtemp(join(tmpdir(), 'tokencraft-plugin-'));
  let plugin;
  let submitCount = 0;
  let resultReads = 0;
  let vendorOrigin;
  const vendor = createServer(async (request, response) => {
    if (request.url !== '/artifact') assert.equal(request.headers.authorization, authorization);
    if (request.url === submitPath && request.method === 'POST') {
      submitCount += 1;
      check(await readJson(request));
      return send(response, 200, acknowledgement);
    }
    if (request.url === statusPath) return send(response, 200, statusReply);
    if (request.url === resultPath) {
      resultReads += 1;
      return send(response, 200, resultReply(`${vendorOrigin}/artifact`));
    }
    if (request.url === '/artifact') {
      assert.equal(request.headers.authorization, undefined);
      response.writeHead(200, { 'content-type': artifactType });
      return response.end('fixture artifact');
    }
    return send(response, 404, {});
  });
  try {
    vendorOrigin = await listen(vendor);
    const provider = fal({ origin: vendorOrigin, key: 'test-vendor-key', artifactOrigins: [vendorOrigin] });
    const start = async () => {
      plugin = await queueServer({ state, token: 'test-gateway-key', provider });
      return listen(plugin);
    };
    let base = await start();
    const headers = { authorization: 'Bearer test-gateway-key', 'idempotency-key': 'example-1' };
    const job = { version: 1, request_id: 'example-1', model, input: jobInput };
    const submit = (body = job) => fetch(`${base}/v1/jobs`, { method: 'POST', headers, body: JSON.stringify(body) });
    const accepted = { version: 1, request_id: 'example-1', outcome: { status: 'accepted', handle: 'example-1' } };
    assert.deepEqual(await (await submit()).json(), accepted);
    assert.deepEqual(await (await submit()).json(), accepted);
    assert.equal((await (await submit({ ...job, input: { ...jobInput, prompt: 'changed' } })).json()).outcome.status,
      'uncertain');
    assert.equal(submitCount, 1);
    await close(plugin);
    const recordPath = join(state, 'example-1.json');
    const record = JSON.parse(await readFile(recordPath, 'utf8'));
    delete record.model;
    await writeFile(recordPath, JSON.stringify(record));
    base = await start();
    assert.deepEqual(await (await submit()).json(), accepted);
    assert.equal(submitCount, 1);
    assert.deepEqual(await (await fetch(`${base}/v1/jobs/example-1`, { headers })).json(),
      { version: 1, handle: 'example-1', status: 'succeeded' });
    assert.equal(resultReads, 0);
    assert.equal((await fetch(`${base}/v1/jobs/example-1/artifacts/0`)).status, 401);
    const artifact = await fetch(`${base}/v1/jobs/example-1/artifacts/0`, { headers });
    assert.equal(artifact.headers.get('content-type'), artifactType);
    assert.equal(await artifact.text(), 'fixture artifact');
    assert.equal(resultReads, 1);
  } finally {
    await close(plugin);
    await close(vendor);
    await rm(state, { recursive: true });
  }
});

test('pins the selected media model across restart and limits its artifact downloads', async () => {
  const state = await mkdtemp(join(tmpdir(), 'tokencraft-fal-media-'));
  const prompt = 'media fixture';
  const examples = [
    {
      id: 'video', model: 'fal-ai/pixverse/v6/text-to-video', path: 'fal-ai/pixverse',
      input: { kind: 'video', prompt, duration_seconds: 5, aspect_ratio: '16:9', resolution: '720p' },
      payload: { prompt, duration: 5, aspect_ratio: '16:9', resolution: '720p', enable_safety_checker: true },
      mediaType: 'video/mp4', result: (url) => ({ video: { url } }),
    },
    {
      id: 'audio', model: 'fal-ai/stable-audio-25/text-to-audio', path: 'fal-ai/stable-audio-25',
      input: { kind: 'audio', prompt, duration_seconds: 10, format: 'wav' },
      payload: { prompt, seconds_total: 10, enable_safety_checker: true },
      mediaType: 'audio/x-wav', result: (url) => ({ audio: url }),
    },
    {
      id: 'mesh', model: 'fal-ai/meshy/v6/text-to-3d', path: 'fal-ai/meshy',
      input: { kind: 'mesh', prompt, format: 'glb' },
      payload: { prompt, mode: 'preview', should_remesh: true, target_polycount: 10_000, enable_safety_checker: true },
      mediaType: 'model/gltf-binary', result: (url) => ({ model_glb: { url } }),
    },
  ];
  const byId = new Map(examples.map((example) => [example.id, example]));
  const submits = [];
  let plugin, vendorOrigin;
  let delivery = 'valid';
  const vendor = createServer(async (request, response) => {
    const { url } = request;
    if (url.startsWith('/artifacts/')) {
      assert.equal(request.headers.authorization, undefined);
      const example = byId.get(url.split('/').at(-1));
      if (delivery === 'wrong-type') return response.writeHead(200, { 'content-type': 'text/html' }).end('wrong');
      if (delivery === 'oversize') {
        return response.writeHead(200, { 'content-type': example.mediaType, 'content-length': 9 * 1024 * 1024 }).end();
      }
      response.writeHead(200, { 'content-type': example.mediaType });
      if (delivery === 'large-stream') return response.end(Buffer.alloc(8 * 1024 * 1024 + 1));
      return response.end(example.id);
    }
    assert.equal(request.headers.authorization, authorization);
    for (const example of examples) {
      if (request.method === 'POST' && url === `/${example.model}`) {
        const payload = await readJson(request);
        submits.push(example.id);
        assert.deepEqual(payload, example.payload);
        return send(response, 200, { request_id: example.id });
      }
      const path = `/${example.path}/requests/${example.id}`;
      if (url === `${path}/status`) {
        const status = delivery === 'failed-status' ? { ...statusReply, error_type: 'fixture_error' } : statusReply;
        return send(response, 200, status);
      }
      if (url === path) {
        assert.notEqual(delivery, 'failed-status');
        const origin = delivery === 'unrecognized-origin' ? 'http://127.0.0.1:1' : vendorOrigin;
        return send(response, 200, example.result(`${origin}/artifacts/${example.id}`));
      }
    }
    return send(response, 404, {});
  });
  try {
    vendorOrigin = await listen(vendor);
    const provider = fal({ origin: vendorOrigin, key: 'test-vendor-key', artifactOrigins: [vendorOrigin] });
    const start = async () => {
      plugin = await queueServer({ state, token: 'test-gateway-key', provider });
      return listen(plugin);
    };
    const headers = { authorization: 'Bearer test-gateway-key' };
    let base = await start();
    const submit = async (id, { model, input }) => {
      const response = await fetch(`${base}/v1/jobs`, { method: 'POST',
        headers: { ...headers, 'idempotency-key': id },
        body: JSON.stringify({ version: 1, request_id: id, model, input }) });
      return (await response.json()).outcome.status;
    };
    const video = examples[0];
    assert.equal(await submit('malformed', { ...video, input: { ...video.input, resolution: '1080p' } }), 'rejected');
    assert.equal(await submit('malformed', { ...video, input: { ...video.input, prompt: '🌊'.repeat(513) } }), 'rejected');
    assert.equal(await submit('unknown', { ...video, model: 'https://unexpected.test' }), 'rejected');
    for (const example of examples) {
      assert.equal(await submit(example.id, example), 'accepted');
    }
    assert.equal(await submit('video', examples[1]), 'uncertain');
    await close(plugin);
    base = await start();
    const artifact = (id, slot = 0) => fetch(`${base}/v1/jobs/${id}/artifacts/${slot}`, { headers });
    for (const example of examples) {
      assert.equal(await submit(example.id, example), 'accepted');
      const poll = await fetch(`${base}/v1/jobs/${example.id}`, { headers });
      assert.equal((await poll.json()).status, 'succeeded');
      const output = await artifact(example.id);
      assert.equal(output.headers.get('content-type'), example.id === 'audio' ? 'audio/wav' : example.mediaType);
      assert.equal(await output.text(), example.id);
      assert.equal((await artifact(example.id, 1)).status, 404);
    }
    assert.deepEqual(submits, ['video', 'audio', 'mesh']);
    delivery = 'failed-status';
    const failed = await fetch(`${base}/v1/jobs/video`, { headers });
    assert.equal((await failed.json()).status, 'failed');
    assert.equal((await artifact('video')).status, 404);
    for (const failure of ['unrecognized-origin', 'wrong-type', 'oversize']) {
      delivery = failure;
      assert.equal((await artifact('video')).status, 502);
    }
    delivery = 'large-stream';
    await assert.rejects(async () => (await artifact('video')).arrayBuffer());
  } finally {
    await close(plugin);
    await close(vendor);
    await rm(state, { recursive: true });
  }
});

test('recovers lost fal acknowledgements with signed webhooks and retries a lost relay after restart', async () => {
  const state = await mkdtemp(join(tmpdir(), 'tokencraft-fal-webhook-'));
  const signer = generateKeyPairSync('ed25519');
  const rotated = generateKeyPairSync('ed25519');
  const user = 'fixture-fal-user';
  const callbackOrigin = 'https://fal-callback.fixture.test';
  const callbacks = new Map();
  const submitted = [];
  const relayed = [];
  let plugin, vendorOrigin, pluginOrigin;
  let key = signer.publicKey;
  let jwksReads = 0;
  let jwksAvailable = true;
  let loseRelay = true;
  let earlyCallback;
  let elapsed = 0;
  const now = Date.now();
  const gateway = createServer(async (request, response) => {
    relayed.push({ path: request.url, token: request.headers.authorization, body: await readJson(request) });
    if (loseRelay) return response.destroy();
    return response.writeHead(204).end();
  });
  const vendor = createServer(async (request, response) => {
    const url = new URL(request.url, vendorOrigin);
    if (url.pathname === '/jwks') {
      jwksReads += 1;
      assert.equal(request.headers.authorization, undefined);
      if (!jwksAvailable) return send(response, 503, {});
      return send(response, 200, { keys: [key.export({ format: 'jwk' })] });
    }
    if (url.pathname === submitPath && request.method === 'POST') {
      assert.equal(request.headers.authorization, authorization);
      const body = await readJson(request);
      check(body);
      submitted.push(body.prompt);
      const completion = new URL(url.searchParams.get('fal_webhook'));
      assert.equal(completion.origin, callbackOrigin);
      callbacks.set(body.prompt, completion.pathname);
      if (body.prompt === 'lost') return response.destroy();
      await earlyCallback();
      return send(response, 200, { request_id: 'failed-task' });
    }
    if (url.pathname === statusPath) return send(response, 200, statusReply);
    if (url.pathname === resultPath) return send(response, 200, resultReply(`${vendorOrigin}/artifact`));
    if (url.pathname === '/artifact') {
      assert.equal(request.headers.authorization, undefined);
      return response.writeHead(200, { 'content-type': artifactType }).end('fixture artifact');
    }
    return send(response, 404, {});
  });
  try {
    vendorOrigin = await listen(vendor);
    const gatewayOrigin = await listen(gateway);
    const provider = fal({ origin: vendorOrigin, key: 'test-vendor-key', artifactOrigins: [vendorOrigin] });
    const start = async () => {
      const webhooks = falWebhooks({ callbackOrigin, gatewayOrigin, user, jwksUrl: `${vendorOrigin}/jwks`,
        now: () => now, elapsed: () => elapsed });
      plugin = await queueServer({ state, token: 'test-gateway-key', provider, webhooks });
      pluginOrigin = await listen(plugin);
    };
    await start();
    const headers = { authorization: 'Bearer test-gateway-key' };
    const submit = (id, completion) => fetch(`${pluginOrigin}/v1/jobs`, { method: 'POST',
      headers: { ...headers, 'idempotency-key': id },
      body: JSON.stringify({ version: 1, request_id: id, model, input: { ...jobInput, prompt: id }, completion }) });
    const lookup = (id, auth = headers) => fetch(`${pluginOrigin}/v1/jobs/by-request/${id}`, { headers: auth });
    const receive = (url, options = {}) => {
      const id = options.id ?? 'vendor-task';
      const account = options.user ?? user;
      const stamp = String(Math.floor(now / 1000) + (options.delta ?? 0));
      const data = JSON.stringify({ request_id: options.bodyId ?? id, gateway_request_id: 'fal-retry-id',
        status: options.status ?? 'OK', payload: { images: [{ url: `${vendorOrigin}/incorrect` }] } });
      const digest = createHash('sha256').update(data).digest('hex');
      const signature = sign(null, Buffer.from([id, account, stamp, digest].join('\n')),
        (options.key ?? signer).privateKey).toString('hex');
      return fetch(`${pluginOrigin}${url}`, { method: 'POST', headers: { 'content-type': 'application/json',
        'x-fal-webhook-request-id': id, 'x-fal-webhook-user-id': account, 'x-fal-webhook-timestamp': stamp,
        'x-fal-webhook-signature': signature }, body: options.alter ? data + ' ' : data });
    };
    const grant = (id) => ({ path: `/v1/connector-jobs/${id}/completion`, token: randomBytes(32).toString('hex') });
    const lost = grant('lost');
    assert.equal((await submit('lost', { ...lost, path: 'https://unexpected.test/' })).status, 400);
    assert.equal((await (await submit('lost', lost)).json()).outcome.status, 'uncertain');
    assert.equal((await (await submit('lost', lost)).json()).outcome.status, 'uncertain');
    assert.equal((await lookup('lost')).status, 404);
    const callback = callbacks.get('lost');
    assert.ok(callback);
    const forged = callback.replace(/.$/, (last) => last === '0' ? '1' : '0');
    assert.equal((await receive(forged)).status, 404);
    assert.equal((await receive(callback, { delta: -301 })).status, 401);
    assert.equal((await receive(callback, { user: 'another-account' })).status, 401);
    assert.equal(jwksReads, 0);
    assert.equal((await receive(callback, { alter: true })).status, 401);
    assert.equal((await receive(callback, { bodyId: 'other' })).status, 401);
    assert.equal(jwksReads, 1);
    assert.equal((await lookup('lost')).status, 404);
    assert.equal((await receive(callback)).status, 502);
    assert.equal(relayed.length, 1);
    assert.equal((await lookup('lost', {})).status, 401);
    assert.deepEqual(await (await lookup('lost')).json(), { version: 1, request_id: 'lost', handle: 'lost' });

    await close(plugin);
    await start();
    loseRelay = false;
    const duplicates = await Promise.all(Array.from({ length: 12 }, () => receive(callback)));
    assert.ok(duplicates.every((response) => response.status === 204));
    assert.equal((await receive(callback, { id: 'other-job' })).status, 409);
    assert.deepEqual(relayed.slice(0, 2), [0, 1].map(() => ({ path: lost.path, token: `Bearer ${lost.token}`,
      body: { version: 1 } })));
    assert.equal(relayed.length, 2);
    assert.equal(await (await fetch(`${pluginOrigin}/v1/jobs/lost/artifacts/0`, { headers })).text(), 'fixture artifact');
    assert.equal((await (await submit('lost', lost)).json()).outcome.status, 'accepted');

    const failed = grant('early-failure');
    key = rotated.publicKey;
    elapsed = 31_000;
    earlyCallback = async () => {
      const url = callbacks.get('early-failure');
      assert.equal((await receive(url, { id: 'failed-task', status: 'ERROR', key: rotated })).status, 204);
    };
    assert.equal((await (await submit('early-failure', failed)).json()).outcome.status, 'accepted');
    assert.deepEqual(await (await fetch(`${pluginOrigin}/v1/jobs/early-failure`, { headers })).json(),
      { version: 1, handle: 'early-failure', status: 'failed' });
    assert.equal((await receive(callback)).status, 401);
    assert.equal(jwksReads, 3);
    jwksAvailable = false;
    elapsed += 16 * 60_000;
    assert.equal((await receive(callbacks.get('early-failure'), { id: 'failed-task', status: 'ERROR',
      key: rotated })).status, 502);
    assert.equal(jwksReads, 4);
    jwksAvailable = true;
    elapsed += 31_000;
    assert.equal((await receive(callbacks.get('early-failure'), { id: 'failed-task', status: 'ERROR',
      key: rotated })).status, 204);
    assert.equal(jwksReads, 5);
    assert.deepEqual(submitted, ['lost', 'early-failure']);
    assert.equal(relayed.length, 3);
  } finally {
    await close(plugin);
    await close(gateway);
    await close(vendor);
    await rm(state, { recursive: true });
  }
});
