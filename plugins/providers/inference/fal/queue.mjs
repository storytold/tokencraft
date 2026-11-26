import { serviceAuthorization } from './service-auth.mjs';
import { delegatedKey, keyTag, matchesKey } from './tenant-keys.mjs';
import { legacyModel } from './models.mjs';
import { createHash, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { link, lstat, mkdir, open, readFile, rename, unlink, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { join } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { Readable } from 'node:stream';

const identifier = /^[A-Za-z0-9_-]{1,128}$/;
const hexToken = /^[a-f0-9]{64}$/;
const baseHeaders = { 'cache-control': 'no-store' };

export function send(response, code, value) {
  response.writeHead(code, { ...baseHeaders, 'content-type': 'application/json' });
  response.end(JSON.stringify(value));
}

export async function readBody(stream, limit) {
  const chunks = [];
  let size = 0;
  for await (const chunk of stream) {
    size += chunk.length;
    if (size > limit) throw new Error('body exceeds size limit');
    chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks);
}

export async function readJson(stream, limit = 65_536) {
  return JSON.parse((await readBody(stream, limit)).toString());
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
export async function queueServer({ state, token, provider, webhooks, tenantKeys = false,
  previousToken, previousTokenExpiresAt }) {
  const authorize = serviceAuthorization(token, {
    previousToken, previousTokenExpiresAt, minimumPreviousLength: tenantKeys ? 32 : 24,
  });
  await mkdir(state, { mode: 0o700, recursive: true });
  const directory = await lstat(state);
  if (!directory.isDirectory() || (directory.mode & 0o077) !== 0) {
    throw new Error('plugin state must be a private directory');
  }
  const recordPath = (id) => join(state, `${id}.json`);
  const activeWrites = new Map();
  const notifications = new Map();

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

  async function syncDirectory() {
    const handle = await open(state, 'r');
    try {
      await handle.sync();
    } finally {
      await handle.close();
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

  async function save(id, record, { create = false } = {}) {
    const pending = join(state, `.${randomUUID()}.json`);
    try {
      await writeFile(pending, JSON.stringify(record), { mode: 0o600, flush: true, flag: 'wx' });
      if (create) await link(pending, recordPath(id));
      else await rename(pending, recordPath(id));
      await syncDirectory();
    } finally {
      await unlink(pending).catch((error) => {
        if (error.code !== 'ENOENT') throw error;
      });
    }
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
    const completion = job.completion;
    if (completion !== undefined && (!webhooks || !completion
        || completion.path !== `/v1/connector-jobs/${id}/completion`
        || typeof completion.token !== 'string' || !hexToken.test(completion.token))) {
      return send(response, 400, { error: 'invalid or unavailable job completion configuration' });
    }
    const fingerprint = createHash('sha256').update(JSON.stringify(payload)).digest('hex');
    const proof = completion && randomBytes(32).toString('hex');
    const intent = { model: job.model, fingerprint, credentialTag,
      ...(proof && { proof, completionToken: completion.token }) };
    const publicOutcome = (outcome) => outcome?.status === 'accepted'
      ? { status: 'accepted', handle: id } : outcome ?? { status: 'uncertain' };

    const reserved = await serialize(id, async () => {
      let recorded = await load(id);
      if (!recorded) {
        try {
          await save(id, intent, { create: true });
          return { intent };
        } catch (error) {
          if (error.code !== 'EEXIST') throw error;
          recorded = await load(id);
        }
      }
      const same = recorded && (recorded.model ?? legacyModel) === job.model
        && recorded.fingerprint === fingerprint && recorded.completionToken === completion?.token
        && matchesKey(recorded.credentialTag, credentialTag);
      if (!same) return {};
      const { outcome, retryAt } = recorded;
      if (outcome?.status !== 'unavailable') return { outcome };
      if (!Number.isFinite(retryAt)) return { outcome: { status: 'unavailable' } };
      if (Date.now() < retryAt) return { outcome };
      // Persist an unresolved intent before retrying. A restart during this
      // attempt must reconcile the original job instead of submitting again.
      const retry = { ...recorded, outcome: undefined, retryAt: undefined, retried: true };
      await save(id, retry);
      return { intent: retry };
    });
    if (!reserved.intent) return reply(publicOutcome(reserved.outcome));
    const callbackProof = reserved.intent.proof;
    const url = callbackProof && webhooks.callbackUrl(id, callbackProof);
    const outcome = await provider.submit(job.model, payload, url, delegated).catch(() => ({ status: 'uncertain' }));
    const saved = await serialize(id, async () => {
      const record = await load(id);
      if (record.terminal) {
        // A verified callback can beat the submit response. Preserve its mapping.
        return outcome.status === 'accepted' && outcome.handle !== record.outcome.handle
          ? { status: 'uncertain' } : record.outcome;
      }
      const canRetry = !record.retried && outcome.status === 'unavailable' && outcome.retry_after_ms;
      const retryAt = canRetry ? Date.now() + outcome.retry_after_ms : undefined;
      const result = outcome.status === 'unavailable' && !canRetry ? { status: 'unavailable' } : outcome;
      await save(id, { ...record, outcome: result, retryAt });
      return result;
    });
    return reply(publicOutcome(saved));
  }

  async function completionCallback(request, response, [id, proof]) {
    const matches = (job) => job?.proof && timingSafeEqual(Buffer.from(job.proof), Buffer.from(proof));
    if (!matches(await load(id))) return send(response, 404, { error: 'unknown callback' });
    const terminal = await webhooks.verify(request);
    if (!terminal) return send(response, 401, { error: 'invalid fal callback' });

    const receipt = await serialize(id, async () => {
      const job = await load(id);
      if (!matches(job) || (job.outcome?.handle && job.outcome.handle !== terminal.handle)
          || (job.terminal && job.terminal !== terminal.status)) return undefined;
      // A tenant callback can confirm completion once the upstream acknowledged this exact job.
      if (tenantKeys && !job.outcome?.handle) return { waitingForAcknowledgement: true };
      if (job.gatewayNotified) return {};
      if (!job.terminal) {
        const outcome = { status: 'accepted', handle: terminal.handle };
        await save(id, { ...job, outcome, terminal: terminal.status });
      }

      let delivery = notifications.get(id);
      if (!delivery) {
        delivery = webhooks.notify(id, job.completionToken).then(() => serialize(id, async () => {
          const latest = await load(id);
          await save(id, { ...latest, gatewayNotified: true });
        })).finally(() => notifications.delete(id));
        notifications.set(id, delivery);
      }
      return { delivery };
    });
    if (!receipt) return send(response, 409, { error: 'conflicting fal callback' });
    if (receipt.waitingForAcknowledgement) return send(response, 503, { error: 'waiting for fal acknowledgement' });
    // A failed forward leaves the receipt on disk and asks fal to retry delivery.
    await receipt.delivery;
    return response.writeHead(204, baseHeaders).end();
  }

  async function lookup(request, response, id, slot) {
    const job = await load(id);
    if (job?.outcome?.status !== 'accepted') return send(response, 404, { error: 'unknown job' });
    const delegated = delegatedKey(request, tenantKeys);
    if (delegated === null || !matchesKey(job.credentialTag, keyTag(delegated, id))) {
      return send(response, 403, { error: 'provider credential unavailable' });
    }
    const model = provider.model(job.model ?? legacyModel);
    if (!model) throw new Error('unknown persisted fal model');
    const observed = job.terminal === 'failed'
      ? { status: 'failed' } : await provider.poll(model, job.outcome.handle, delegated);
    if (slot === undefined) {
      return send(response, 200, { version: 1, handle: id, status: observed.status });
    }
    const artifact = observed.files?.[slot];
    if (observed.status !== 'succeeded' || !artifact) return send(response, 404, { error: 'artifact unavailable' });
    const source = new URL(artifact);
    if (!['http:', 'https:'].includes(source.protocol) || !provider.artifactOrigins.includes(source.origin)
        || source.username || source.password) {
      return send(response, 502, { error: 'unrecognized artifact origin' });
    }
    const upstreamResponse = await upstream(source);
    const mediaType = upstreamResponse.headers.get('content-type')?.split(';')[0].trim().toLowerCase();
    const length = upstreamResponse.headers.get('content-length');
    if (!upstreamResponse.ok || !upstreamResponse.body || Number(length) > model.maxBytes
        || (mediaType !== model.mediaType && !model.mediaAliases?.includes(mediaType))) {
      await upstreamResponse.body?.cancel();
      return send(response, 502, { error: 'invalid upstream artifact' });
    }
    let bytes = 0;
    response.writeHead(200, { ...baseHeaders, 'content-type': model.mediaType });
    await pipeline(Readable.fromWeb(upstreamResponse.body), async function* (chunks) {
      for await (const chunk of chunks) {
        bytes += chunk.length;
        if (bytes > model.maxBytes) throw new Error('artifact exceeds limit');
        yield chunk;
      }
    }, response);
  }

  const limits = { requestTimeout: 12_000, headersTimeout: 8_000, connectionsCheckingInterval: 1_000 };
  return createServer(limits, async (request, response) => {
    try {
      const callback = webhooks?.route(request);
      if (callback) return await completionCallback(request, response, callback);
      if (!authorize(request)) return send(response, 401, { error: 'unauthorized' });
      if (request.method === 'POST' && request.url === '/v1/jobs') return await submit(request, response);
      const byRequest = /^\/v1\/jobs\/by-request\/([A-Za-z0-9_-]{1,128})$/.exec(request.url ?? '');
      if (request.method === 'GET' && byRequest) {
        const id = byRequest[1];
        const job = await load(id);
        if (job?.outcome?.status !== 'accepted') return send(response, 404, { error: 'unknown job' });
        return send(response, 200, { version: 1, request_id: id, handle: id });
      }
      const match = /^\/v1\/jobs\/([A-Za-z0-9_-]{1,128})(?:\/artifacts\/([0-7]))?$/.exec(request.url ?? '');
      if (request.method === 'GET' && match) return await lookup(request, response, match[1], match[2]);
      return send(response, 404, { error: 'unknown endpoint' });
    } catch {
      if (response.headersSent) response.destroy();
      else send(response, 502, { error: 'plugin operation unavailable' });
    }
  });
}
