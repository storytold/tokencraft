import assert from "node:assert/strict";
import test from "node:test";
import { scheduleDeadline } from "../frontend/src/lib/deadline.ts";
import { parseRetryAfter } from "../frontend/src/lib/retryAfter.ts";

test("accepts retry seconds and canonical HTTP dates", () => {
  const now = Date.UTC(2026, 10, 28, 16);
  assert.equal(parseRetryAfter(" 005 ", now), 5_000);
  assert.equal(parseRetryAfter(new Date(now + 8_000).toUTCString(), now), 8_000);
  assert.equal(parseRetryAfter(new Date(now - 1_000).toUTCString(), now), 0);
  assert.equal(parseRetryAfter("9".repeat(400), now), Number.MAX_SAFE_INTEGER);
});

test("discards ambiguous or malformed retry hints", () => {
  for (const value of [null, "", "+2", "-2", "1.5", "1, 5", "Nov 28 2026 16:00:08 GMT"]) {
    assert.equal(parseRetryAfter(value), undefined);
  }
});

test("checks a long retry deadline against the supplied monotonic clock", (t) => {
  const browserLimit = 2_147_483_647;
  const wait = parseRetryAfter("2592000");
  let monotonic = 1_025.25;
  let scheduled;
  let fired = false;
  t.mock.method(Date, "now", () => Number.MAX_SAFE_INTEGER);
  globalThis.window = {
    setTimeout(callback, delay) {
      scheduled = { callback, delay };
      return 1;
    },
    clearTimeout() { scheduled = undefined; },
  };
  t.after(() => { delete globalThis.window; });

  scheduleDeadline(monotonic + wait, () => { fired = true; }, () => monotonic);
  assert.equal(fired, false);
  assert.equal(scheduled.delay, browserLimit);
  monotonic += scheduled.delay;
  scheduled.callback();
  assert.equal(fired, false);
  assert.equal(scheduled.delay, wait - browserLimit);
  monotonic += scheduled.delay;
  scheduled.callback();
  assert.equal(fired, true);
});
