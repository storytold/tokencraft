import assert from 'node:assert/strict';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { queueServer as falQueue } from '../plugins/providers/inference/fal/queue.mjs';
import { fal } from '../plugins/providers/inference/fal/provider.mjs';
import { queueServer as kinoviQueue } from '../plugins/providers/inference/kinovi/queue.mjs';
import { kinovi } from '../plugins/providers/inference/kinovi/provider.mjs';

const cases = {
  fal: {
    provider: fal, queue: falQueue, model: 'fal-ai/flux/dev',
    input: { kind: 'image', prompt: 'A square', count: 1, aspect_ratio: '1:1' },
    submitPath: '/fal-ai/flux/dev', pollPath: '/fal-ai/flux/requests/vendor-task/status',
    acknowledgement: { request_id: 'vendor-task' },
    completed: (artifact) => [
      ['/fal-ai/flux/requests/vendor-task/status', { status: 'COMPLETED' }],
      ['/fal-ai/flux/requests/vendor-task', { images: [{ url: artifact }] }],
    ],
    media: 'image/png',
  },
  kinovi: {
    provider: kinovi, queue: kinoviQueue, model: 'seedance-20',
    input: { kind: 'video', prompt: 'A square', duration_seconds: 5, aspect_ratio: '16:9', resolution: '720p' },
    submitPath: '/api/v1/jobs/createTask', pollPath: '/api/v1/jobs/recordInfo?taskId=vendor-task',
    acknowledgement: { taskId: 'vendor-task' },
    completed: (artifact) => [
      ['/api/v1/jobs/recordInfo?taskId=vendor-task', {
        taskId: 'vendor-task', model: 'seedance-20', status: 'success', output: [{ url: artifact }],
      }],
    ],
    media: 'video/mp4',
  },
};

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return 'http://127.0.0.1:' + server.address().port;
}

async function close(server) {
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}

for (const [name, fixture] of Object.entries(cases)) {
  test(name + ' releases unused upstream bodies and preserves outcomes', { timeout: 15_000 }, async (t) => {
    const state = await mkdtemp(join(tmpdir(), 'tokencraft-upstream-cleanup-'));
    const json = new Map([[fixture.submitPath, fixture.acknowledgement]]);
    let stalled;
    let plugin;
    const vendor = createServer((request, response) => {
      request.resume();
      if (stalled?.path === request.url) {
        const { status, closed } = stalled;
        stalled = undefined;
        response.once('close', () => closed(true));
        response.writeHead(status, { 'content-type': 'application/json' });
        response.write('fixture body remains open');
      } else if (request.url === '/artifact') {
        response.writeHead(200, { 'content-type': fixture.media }).end('fixture bytes');
      } else {
        response.writeHead(json.has(request.url) ? 200 : 404, { 'content-type': 'application/json' });
        response.end(JSON.stringify(json.get(request.url) ?? {}));
      }
    });
    t.after(async () => {
      if (plugin?.listening) await close(plugin);
      if (vendor.listening) await close(vendor);
      await rm(state, { recursive: true });
    });

    const origin = await listen(vendor);
    for (const [path, reply] of fixture.completed(origin + '/artifact')) json.set(path, reply);
    const provider = fixture.provider({ origin, key: 'fixture-vendor-key', artifactOrigins: [origin] });
    plugin = await fixture.queue({ state, provider, token: 'fixture-gateway-key' });
    const jobs = (await listen(plugin)) + '/v1/jobs';
    const headers = { authorization: 'Bearer fixture-gateway-key' };
    const submit = async (id) => {
      const response = await fetch(jobs, {
        method: 'POST', headers: { ...headers, 'idempotency-key': id },
        body: JSON.stringify({ version: 1, request_id: id, model: fixture.model, input: fixture.input }),
      });
      assert.equal(response.status, 200);
      return (await response.json()).outcome;
    };
    const read = (path) => fetch(jobs + '/accepted' + path, { headers });

    async function checkClose(path, status, verify) {
      const closed = new Promise((resolve) => { stalled = { path, status, closed: resolve }; });
      await verify();
      assert.equal(await Promise.race([closed, delay(2_500, false, { ref: false })]), true,
        'The unused upstream body should close before the four-second request timeout.');
    }

    await checkClose(fixture.submitPath, 429, async () => {
      assert.deepEqual(await submit('throttled'), { status: 'unavailable', retry_after_ms: 2_000 });
    });
    await checkClose(fixture.submitPath, 403, async () => {
      assert.deepEqual(await submit('rejected'), { status: 'rejected' });
    });
    await checkClose(fixture.submitPath, 503, async () => {
      assert.deepEqual(await submit('unconfirmed'), { status: 'uncertain' });
    });
    assert.deepEqual(await submit('accepted'), { status: 'accepted', handle: 'accepted' });
    await checkClose(fixture.pollPath, 503, async () => {
      const response = await read('');
      assert.equal(response.status, 502);
      assert.deepEqual(await response.json(), { error: 'plugin operation unavailable' });
    });
    await checkClose('/artifact', 200, async () => {
      const response = await read('/artifacts/0');
      assert.equal(response.status, 502);
      assert.deepEqual(await response.json(), { error: 'invalid upstream artifact' });
    });
    const artifact = await read('/artifacts/0');
    assert.equal(artifact.headers.get('content-type'), fixture.media);
    assert.equal(await artifact.text(), 'fixture bytes');
  });
}
