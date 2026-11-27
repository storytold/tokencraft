import assert from 'node:assert/strict';
import { once } from 'node:events';
import { readFile, rename, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';

import { fal } from '../plugins/providers/inference/fal/provider.mjs';
import { queueServer as falServer, readJson, send } from '../plugins/providers/inference/fal/queue.mjs';
import { kinovi } from '../plugins/providers/inference/kinovi/provider.mjs';
import { queueServer as kinoviServer } from '../plugins/providers/inference/kinovi/queue.mjs';
import { openRouterProxy } from '../plugins/providers/inference/openrouter/proxy.mjs';

const counts = { fal: 0, kinovi: 0, openrouter: 0, kinovi_throttles: 0 };
const media = await readFile('fixture.mp4');
let supplierOrigin;
const supplier = createServer(async (request, response) => {
  try {
    const path = new URL(request.url, supplierOrigin);
    if (path.pathname === '/counts') return send(response, 200, counts);
    for (const name of ['traceparent', 'tracestate', 'baggage', 'x-tokencraft-upstream-key']) {
      assert.equal(request.headers[name], undefined);
    }
    if (path.pathname === '/video') {
      assert.equal(request.headers.authorization, undefined);
      return response.writeHead(200, { 'content-type': 'video/mp4' }).end(media);
    }
    if (path.pathname === '/fal-ai/pixverse/v6/text-to-video' && request.method === 'POST') {
      assert.equal(request.headers.authorization, 'Key synthetic-fal');
      const input = await readJson(request);
      assert.equal(input.aspect_ratio, '16:9');
      assert.equal(input.resolution, '720p');
      assert.equal(input.duration, 5);
      return send(response, 200, { request_id: `fal-${++counts.fal}` });
    }
    if (/^\/fal-ai\/pixverse\/requests\/fal-\d+(\/status)?$/.test(path.pathname)) {
      assert.equal(request.headers.authorization, 'Key synthetic-fal');
      return send(response, 200, path.pathname.endsWith('/status')
        ? { status: 'COMPLETED' } : { video: { url: supplierOrigin + '/video' } });
    }
    if (path.pathname === '/api/v1/jobs/createTask' && request.method === 'POST') {
      assert.equal(request.headers.authorization, 'Bearer synthetic-kinovi');
      const job = await readJson(request);
      assert.equal(job.model, 'seedance-20');
      assert.equal(job.inputs.duration, '5');
      assert.equal(job.inputs.aspectRatio, '16:9');
      assert.equal(job.inputs.outputResolution, '720p');
      if (job.inputs.prompt === 'Throttle fixture') {
        counts.kinovi_throttles += 1;
        return send(response, 429, {});
      }
      return send(response, 200, { taskId: `kinovi-${++counts.kinovi}` });
    }
    if (path.pathname === '/api/v1/jobs/recordInfo') {
      assert.equal(request.headers.authorization, 'Bearer synthetic-kinovi');
      return send(response, 200, {
        taskId: path.searchParams.get('taskId'), model: 'seedance-20', status: 'success',
        output: [{ url: supplierOrigin + '/video' }], creditsUsed: 2.17,
      });
    }
    if (path.pathname === '/api/v1/chat/completions' && request.method === 'POST') {
      assert.equal(request.headers.authorization, 'Bearer synthetic-openrouter');
      const job = await readJson(request);
      assert.equal(job.model, 'openai/gpt-4.1-mini');
      counts.openrouter += 1;
      const identity = { id: 'fixture-chat', model: job.model };
      const usage = { prompt_tokens: 12, completion_tokens: 5, total_tokens: 17 };
      if (job.stream) {
        const frames = [
          { ...identity, provider: 'fixture upstream', choices: [{
            index: 0, delta: { content: 'Synthetic chat result' },
          }] },
          { ...identity, provider: { invalid: true }, choices: [{ index: 0, finish_reason: 'stop' }] },
          { ...identity, provider: 'sk-synthetic-metadata', choices: [], usage },
        ];
        const body = frames.map((frame) => `data: ${JSON.stringify(frame)}\n\n`).join('');
        return response.writeHead(200, { 'content-type': 'text/event-stream' }).end(body + 'data: [DONE]\n\n');
      }
      const provider = job.messages[0].content === 'Unstructured supplier metadata'
        ? { invalid: true } : 'fixture upstream';
      return send(response, 200, {
        ...identity, provider,
        object: 'chat.completion', created: 1, choices: [{
          index: 0, message: { role: 'assistant', content: 'Synthetic chat result' }, finish_reason: 'stop',
        }],
        usage,
      });
    }
    return send(response, 404, {});
  } catch (error) {
    console.error(error.message);
    return send(response, 400, { error: 'fixture contract mismatch' });
  }
});

async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return `http://127.0.0.1:${server.address().port}`;
}

supplierOrigin = await listen(supplier);
const supplierConfig = { origin: supplierOrigin, artifactOrigins: [supplierOrigin] };
const falOrigin = await listen(await falServer({
  state: 'fal-state', token: process.env.TOKENCRAFT_CONNECTOR_TOKEN_FAL,
  provider: fal({ ...supplierConfig, key: 'synthetic-fal' }),
}));
const kinoviOrigin = await listen(await kinoviServer({
  state: 'kinovi-state', token: process.env.TOKENCRAFT_CONNECTOR_TOKEN_KINOVI,
  provider: kinovi({ ...supplierConfig, key: 'synthetic-kinovi' }),
}));
const openrouterOrigin = await listen(openRouterProxy({
  endpoint: supplierOrigin + '/api/v1/chat/completions', upstreamKey: 'synthetic-openrouter',
  gatewayKey: process.env.TOKENCRAFT_LOCAL_CHAT_KEY_OPENROUTER, models: new Set(['openai/gpt-4.1-mini']),
}));
await writeFile('addresses.tmp', JSON.stringify({
  supplier: supplierOrigin, fal: falOrigin, kinovi: kinoviOrigin, openrouter: openrouterOrigin,
}));
await rename('addresses.tmp', 'addresses.json');
