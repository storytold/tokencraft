import assert from 'node:assert/strict';
import test from 'node:test';
import { EventEmitter } from 'node:events';
import { monitoringProbe, serviceAuthorization } from './service-auth.mjs';

const current = 'new-fixture-service-token-0123456789';
const previous = 'prior-fixture-service-token-0123456789';
const expires = '2030-01-02T03:04:05Z';
const header = (key) => `Bearer ${key}`;
const request = (...values) => ({ headersDistinct: { authorization: values } });

test('accepts both service tokens until the deadline and then retires the previous token', (t) => {
  const deadline = Date.parse(expires);
  let now = deadline - 1;
  let elapsed = 0;
  t.mock.method(Date, 'now', () => now);
  t.mock.method(performance, 'now', () => elapsed);
  const authorize = serviceAuthorization(current, { previousToken: previous, previousTokenExpiresAt: expires });
  assert.equal(authorize(request(header(current))), true);
  assert.equal(authorize(request(header(previous))), true);
  assert.equal(authorize(request(header('unrelated'))), false);
  assert.equal(authorize(request(header(current), header(previous))), false);
  assert.equal(authorize(request()), false);
  now = deadline;
  assert.equal(authorize(request(header(previous))), false);
  assert.equal(authorize(request(header(current))), true);
  now -= 100;
  assert.equal(authorize(request(header(previous))), false);
  const restarted = serviceAuthorization(current, { previousToken: previous, previousTokenExpiresAt: expires });
  now -= 100;
  elapsed += 100;
  assert.equal(restarted(request(header(previous))), false);
});

test('requires a valid, distinct previous token paired with a UTC expiry', () => {
  assert.throws(() => serviceAuthorization(current, { previousToken: previous }), /UTC timestamp/);
  assert.throws(() => serviceAuthorization(current, { previousTokenExpiresAt: expires }), /distinct previous service token/);
  assert.throws(() => serviceAuthorization(current, { previousToken: current, previousTokenExpiresAt: expires }), /distinct/);
  assert.throws(() => serviceAuthorization(current, { previousToken: 'short', previousTokenExpiresAt: expires,
    minimumPreviousLength: 24 }), /distinct/);
  assert.throws(() => serviceAuthorization(current, { previousToken: previous,
    previousTokenExpiresAt: '2030-02-30T03:04:05Z' }), /UTC timestamp/);
  const expired = serviceAuthorization(current, { previousToken: previous, previousTokenExpiresAt: '2000-01-01T00:00:00Z' });
  assert.equal(expired(request(header(previous))), false);
  assert.equal(expired(request(header(current))), true);
});

test('monitoring is opt-in and its credential authorizes only the process probe', () => {
  const monitor = 'monitoring-fixture-token-012345678901';
  const respond = (key, method = 'GET', url = '/internal/monitoring') => {
    const request = { method, url, headersDistinct: { authorization: [header(key)] } };
    let status = null;
    const response = { writeHead(code) { status = code; }, end() {} };
    const handled = monitoringProbe(monitor, current, previous).handle(request, response);
    return { status, handled };
  };
  assert.equal(monitoringProbe(undefined, current, previous), undefined);
  assert.throws(() => monitoringProbe(current, current, previous), /separate/);
  assert.throws(() => monitoringProbe(previous, current, previous), /separate/);
  assert.deepEqual(respond(monitor), { status: 204, handled: true });
  assert.deepEqual(respond(current), { status: 401, handled: true });
  assert.deepEqual(respond(monitor, 'POST'), { status: null, handled: false });
  assert.deepEqual(respond(monitor, 'GET', '/v1/jobs/example'), { status: null, handled: false });
  assert.equal(serviceAuthorization(current)(request(header(monitor))), false);
});

test('private JSON probe reports finished, throttled, and interrupted traffic once', (t) => {
  let now = 0;
  t.mock.method(performance, 'now', () => now);
  const secret = 'monitoring-fixture-token-012345678901';
  const monitor = monitoringProbe(secret, current, previous, ['chat']);
  function poll(key) {
    const request = {
      method: 'GET', url: '/internal/monitoring', headers: { accept: 'application/json' },
      headersDistinct: { authorization: [header(key)] },
    };
    let state;
    let body;
    monitor.handle(request, { writeHead(code) { state = code; }, end(value) { body = value; } });
    return [state, body === undefined ? undefined : JSON.parse(body)];
  }

  const streaming = new EventEmitter();
  streaming.statusCode = 200;
  monitor.record('chat', streaming);
  const [status, start] = poll(secret);
  assert.equal(status, 200);
  assert.equal(start.operations.chat.in_flight, 1);
  assert.deepEqual(poll(current), [401, undefined]);

  now = 73;
  streaming.emit('finish');
  streaming.emit('close');
  for (const [code, event] of [[429, 'finish'], [502, 'finish'], [409, 'finish'], [200, 'close']]) {
    const response = new EventEmitter();
    response.statusCode = code;
    monitor.record('chat', response);
    response.emit(event);
  }
  const [, result] = poll(secret);
  assert.equal(result.version, 1);
  assert.equal(result.boot_id, start.boot_id);
  assert.equal(result.operations.chat.in_flight, 0);
  assert.deepEqual(result.operations.chat.responses, {
    ok: 1, throttled: 1, client_error: 1, server_error: 1, interrupted: 1,
  });
  assert.deepEqual(result.operations.chat.latency_ms.buckets, [0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
  assert.equal(result.operations.chat.latency_ms.sum, 73);
});
