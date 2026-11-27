import assert from "node:assert/strict";
import test from "node:test";
import { prepareModelSubmission, readModelReceipt } from "../frontend/src/surfaces/comparison/modelSubmission.ts";

const requestId = "req_01m8des3gv2esdtx1bxekn3bqg";

test("a staged submission keeps the original quote, scope, prompt, and delivery settings", () => {
  const scope = { account_id: "original-account", project_id: "original-project" };
  const quote = { id: "original-quote", input: { model: "original-model" } };
  const options = { clientRef: "client-a", appRef: "editor", subscription: "hook-a", promptMode: "image", prompt: "Original prompt" };
  const submission = prepareModelSubmission(scope, quote, options);

  scope.project_id = "different-project";
  quote.id = "refreshed-quote";
  quote.input.model = "different-model";
  options.prompt = "Updated draft";

  assert.deepEqual(JSON.parse(submission.body), {
    account_id: "original-account", project_id: "original-project", quote_id: "original-quote",
    input: { model: "original-model" }, client_ref: "client-a", app_ref: "editor",
    delivery: { webhook_subscription_id: "hook-a" }, image_prompt: "Original prompt",
  });
  assert.notEqual(submission.key, prepareModelSubmission(scope, quote, options).key);
});

test("an accepted receipt can be recovered from its header without a readable body", async () => {
  const headers = { "TokenCraft-Request-Id": requestId };
  assert.equal(await readModelReceipt(new Response("{", { status: 202, headers })), requestId);
  assert.equal(await readModelReceipt(Response.json({ id: requestId })), requestId);
  assert.equal(await readModelReceipt(new Response("{", { status: 200 })), null);
  assert.equal(await readModelReceipt(Response.json({ id: "bad-id" })), null);
  assert.equal(await readModelReceipt(new Response("{", { status: 503, headers })), null);
});
