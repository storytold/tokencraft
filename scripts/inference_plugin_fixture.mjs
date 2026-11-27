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
const artifacts = new Map([
  ['/video', { file: 'fixture.mp4', contentType: 'video/mp4' }],
  ['/image', { file: 'fixture.png', contentType: 'image/png' }],
  ['/audio', { file: 'fixture.wav', contentType: 'audio/x-wav' }],
  ['/mesh', { file: 'fixture.glb', contentType: 'model/gltf-binary' }],
  ['/broken-mesh', { file: 'broken.glb', contentType: 'model/gltf-binary' }],
]);
const falModels = new Map([
  ['/fal-ai/flux/dev', {
    statusPath: '/fal-ai/flux', artifact: '/image',
    input: { image_size: { width: 1024, height: 1024 }, num_images: 1, output_format: 'png' },
    result: (url) => ({ images: [{ url }] }),
  }],
  ['/fal-ai/pixverse/v6/text-to-video', {
    statusPath: '/fal-ai/pixverse', artifact: '/video',
    input: { aspect_ratio: '16:9', resolution: '720p', duration: 5 },
    result: (url) => ({ video: { url } }),
  }],
  ['/fal-ai/stable-audio-25/text-to-audio', {
    statusPath: '/fal-ai/stable-audio-25', artifact: '/audio', input: { seconds_total: 5 },
    result: (url) => ({ audio: { url } }),
  }],
  ['/fal-ai/meshy/v6/text-to-3d', {
    statusPath: '/fal-ai/meshy', artifact: '/mesh',
    input: { mode: 'preview', should_remesh: true, target_polycount: 10_000 },
    result: (url) => ({ model_glb: { url } }),
  }],
]);
const falJobs = new Map();
let supplierOrigin;
const supplier = createServer(async (request, response) => {
  try {
    const path = new URL(request.url, supplierOrigin);
    if (path.pathname === '/counts') return send(response, 200, counts);
    for (const name of ['traceparent', 'tracestate', 'baggage', 'x-tokencraft-upstream-key']) {
      assert.equal(request.headers[name], undefined);
    }
    const artifact = artifacts.get(path.pathname);
    if (artifact && request.method === 'GET') {
      assert.equal(request.headers.authorization, undefined);
      const body = await readFile(artifact.file);
      return response.writeHead(200, { 'content-type': artifact.contentType }).end(body);
    }
    const submittedModel = falModels.get(path.pathname);
    if (submittedModel && request.method === 'POST') {
      assert.equal(request.headers.authorization, 'Key synthetic-fal');
      const { prompt, ...input } = await readJson(request);
      assert.equal(typeof prompt, 'string');
      assert.deepEqual(input, { ...submittedModel.input, enable_safety_checker: true });
      const handle = `fal-${++counts.fal}`;
      const file = submittedModel.artifact === '/mesh' && prompt === 'Corrupt mesh fixture'
        ? '/broken-mesh' : submittedModel.artifact;
      falJobs.set(handle, { model: submittedModel, file });
      return send(response, 200, { request_id: handle });
    }
    const falJobPath = /^(\/fal-ai\/[^/]+)\/requests\/(fal-\d+)(\/status)?$/.exec(path.pathname);
    if (falJobPath && request.method === 'GET') {
      assert.equal(request.headers.authorization, 'Key synthetic-fal');
      const [, statusPath, handle, status] = falJobPath;
      const job = falJobs.get(handle);
      assert.equal(job?.model.statusPath, statusPath);
      return send(response, 200, status ? { status: 'COMPLETED' } : job.model.result(supplierOrigin + job.file));
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
