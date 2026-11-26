import { serviceAuthorization } from './service-auth.mjs';
import { createServer } from 'node:http';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';

const fields = ['model', 'messages', 'max_tokens', 'temperature', 'top_p', 'stop', 'stream', 'stream_options'];
const tenantKeyHeader = 'x-tokencraft-upstream-key';
const requestOptions = (body) => Object.fromEntries(fields.filter((key) => Object.hasOwn(body, key))
  .map((key) => [key, body[key]]));

function error(response, status, message) {
  response.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
  response.end(JSON.stringify({ error: { message } }));
}

async function readRequest(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > 65_536) throw new Error('request too large');
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString());
}

export function openRouterProxy({ endpoint, upstreamKey, tenantKeys = false, gatewayKey, models,
  previousToken, previousTokenExpiresAt }) {
  const authorize = serviceAuthorization(gatewayKey, {
    previousToken, previousTokenExpiresAt, minimumPreviousLength: tenantKeys ? 32 : 1,
  });
  return createServer(async (request, response) => {
    if (!authorize(request)) {
      return error(response, 401, 'unauthorized');
    }
    if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
      return error(response, 404, 'unknown endpoint');
    }
    const delegated = request.headersDistinct[tenantKeyHeader];
    const tenantKey = delegated?.length === 1 ? delegated[0] : '';
    if (tenantKeys ? !/^sk-or-v1-[a-f0-9]{64}$/.test(tenantKey) : delegated !== undefined) {
      return error(response, 403, 'provider credential unavailable');
    }
    const providerKey = tenantKeys ? tenantKey : upstreamKey;
    let body;
    try {
      body = await readRequest(request);
      if (!models.has(body?.model) || !Array.isArray(body.messages) || body.messages.length === 0
          || !Number.isInteger(body.max_tokens) || body.max_tokens < 1 || body.max_tokens > 32_768
          || !body.messages.every((message) => typeof message?.content === 'string'
            && ['system', 'user', 'assistant'].includes(message.role))) {
        return error(response, 400, 'unsupported chat request');
      }
    } catch {
      return error(response, 400, 'invalid chat request');
    }
    const abort = new AbortController();
    response.once('close', () => abort.abort());
    try {
      const upstream = await fetch(endpoint, {
        method: 'POST',
        redirect: 'error',
        signal: AbortSignal.any([abort.signal, AbortSignal.timeout(90_000)]),
        headers: { authorization: `Bearer ${providerKey}`, 'content-type': 'application/json' },
        body: JSON.stringify(requestOptions(body)),
      });
      if (!upstream.ok) {
        const known = [400, 401, 402, 403, 413, 422, 429];
        await upstream.body?.cancel();
        return error(response, known.includes(upstream.status) ? upstream.status : 502, 'upstream request unavailable');
      }
      const type = upstream.headers.get('content-type')?.split(';')[0].trim().toLowerCase();
      if (!upstream.body || type !== (body.stream ? 'text/event-stream' : 'application/json')) {
        await upstream.body?.cancel();
        return error(response, 502, 'unexpected upstream response');
      }
      response.writeHead(200, { 'content-type': type, 'cache-control': 'no-store' });
      let bytes = 0;
      await pipeline(Readable.fromWeb(upstream.body), async function* (chunks) {
        for await (const chunk of chunks) {
          bytes += chunk.length;
          if (bytes > 4 * 1024 * 1024) throw new Error('response too large');
          yield chunk;
        }
      }, response);
    } catch {
      if (response.headersSent) response.destroy();
      else error(response, 502, 'upstream request unavailable');
    }
  });
}
