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

Native submission defaults to one attempt. Set `submitMaxAttempts: 3` in the client options to retry a `429` or `503` that carries a valid `Retry-After` in seconds or as an HTTP date. The limit includes the first attempt, and the client reuses the same serialized body and idempotency key. All attempts and delays share `timeoutMs`. Persist the quote, submission options, and key so you can explicitly replay the same intent after an interruption. During polling, `wait` follows the same hints within its own deadline and retains the last request validator.

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

## Verify a terminal webhook

Call `verifyWebhook(rawBody, headers, signingSecret)` for request events and `verifyWorkflowWebhook(rawBody, headers, signingSecret)` for [workflow events](../../docs/workflows.md#receive-terminal-events). Pass the original bytes before parsing the body. Both helpers enforce a five-minute timestamp window by default and accept overlapping keys during rotation. Deduplicate by the verified event ID before applying side effects. A tagged workflow event includes the invocation's customer reference in `event.data.client_ref`.
