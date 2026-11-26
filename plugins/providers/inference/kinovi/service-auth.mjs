import { createHash, randomUUID, timingSafeEqual } from 'node:crypto';

const digest = (value) => createHash('sha256').update(value).digest();
const bearer = (token) => digest(`Bearer ${token}`);
const tokenFormat = /^[A-Za-z0-9._~+/=-]{1,256}$/;

export function serviceAuthorization(token, { previousToken, previousTokenExpiresAt, minimumPreviousLength = 1 } = {}) {
  if (typeof token !== 'string' || !token) throw new Error('set the plugin service token');
  const current = bearer(token);
  let previous;
  let deadline;
  let elapsedDeadline;
  if (previousToken !== undefined || previousTokenExpiresAt !== undefined) {
    if (typeof previousToken !== 'string' || previousToken === token || !tokenFormat.test(previousToken)
        || previousToken.length < minimumPreviousLength) {
      throw new Error('configure a distinct previous service token');
    }
    deadline = Date.parse(previousTokenExpiresAt);
    const canonical = Number.isFinite(deadline) && new Date(deadline).toISOString();
    if (!canonical || (previousTokenExpiresAt !== canonical && previousTokenExpiresAt !== canonical.replace('.000Z', 'Z'))) {
      throw new Error('set the previous service token expiry to a UTC timestamp');
    }
    const remaining = deadline - Date.now();
    if (remaining > 0) {
      previous = bearer(previousToken);
      elapsedDeadline = performance.now() + remaining;
    }
  }

  return (request) => {
    const values = request.headersDistinct.authorization;
    if (values?.length !== 1) return false;
    // A correction to the system clock must never extend the overlap.
    if (previous && (Date.now() >= deadline || performance.now() >= elapsedDeadline)) previous = undefined;
    const supplied = digest(values[0]);
    const matchesCurrent = timingSafeEqual(current, supplied);
    const matchesPrevious = previous !== undefined && timingSafeEqual(previous, supplied);
    return matchesCurrent || matchesPrevious;
  };
}

// Keep this contract identical in the independent plugin packages.
const latencyBounds = [25, 50, 100, 250, 500, 1000, 2500, 5000, 10_000, 30_000, 60_000, 120_000];

export function monitoringProbe(token, serviceToken, previousToken, operations = []) {
  if (token === undefined) return undefined;
  if (!/^[A-Za-z0-9_-]{32,128}$/.test(token) || token === serviceToken || token === previousToken) {
    throw new Error('configure a separate monitoring token');
  }
  const authorized = serviceAuthorization(token);
  const bootId = randomUUID();
  const traffic = Object.fromEntries(operations.map((operation) => [operation, {
    in_flight: 0,
    responses: { ok: 0, throttled: 0, client_error: 0, server_error: 0, interrupted: 0 },
    latency_ms: { buckets: Array(latencyBounds.length + 1).fill(0), sum: 0 },
  }]));
  return {
    handle(request, response) {
      if (request.method !== 'GET' || request.url !== '/internal/monitoring') return false;
      if (!authorized(request)) {
        response.writeHead(401, { 'cache-control': 'no-store' });
        response.end();
      } else if (request.headers?.accept === 'application/json') {
        response.writeHead(200, { 'cache-control': 'no-store', 'content-type': 'application/json' });
        response.end(JSON.stringify({ version: 1, boot_id: bootId, operations: traffic }));
      } else {
        response.writeHead(204, { 'cache-control': 'no-store' });
        response.end();
      }
      return true;
    },
    record(operation, response) {
      const stats = traffic[operation];
      const start = performance.now();
      stats.in_flight++;
      const close = () => finish(false);
      const complete = () => finish(true);
      function finish(completed) {
        response.off('close', close);
        response.off('finish', complete);
        stats.in_flight--;
        const status = response.statusCode;
        const outcome = !completed ? 'interrupted'
          : status >= 200 && status < 300 ? 'ok'
            : status === 429 ? 'throttled'
              : status >= 500 ? 'server_error' : 'client_error';
        stats.responses[outcome]++;
        if (outcome === 'ok') {
          const elapsed = Math.max(0, performance.now() - start);
          const bucket = latencyBounds.findIndex((limit) => elapsed <= limit);
          stats.latency_ms.buckets[bucket === -1 ? latencyBounds.length : bucket]++;
          stats.latency_ms.sum += elapsed;
        }
      }
      response.once('close', close);
      response.once('finish', complete);
    },
  };
}

export function logRequestTrace(request, response, service, operation, write = (entry) => console.info(JSON.stringify(entry))) {
  const values = request.headersDistinct.traceparent;
  if (values?.length !== 1 || values[0].length !== 55) return;
  const match = /^00-([0-9a-f]{32})-([0-9a-f]{16})-[0-9a-f]{2}$/.exec(values[0]);
  if (!match || /^0+$/.test(match[1]) || /^0+$/.test(match[2])) return;

  const [, traceId, parentSpanId] = match;
  const start = performance.now();
  response.once('close', () => write({
    event: 'connector_request',
    timestamp: new Date(Date.now()).toISOString(),
    service,
    operation,
    trace_id: traceId,
    parent_span_id: parentSpanId,
    http_status: response.headersSent ? response.statusCode : null,
    elapsed_ms: Math.max(0, Math.round(performance.now() - start)),
    interrupted: !response.writableFinished,
  }));
}
