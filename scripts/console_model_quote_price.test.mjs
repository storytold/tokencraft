import assert from "node:assert/strict";
import { test } from "node:test";
import { lowestCurrentQuotePrice } from "../frontend/src/surfaces/models/modelQuotePrice.ts";

const deadline = "2026-11-27T20:00:00Z";
const atDeadline = Date.parse(deadline);

test("model comparison removes expired supplier prices and keeps eligible alternatives", () => {
  const cheap = { reason: "eligible", estimated_usd_nanos: 1_000_000, rate_valid_until: deadline };
  const backup = { reason: "eligible", estimated_usd_nanos: 5_000_000 };
  const excluded = { reason: "workload_unsupported", estimated_usd_nanos: 0 };
  assert.equal(lowestCurrentQuotePrice({ candidates: [cheap, backup, excluded] }, atDeadline - 1), 0.001);
  assert.equal(lowestCurrentQuotePrice({ candidates: [cheap, backup, excluded] }, atDeadline), 0.005);
  assert.equal(lowestCurrentQuotePrice({ candidates: [cheap, excluded] }, atDeadline), null);
  assert.equal(lowestCurrentQuotePrice({ expires_at: deadline, candidates: [backup] }, atDeadline), null);
});

test("model comparison preserves zero prices and ignores invalid evidence", () => {
  const zero = { reason: "eligible", estimated_usd_nanos: 0 };
  const invalid = [
    { ...zero, estimated_usd_nanos: null },
    { ...zero, estimated_usd_nanos: 0.25 },
    { ...zero, estimated_usd_nanos: -1 },
    { ...zero, rate_valid_until: "unknown" },
  ];
  assert.equal(lowestCurrentQuotePrice({ candidates: invalid }, atDeadline), null);
  assert.equal(lowestCurrentQuotePrice({ candidates: [...invalid, zero] }, atDeadline), 0);
});
