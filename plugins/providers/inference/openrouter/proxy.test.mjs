import assert from 'node:assert/strict';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { openRouterProxy } from './proxy.mjs';

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return `http://127.0.0.1:${server.address().port}`;
}

test('forwards JSON and SSE with the vendor key and strips provider overrides', async (t) => {
  const calls = [];
  const vendor = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    calls.push({ path: request.url, auth: request.headers.authorization, body });
    if (body.stream) {
      response.writeHead(200, { 'content-type': 'Text/Event-Stream; charset=utf-8' });
      return response.end('data: {"choices":[]}\n\ndata: [DONE]\n\n');
    }
    response.writeHead(200, { 'content-type': 'application/json' });
    return response.end('{"choices":[]}');
  });
  const base = await listen(vendor);
  const proxy = openRouterProxy({
    endpoint: `${base}/api/v1/chat/completions`,
    upstreamKey: 'test-vendor-key',
    gatewayKey: 'test-gateway-key',
    models: new Set(['vendor/model']),
  });
  const url = `${await listen(proxy)}/v1/chat/completions`;
  t.after(async () => {
    for (const server of [proxy, vendor]) {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    }
  });
  const body = { model: 'vendor/model', messages: [{ role: 'user', content: 'hello' }], max_tokens: 16 };
  const submit = (value, key = 'test-gateway-key') => fetch(url, {
    method: 'POST', headers: { authorization: `Bearer ${key}` }, body: JSON.stringify(value),
  });
  assert.equal((await submit(body, 'wrong')).status, 401);
  assert.equal((await fetch(url, { method: 'POST', headers: { authorization: 'Bearer test-gateway-key',
    'x-tokencraft-upstream-key': `sk-or-v1-${'2'.repeat(64)}` }, body: JSON.stringify(body) })).status, 403);
  assert.equal((await submit({ ...body, model: 'different/model' })).status, 400);
  assert.deepEqual(await (await submit({ ...body, provider: { order: ['unexpected'] } })).json(), { choices: [] });
  const streamBody = { ...body, stream: true };
  const stream = await submit(streamBody);
  assert.equal(stream.headers.get('content-type'), 'text/event-stream');
  assert.match(await stream.text(), /data: \[DONE\]/);
  assert.deepEqual(calls, [body, streamBody].map((body) => ({
    path: '/api/v1/chat/completions', auth: 'Bearer test-vendor-key', body,
  })));
});

test('uses only a delegated key on a tenant binding after authenticating the gateway', async (t) => {
  const key = `sk-or-v1-${'1'.repeat(64)}`;
  const calls = [];
  const vendor = createServer((request, response) => {
    calls.push({ authorization: request.headers.authorization, delegated: request.headers['x-tokencraft-upstream-key'] });
    request.resume();
    response.writeHead(200, { 'content-type': 'application/json' }).end('{"choices":[]}');
  });
  const endpoint = await listen(vendor);
  const proxy = openRouterProxy({ endpoint, tenantKeys: true, gatewayKey: 'fixture-gateway-token',
    models: new Set(['vendor/model']) });
  const url = `${await listen(proxy)}/v1/chat/completions`;
  t.after(async () => {
    for (const server of [proxy, vendor]) {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    }
  });
  const submit = (headers) => fetch(url, { method: 'POST', headers, body: JSON.stringify({
    model: 'vendor/model', messages: [{ role: 'user', content: 'hello' }], max_tokens: 16,
  }) });
  const delegated = { 'x-tokencraft-upstream-key': key };
  const authorization = { authorization: 'Bearer fixture-gateway-token' };
  assert.equal((await submit(delegated)).status, 401);
  assert.equal((await submit(authorization)).status, 403);
  assert.equal((await submit({ ...authorization, 'x-tokencraft-upstream-key': 'invalid' })).status, 403);
  assert.equal((await submit({ ...authorization, ...delegated })).status, 200);
  assert.deepEqual(calls, [{ authorization: `Bearer ${key}`, delegated: undefined }]);
});
