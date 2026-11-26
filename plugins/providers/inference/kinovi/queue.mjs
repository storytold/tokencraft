import { serviceAuthorization } from './service-auth.mjs';
import { delegatedKey, keyTag, matchesKey } from './tenant-keys.mjs';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, open, readFile, rename, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { join } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { Readable } from 'node:stream';

const identifier = /^[A-Za-z0-9_-]{1,128}$/;
const baseHeaders = { 'cache-control': 'no-store' };

export function send(response, code, value) {
  response.writeHead(code, { ...baseHeaders, 'content-type': 'application/json' });
  response.end(JSON.stringify(value));
}

export async function readJson(stream, limit = 65_536, reviver) {
  const chunks = [];
  let size = 0;
  for await (const chunk of stream) {
    size += chunk.length;
    if (size > limit) throw new Error('body exceeds size limit');
    chunks.push(Buffer.from(chunk));
  }
  return JSON.parse(Buffer.concat(chunks).toString(), reviver);
}

export function upstream(url, options = {}) {
  return fetch(url, { ...options, redirect: 'error', signal: AbortSignal.timeout(4_000) });
}

export async function submitUpstream(url, authorization, body, handle) {
  const response = await upstream(url, {
    method: 'POST',
    headers: { authorization, 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (response.status === 429) return { status: 'unavailable', retry_after_ms: 2_000 };
  if ([400, 401, 402, 403, 413, 422].includes(response.status)) return { status: 'rejected' };
  if (!response.ok) return { status: 'uncertain' };
  const id = handle(await readJson(response.body));
  return typeof id === 'string' && identifier.test(id)
    ? { status: 'accepted', handle: id }
    : { status: 'uncertain' };
}

export function validatedOrigin(value) {
  const origin = new URL(value);
  if (origin.protocol !== 'https:' || origin.username || origin.password
      || origin.pathname !== '/' || origin.search || origin.hash) throw new Error('expected an HTTPS origin');
  return origin.origin;
}

export function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`set ${name}`);
  return value;
}

// Store the submit intent before the paid call. A crash at the acknowledgement
// boundary leaves an unresolved intent, which suppresses another vendor POST.
export async function queueServer({ state, token, provider, tenantKeys = false,
  previousToken, previousTokenExpiresAt }) {
  const authorize = serviceAuthorization(token, {
    previousToken, previousTokenExpiresAt, minimumPreviousLength: tenantKeys ? 32 : 24,
  });
  await mkdir(state, { mode: 0o700, recursive: true });
  const recordPath = (id) => join(state, `${id}.json`);
  const activeWrites = new Map();

  async function serialize(id, action) {
    const previous = activeWrites.get(id);
    const next = (previous ?? Promise.resolve()).catch(() => {}).then(action);
    activeWrites.set(id, next);
    try {
      return await next;
    } finally {
      if (activeWrites.get(id) === next) activeWrites.delete(id);
    }
  }

  async function load(id) {
    try {
      return JSON.parse(await readFile(recordPath(id), 'utf8'));
    } catch (error) {
      if (error.code === 'ENOENT') return undefined;
      throw error;
    }
  }

  async function syncDirectory() {
    const directory = await open(state, 'r');
    try {
      await directory.sync();
    } finally {
      await directory.close();
    }
  }

  async function save(id, record) {
    const pending = join(state, `.${randomUUID()}.json`);
    await writeFile(pending, JSON.stringify(record), { mode: 0o600, flush: true, flag: 'wx' });
    await rename(pending, recordPath(id));
    await syncDirectory();
  }

  async function submit(request, response) {
    const job = await readJson(request);
    const id = job?.request_id;
    if (job?.version !== 1 || typeof id !== 'string' || !identifier.test(id)
        || request.headers['idempotency-key'] !== id) {
      return send(response, 400, { error: 'invalid submission envelope' });
    }
    const delegated = delegatedKey(request, tenantKeys);
    if (delegated === null) return send(response, 403, { error: 'provider credential unavailable' });
    const credentialTag = keyTag(delegated, id);
    const reply = (outcome) => send(response, 200, { version: 1, request_id: id, outcome });
    const payload = provider.input(job);
    if (!payload) return reply({ status: 'rejected' });
    const fingerprint = createHash('sha256').update(JSON.stringify(payload)).digest('hex');
    const reserved = await serialize(id, async () => {
      const intent = { fingerprint, credentialTag };
      let file;
      try {
        file = await open(recordPath(id), 'wx', 0o600);
      } catch (error) {
        if (error.code !== 'EEXIST') throw error;
        const recorded = await load(id).catch(() => undefined);
        if (recorded?.fingerprint !== fingerprint || !matchesKey(recorded.credentialTag, credentialTag)) {
          return {};
        }
        const { outcome, retryAt } = recorded;
        if (outcome?.status !== 'unavailable') return { outcome };
        if (!Number.isFinite(retryAt)) return { outcome: { status: 'unavailable' } };
        if (Date.now() < retryAt) return { outcome };
        // Save the unresolved retry intent before the upstream call.
        const retry = { ...intent, retried: true };
        await save(id, retry);
        return { intent: retry };
      }
      try {
        await file.writeFile(JSON.stringify(intent));
        await file.sync();
      } finally {
        await file.close();
      }
      await syncDirectory();
      return { intent };
    });
    const publicOutcome = (outcome) => outcome?.status === 'accepted'
      ? { status: 'accepted', handle: id } : outcome ?? { status: 'uncertain' };
    if (!reserved.intent) return reply(publicOutcome(reserved.outcome));
    const outcome = await provider.submit(payload, delegated).catch(() => ({ status: 'uncertain' }));
    const canRetry = !reserved.intent.retried && outcome.status === 'unavailable' && outcome.retry_after_ms;
    const retryAt = canRetry ? Date.now() + outcome.retry_after_ms : undefined;
    const result = outcome.status === 'unavailable' && !canRetry ? { status: 'unavailable' } : outcome;
    await serialize(id, () => save(id, { ...reserved.intent, outcome: result, retryAt }));
    return reply(publicOutcome(result));
  }

  async function lookup(request, response, id, slot) {
    const job = await load(id);
    if (job?.outcome?.status !== 'accepted') return send(response, 404, { error: 'unknown job' });
    const delegated = delegatedKey(request, tenantKeys);
    if (delegated === null || !matchesKey(job.credentialTag, keyTag(delegated, id))) {
      return send(response, 403, { error: 'provider credential unavailable' });
    }
    const observed = await provider.poll(job.outcome.handle, delegated);
    if (slot === undefined) {
      const reply = { version: 1, handle: id, status: observed.status };
      if (request.headers['x-tokencraft-provider-charge'] === '1' && observed.providerCharge) {
        reply.provider_charge = observed.providerCharge;
      }
      return send(response, 200, reply);
    }
    const artifact = observed.files?.[slot];
    if (observed.status !== 'succeeded' || !artifact) return send(response, 404, { error: 'artifact unavailable' });
    const source = new URL(artifact);
    if (!provider.artifactOrigins.includes(source.origin) || source.username || source.password) {
      return send(response, 502, { error: 'unrecognized artifact origin' });
    }
    const upstreamResponse = await upstream(source);
    if (!upstreamResponse.ok || upstreamResponse.headers.get('content-type')?.split(';')[0] !== provider.mediaType
        || !upstreamResponse.body) return send(response, 502, { error: 'invalid upstream artifact' });
    let bytes = 0;
    response.writeHead(200, { ...baseHeaders, 'content-type': provider.mediaType });
    await pipeline(Readable.fromWeb(upstreamResponse.body), async function* (chunks) {
      for await (const chunk of chunks) {
        bytes += chunk.length;
        if (bytes > provider.maxBytes) throw new Error('artifact exceeds limit');
        yield chunk;
      }
    }, response);
  }

  return createServer(async (request, response) => {
    try {
      if (!authorize(request)) return send(response, 401, { error: 'unauthorized' });
      if (request.method === 'POST' && request.url === '/v1/jobs') return await submit(request, response);
      const match = /^\/v1\/jobs\/([A-Za-z0-9_-]{1,128})(?:\/artifacts\/([0-7]))?$/.exec(request.url ?? '');
      if (request.method === 'GET' && match) return await lookup(request, response, match[1], match[2]);
      return send(response, 404, { error: 'unknown endpoint' });
    } catch {
      if (response.headersSent) response.destroy();
      else send(response, 502, { error: 'plugin operation unavailable' });
    }
  });
}
