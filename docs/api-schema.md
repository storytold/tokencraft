# API schema walkthrough

Open **Docs → API reference** in the TokenCraft console to browse `/docs/api`. Each operation has a shareable page with its credential requirements, request fields, headers, response codes, content types, and expandable schemas. The [backend design](../frontend/public/design/index.html#api-schema/caller) follows the same contract across the gateway and workers.

The reference renders the checked-in [OpenAPI 3.1 contract](../contracts/openapi.json). Pages labeled **Handler source** cover operations outside that snapshot, including project administration, archives, routing, and MCP. Feature-dependent paths require the corresponding gateway configuration. Use the [served contract](api-contracts.md) for client generation and compare it against the gateway you run.

## API conventions

| Boundary | What the caller should preserve or inspect |
| --- | --- |
| Authentication | Send `Authorization: Bearer` with a scoped project key for app traffic. Media submission requires `requests:submit`, polling requires `requests:read`, and compatible chat needs both. Account administrators use the session cookie with the authorized account and project. |
| Discovery | Read `GET /v1/catalog/models` for public aliases and `GET /v1/catalog/models/scoped` for the project’s view. Match the workload to the model's configured capabilities. |
| Quote | `POST /v1/quotes/scoped` returns `id`, normalized `input`, candidate estimates, and an expiry. Preserve the quoted model, workload, client scope, and offer pin when supplied. For media, `dry_run: true` returns a model quote with `200` and creates zero jobs. |
| Admission | `POST /v1/requests` requires a quote ID and `Idempotency-Key`. The same project, key, and normalized intent recover the original receipt, including after a policy change. A changed intent under the same key conflicts. New work returns `202` and a replay returns `200`. |
| Status | Follow `Location`. Send the previous `ETag` as `If-None-Match` to the scoped status endpoint. A matching representation returns `304`. Respect `Retry-After` while work is queued or running. |
| Errors | Native handler errors contain `error.code` and `error.message`. Treat `quote_stale` as a signal to quote again and `active_request_limit` as a signal to retry after work drains. A `429 submission_rate_limit` supplies a numeric `Retry-After`. Body extraction, compatible text, and MCP can return different wire formats. |
| Submission rate | New admissions share a per-organization bucket. The default is 120 requests/minute and a 20-request burst. An admitted replay and a quote preview consume zero new-work credits. Read or request the next policy tier with the [rate-limit APIs](capacity.md#request-a-higher-limit). |
| Money | A nano-USD is one billionth of a US dollar. Media's `max_estimated_cost_micros` uses whole millionths of a dollar. The header `X-TokenCraft-Max-Cost-Nanos` is an alternative on supported routes. Some usage and settlement totals are decimal strings. Keep estimate, provider report, and settlement evidence distinct. |
| Pagination | Continue with the cursor and the same filters and scope. Request history uses `next_after`. Some management lists use `next_before`. Export data uses response headers and versioned NDJSON. |
| Compatibility | The typed REST generation endpoints and OpenAI-compatible text endpoints each have their own wire shape. Both use the durable request lifecycle. Treat the public gateway contract, the connector protocol, and the vendor API as separate versioned interfaces. |

## Follow a request from quote to delivery

1. **Quote.** Send the workload to the scoped quote endpoint. The gateway validates the workload, resolves requester scope, merges request and inherited policy, checks eligible capabilities and provider access, and ranks candidates. Quote previews detail the price and any allowed adjustment. Invalid JSON fields or task combinations can fail before routing.
2. **Admit.** Send the quote's `id` as `quote_id`, its `input` unchanged, and the modality payload. Save the idempotency key and the submitted intent before transmitting. In the home cell, admission durably commits the receipt, reservation and job together. An identical retry recovers the receipt without rerouting.
3. **Execute.** A worker claims the job, checks its pinned binding and credentials, and maps the normalized request to the selected adapter. A queued media connector receives the [version 1 job](provider-connectors.md#queued-media-protocol-version-1) and returns a stable handle. An OpenAI-compatible chat binding receives text messages and generation options. A self-hosted binding can route execution among its eligible compute targets.
4. **Read.** Poll request status for asynchronous work and fetch result metadata, artifact bytes, or trace data using the authorized request ID. Artifact URLs appear after publication. Read headers such as `Content-Type` and `X-TokenCraft-Content-Sha256` when validating a download. Request history and traces carry lifecycle evidence. [Request diagnostics](request-diagnostics.md)
5. **Deliver.** For chat, `stream: true` selects SSE. Chat Completions emits `data:` frames and successful streams end with `[DONE]`. Responses uses named events with `sequence_number`. Replay the original text request and key to recover retained output. The worker owns upstream execution and the API serves retained chunks. For async completion, configure a webhook subscription. Verify the raw signed body and deduplicate on `webhook-id` before committing a side effect. [Content retention](chat-history.md) · [Webhook management](api-contracts.md#manage-project-webhooks)

Automatic quotes retain `input.model: "auto"` for submission and idempotent replay. Match `selected_offer_id` to a candidate to find its resolved `model`. Model request status and usage use the resolved model. [Routing policy precedence](deployment.md#routing-policy-precedence)

### Read a media receipt

A minimal fixture quote for `POST /v1/quotes/scoped` looks like this.

```json
{
  "model": "example_image",
  "workload": { "kind": "image", "count": 1, "aspect_ratio": "1:1" }
}
```

Submit the returned `input` and ID through `POST /v1/requests`. The placeholder below stands for the actual returned quote ID.

```json
{
  "quote_id": "COPY_THE_SCOPED_QUOTE_ID",
  "input": {
    "model": "example_image",
    "workload": { "kind": "image", "count": 1, "aspect_ratio": "1:1" }
  },
  "image_prompt": "A paper lantern under a starry sky."
}
```

The new request returns `202`, `Location`, and `TokenCraft-Request-Id`. The receipt JSON has `id`, `project_id`, and `status_url`. Store the full receipt and URL. Read the final status using a credential for the same project.

## Maintain the reference

Update the handler contract and [re-export OpenAPI](api-contracts.md#review-and-export-a-contract-change) when changing a public wire shape. When a routed operation is still outside OpenAPI, update its handler-backed entry in [api-source-reference.ts](../frontend/src/surfaces/marketing/api-source-reference.ts). Model-request aliases reuse the native schemas in [api-reference-data.ts](../frontend/src/surfaces/marketing/api-reference-data.ts). Use [api-schema-notes.ts](../frontend/src/surfaces/marketing/api-schema-notes.ts) for narrative and examples that clarify the generated contract.
