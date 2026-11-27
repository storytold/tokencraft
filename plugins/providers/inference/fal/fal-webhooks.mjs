import { createHash, createPublicKey, verify } from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { cancelUnusedBody, readBody, readJson, required, upstream, validatedOrigin } from './queue.mjs';

const keyEndpoint = 'https://rest.fal.ai/.well-known/jwks.json';
const callbackPath = /^\/v1\/fal\/webhooks\/([A-Za-z0-9_-]{1,128})\/([0-9a-f]{64})$/;
const callbackLimit = 128 * 1024;
const keyLifetimeMs = 15 * 60_000;
const keyRefreshDelayMs = 30_000;
const environment = ['FAL_WEBHOOK_ORIGIN', 'FAL_WEBHOOK_USER_ID', 'TOKENCRAFT_GATEWAY_ORIGIN'];

function header(request, name) {
  const values = request.headersDistinct[name];
  return values?.length === 1 ? values[0] : '';
}

export function configuredFalWebhooks({ tenantKeys = false } = {}) {
  if (!environment.some((name) => process.env[name] !== undefined)) return undefined;
  const [originName, userName, gatewayName] = environment;
  const publicOrigin = required(originName);
  const gateway = required(gatewayName);
  const user = tenantKeys ? process.env[userName] : required(userName);
  const callbackOrigin = validatedOrigin(publicOrigin);
  const gatewayUrl = new URL(gateway);
  // The local gateway can be addressed directly without going through public ingress.
  const gatewayOrigin = gatewayUrl.protocol === 'http:' && gatewayUrl.hostname === '127.0.0.1'
    && gatewayUrl.href === `${gatewayUrl.origin}/` ? gatewayUrl.origin : validatedOrigin(gateway);
  if (user !== undefined && !/^[!-~]{1,128}$/.test(user)) throw new Error('invalid FAL_WEBHOOK_USER_ID');
  return falWebhooks({ callbackOrigin, gatewayOrigin, user });
}

export function falWebhooks({ callbackOrigin, gatewayOrigin, user, jwksUrl = keyEndpoint, now = Date.now,
  elapsed = () => performance.now() }) {
  let snapshot;
  let nextRefresh = 0;
  let refreshing;

  async function fetchKeys() {
    const response = await upstream(jwksUrl);
    if (!response.ok) {
      cancelUnusedBody(response);
      throw new Error('fal signing keys unavailable');
    }
    const { keys } = await readJson(response.body, 32 * 1024);
    if (!Array.isArray(keys) || keys.length > 32) throw new Error('invalid fal signing keys');
    const publicKeys = keys.filter((key) => key?.kty === 'OKP' && key.crv === 'Ed25519'
      && (key.use === undefined || key.use === 'sig')
      && (key.alg === undefined || ['EdDSA', 'Ed25519'].includes(key.alg))
      && (key.key_ops === undefined || (Array.isArray(key.key_ops) && key.key_ops.includes('verify')))
      && typeof key.x === 'string' && /^[\w-]{43}=?$/.test(key.x))
      .map(({ x }) => createPublicKey({ format: 'jwk', key: { kty: 'OKP', crv: 'Ed25519', x } }));
    if (!publicKeys.length) throw new Error('no fal signing keys available');
    snapshot = { expires: elapsed() + keyLifetimeMs, publicKeys };
  }

  async function verifySignature(message, signature) {
    const current = () => snapshot?.expires > elapsed();
    const matches = () => current() && snapshot.publicKeys.some((key) => verify(null, message, key, signature));
    if (matches()) return true;
    if (!refreshing && elapsed() >= nextRefresh) {
      nextRefresh = elapsed() + keyRefreshDelayMs;
      refreshing = fetchKeys().finally(() => { refreshing = undefined; });
    }
    if (refreshing) await refreshing;
    if (!current()) throw new Error('fal signing keys unavailable');
    return matches();
  }

  return {
    route(request) {
      const match = request.method === 'POST' && callbackPath.exec(request.url ?? '');
      return match ? match.slice(1) : undefined;
    },
    callbackUrl(id, proof) {
      return new URL(`/v1/fal/webhooks/${id}/${proof}`, callbackOrigin);
    },
    async verify(request) {
      const id = header(request, 'x-fal-webhook-request-id');
      const account = header(request, 'x-fal-webhook-user-id');
      const stamp = header(request, 'x-fal-webhook-timestamp');
      const signature = header(request, 'x-fal-webhook-signature');
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(id) || !/^[!-~]{1,128}$/.test(account)
          || (user !== undefined && account !== user)
          || !/^\d{1,13}$/.test(stamp) || Math.abs(Math.floor(now() / 1000) - Number(stamp)) > 300
          || !/^[\da-fA-F]{128}$/.test(signature)
          || Number(request.headers['content-length']) > callbackLimit) return undefined;
      const body = await readBody(request, callbackLimit);
      const digest = createHash('sha256').update(body).digest('hex');
      const message = Buffer.from([id, account, stamp, digest].join('\n'));
      if (!await verifySignature(message, Buffer.from(signature, 'hex'))) return undefined;
      const callback = JSON.parse(body.toString());
      if (callback?.request_id !== id || !['OK', 'ERROR'].includes(callback.status)) return undefined;
      return { handle: id, status: callback.status === 'OK' ? 'succeeded' : 'failed' };
    },
    async notify(id, token) {
      const endpoint = new URL(`/v1/connector-jobs/${id}/completion`, gatewayOrigin);
      const reply = await upstream(endpoint, {
        method: 'POST',
        headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
        body: JSON.stringify({ version: 1 }),
      });
      cancelUnusedBody(reply);
      if (reply.status !== 204) throw new Error('gateway completion unavailable');
    },
  };
}
