import assert from "node:assert/strict";
import test from "node:test";
import { scheduleDeadline } from "../frontend/src/lib/deadline.ts";

test("a 30-day retention deadline stays inside the browser timer range", (t) => {
  const browserLimit = 2_147_483_647;
  const month = 30 * 24 * 60 * 60 * 1_000;
  let now = 0;
  let pending;
  let expired = 0;
  t.mock.method(Date, "now", () => now);
  globalThis.window = {
    setTimeout(callback, delay) {
      pending = { callback, delay };
      return 1;
    },
    clearTimeout() { pending = undefined; },
  };
  t.after(() => { delete globalThis.window; });

  scheduleDeadline(month, () => { expired++; });
  assert.equal(expired, 0);
  assert.equal(pending.delay, browserLimit);
  now += pending.delay;
  pending.callback();
  assert.equal(expired, 0);
  assert.equal(pending.delay, month - browserLimit);
  now += pending.delay;
  pending.callback();
  assert.equal(expired, 1);

  const cancel = scheduleDeadline(now + month, () => assert.fail("canceled deadline fired"));
  cancel();
  assert.equal(pending, undefined);
});
