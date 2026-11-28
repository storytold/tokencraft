import assert from "node:assert/strict";
import test from "node:test";
import {
  creationStorageKey, pendingWebhookCreation, pendingWebhookRotation, rotationStorageKey,
  stageWebhookCreation, stageWebhookRotation,
} from "../frontend/src/surfaces/projects/webhookReceipts.ts";

function storage(t) {
  const records = new Map();
  const existing = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");
  Object.defineProperty(globalThis, "sessionStorage", {
    configurable: true,
    value: {
      getItem: (key) => records.get(key) ?? null,
      setItem: (key, value) => records.set(key, value),
      removeItem: (key) => records.delete(key),
    },
  });
  t.after(() => {
    if (existing) Object.defineProperty(globalThis, "sessionStorage", existing);
    else delete globalThis.sessionStorage;
  });
  return records;
}

test("the browser can recover a rotation across reloads without storing the signing secret", async (t) => {
  const records = storage(t);
  const key = rotationStorageKey("owner", "account", "project", "hook");
  const attempt = stageWebhookRotation(key, 1);
  assert.deepEqual(Object.keys(JSON.parse(records.get(key))).sort(), ["expectedVersion", "key", "stagedAt"]);
  assert.deepEqual(stageWebhookRotation(key, 2), attempt);

  const reloaded = await import("../frontend/src/surfaces/projects/webhookReceipts.ts?reloaded");
  assert.deepEqual(reloaded.pendingWebhookRotation(key, 1), attempt);
  assert.deepEqual(reloaded.pendingWebhookRotation(key, 2), attempt);
  assert.equal(reloaded.pendingWebhookRotation(key, 3), undefined);
  assert.equal(reloaded.pendingWebhookRotation(rotationStorageKey("other-owner", "account", "project", "hook"), 2), undefined);
  assert.equal(reloaded.pendingWebhookRotation(rotationStorageKey("owner", "account", "different-project", "hook"), 2), undefined);
  reloaded.clearWebhookReceipt(key);
  assert.equal(records.has(key), false);
});

test("creation keeps the original intent across reloads without storing a signing secret", async (t) => {
  const records = storage(t);
  const scope = creationStorageKey("owner", "account", "project");
  const input = { name: "Workflow completed", destination_id: "fixture", client_ref: null, workflow_app_id: "wfa_fixture" };
  const attempt = stageWebhookCreation(scope, input);
  assert.deepEqual(Object.keys(JSON.parse(records.get(scope))).sort(), ["input", "key", "stagedAt"]);
  assert.deepEqual(stageWebhookCreation(scope, { ...input, name: "Different" }), attempt);
  const reloaded = await import("../frontend/src/surfaces/projects/webhookReceipts.ts?creation-reloaded");
  assert.deepEqual(reloaded.pendingWebhookCreation(scope), attempt);
  assert.equal(reloaded.pendingWebhookCreation(creationStorageKey("owner", "account", "other-project")), undefined);
  reloaded.clearWebhookReceipt(scope);
  assert.equal(reloaded.pendingWebhookCreation(scope), undefined);
});

test("creation ignores malformed or stale stored intent", (t) => {
  const records = storage(t);
  const scope = creationStorageKey("owner", "account", "stale-project");
  const input = { name: "Fixture", destination_id: "fixture", client_ref: null };
  records.set(scope, JSON.stringify({ key: crypto.randomUUID(), input: { ...input, destination_id: [] }, stagedAt: Date.now() }));
  assert.equal(pendingWebhookCreation(scope), undefined);
  records.set(scope, JSON.stringify({ key: crypto.randomUUID(), input, stagedAt: Date.now() - 86_400_000 }));
  assert.equal(pendingWebhookCreation(scope), undefined);
});

test("expired or malformed browser receipts cannot be replayed", (t) => {
  const records = storage(t);
  const key = rotationStorageKey("admin", "account", "project", "hook");
  records.set(key, '{"expectedVersion":1,"key":"wrong","stagedAt":0}');
  assert.equal(pendingWebhookRotation(key, 1), undefined);
  records.set(key, JSON.stringify({ expectedVersion: 1, key: crypto.randomUUID(), stagedAt: Date.now() - 86_400_000 }));
  assert.equal(pendingWebhookRotation(key, 2), undefined);
});
