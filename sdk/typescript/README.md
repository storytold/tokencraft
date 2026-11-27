# TokenCraft TypeScript SDK

Use Node.js 22 and the SDK lockfile to build the package. The runtime client uses Node's built-in fetch.

```sh
cd sdk/typescript
npm ci --ignore-scripts
npm test
npm pack --ignore-scripts
```

Install the generated tarball in your server project. Set `TOKENCRAFT_API_KEY` to a scoped project key and `TOKENCRAFT_REQUEST_ID` to an existing request ID.

```ts
import { TokenCraft } from "@tokencraft/sdk";

const client = new TokenCraft({ baseUrl: "http://localhost:4000", apiKey: process.env.TOKENCRAFT_API_KEY });
const requestId = process.env.TOKENCRAFT_REQUEST_ID!;
const trace = await client.trace(requestId);
const baseline = await client.traceBaseline(requestId);
const otlpJson = JSON.stringify(await client.traceOtlp(requestId));
```

Read the [request diagnostics guide](../../docs/request-diagnostics.md) for scopes and baseline criteria. Provider IDs are the strings declared in the gateway catalog.

Native submissions and published workflow invocations make one attempt by default. Set `submitMaxAttempts: 3` to enable retries after a connection error, an interrupted success response, or a `429` or `503` with a valid `Retry-After` in seconds or as an HTTP date. The limit includes the first attempt. Every retry uses the same serialized body and idempotency key within the original `timeoutMs`. Persist the quote, submission options, and key so you can replay the same intent if the client exhausts its attempts. During polling, `wait` follows the same HTTP hints within its own deadline and retains the last request validator.

Artifact and workflow-output downloads also make one attempt by default. Set `downloadMaxAttempts: 3` to retry a `429` or `503` with a valid `Retry-After`. One `timeoutMs` and the caller's abort signal cover the wait, repeat requests, and final verified transfer.

The SDK gives an HTTP error body up to one second and 64 KiB within the operation deadline. If it cannot decode the error details, `TokenCraftError` reports `http_error` with the HTTP status and any available request ID.

For a shared project key, pass the same `clientRef` to `quote(input, { clientRef })` and `submit(quote, { idempotencyKey, clientRef, ...options })`.

To link the active span of your application, pass its trace header to `submit(quote, { idempotencyKey, traceparent })`. Call `trace(receipt.id)` to read the stored correlation, or use `list({ traceId: callerTraceId })` to find the request by the caller's trace ID. [Trace correlation](../../docs/request-diagnostics.md#link-a-caller-trace)

## Compare priced offers

Use offer IDs from your gateway's configured catalog. Quote the same workload across two to four offers, set a total estimate cap in whole nano-USD, and submit the same prompts to each offer.

```ts
import { planOfferComparison, refreshOfferComparison, runOfferComparison } from "@tokencraft/sdk";

const plan = await planOfferComparison(client, {
  runId: "image-test-01",
  input: { model: "studio_image", workload: { kind: "image", count: 1, aspect_ratio: "1:1" } },
  offerIds: ["studio-image-fal", "studio-image-local"],
  prompts: ["A still life with an orange glass vase"],
  maxEstimatedUsdNanos: 200_000_000,
});
const report = await runOfferComparison(client, plan);
// Save the plan and report in your own project storage.

const refreshed = await refreshOfferComparison(client, plan, report);
```

Refresh uses project-authorized request reads with the saved request IDs and works after the quotes expire. It preserves earlier output metadata and updates observed outcomes and costs. Each arm reports USD spending by evidence source and native amounts by provider and unit. Check each sample's `refreshErrorCode` for a read that could not update its observations.

## Read retained history

Use `contentMetadata` to check dates, phase, and any restore progress without fetching input or output. `warmContent` reads available warm content. `retrieveContent` reads an available archive. Each operation uses the project's `requests:read` scope and the API key's client restriction.

```ts
import { writeFile } from "node:fs/promises";

const metadata = await client.contentMetadata(requestId);
if (metadata.phase === "archived") {
  const { result } = await client.retrieveContent(requestId);
  if (result && "images" in result && result.images[0]?.artifact) {
    const file = await client.downloadArtifact(requestId, result.images[0].artifact, { location: "archived" });
    await writeFile("output.png", file.bytes);
  }
}
```

A cold object can return a `TokenCraftError` with status `409` and code `archive_retrieval_required`. A project administrator can start a restore in Requests. The SDK downloads stored output from the configured gateway and verifies the saved byte length and SHA-256. The default artifact location is `warm`. [Retention and restore guide](../../docs/chat-history.md)

## Run a published workflow

Create a client with the published workflow's application key. Use the active OpenAPI document to find the input names, supported file types, and named outputs. This example uses an endpoint with `video` and `narration` inputs and a `master` output.

```ts
import { readFile, writeFile } from "node:fs/promises";
import { TokenCraft } from "@tokencraft/sdk";

const client = new TokenCraft({ baseUrl, apiKey: workflowAppKey });
const endpoint = client.workflow(projectId, workflowId);
const contract = await endpoint.contract();
const video = await endpoint.upload("video/mp4", await readFile("source.mp4"));
const narration = await endpoint.upload("audio/wav", await readFile("narration.wav"));
const receipt = await endpoint.invoke({ video, narration }, { idempotencyKey: jobKey, clientRef: customerRef });
const run = await endpoint.wait(receipt.id, { timeoutMs: 120_000 });
if (run.status === "succeeded") {
  const output = await endpoint.downloadOutput(run.id, "master");
  await writeFile("master.mp4", output.bytes);
}
```

Save the invocation inputs and job key to replay admission after an interruption. Once you have the run ID, use `get` or `wait` to reopen it. Pass `reuseRunId` for a new invocation that can reuse eligible steps from a prior run. A timeout or `AbortSignal` stops the local wait. Call `cancel` with a key that has `runs:cancel` to stop further admission. The download method builds a scoped gateway route from the run ID and output name and verifies the SHA-256 returned with the bytes. [Workflow permissions and lifecycle](../../docs/workflows.md#build-and-invoke)

## Verify a terminal webhook

Call `verifyWebhook(rawBody, headers, signingSecret)` for request events and `verifyWorkflowWebhook(rawBody, headers, signingSecret)` for [workflow events](../../docs/workflows.md#receive-terminal-events). Pass the original bytes before parsing the body. Both helpers enforce a five-minute timestamp window by default and accept overlapping keys during rotation. Deduplicate by the verified event ID before applying side effects. Use the optional `event.data.client_ref` to read the client reference from a tagged request or run.
