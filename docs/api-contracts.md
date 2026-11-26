# API contracts

Use the [OpenAPI 3.1 snapshot](../contracts/openapi.json) for HTTP clients and the [connector primitive registry](../crates/core/provider-mapping/schemas/v1.json) for provider mappings. A running gateway serves these contracts at `GET /openapi.json` and `GET /v1/schemas/primitives`. The OpenAPI catalog operations distinguish public offers and model quotes from project-scoped quotes.

Open `/docs/api` in the console for individual operation pages. The [API schema walkthrough](api-schema.md) explains the shared conventions from quote through delivery.

The hosted console publishes its release contract at `/openapi.json` and `/docs/openapi.json`. Both URLs serve the same checked-in snapshot used by the reference. Fetch `/openapi.json` from the gateway origin to inspect the contract from the running server.

The [hosted release check](deployment.md#run-behind-an-https-ingress) compares all three contract URLs and confirms that the console's `/v1/` proxy reaches a protected route from the release. A passing readiness probe alone establishes service readiness. Contract and routing checks establish release alignment.

## Manage project webhooks

When the operator configures webhook destinations, organization owners and administrators use their session cookie and the `account_id` query parameter to manage subscriptions and inspect deliveries. List the project's destination IDs, then create a subscription using one of those IDs. Request subscriptions accept an optional `client_ref`. To receive [workflow events](workflows.md#receive-terminal-events), supply `workflow_app_id` instead and omit `client_ref`. Save the signing secret from creation or rotation in the receiver's secret store. [Webhook deployment](deployment.md#run-behind-an-https-ingress)

The subscription and delivery lists return up to 50 items. Send the returned `next_before` value as `before` until it is null. The detail operation includes the latest 50 delivery attempts. Redrive a failed delivery while its subscription is active to queue the original event again. A project API key can attach an allowed subscription to a request using `delivery.webhook_subscription_id` on `POST /v1/requests`.

Delivery records identify their subject with `request_id` or `workflow_run_id`. App subscriptions only receive terminal events for the assigned app.

## Review and export a contract change

Start the [local gateway](setup.md#backend) from the same source you plan to release. Generate the two documents from it.

```sh
bash scripts/export_contracts.sh ./contract-preview
diff -u contracts/openapi.json ./contract-preview/openapi.json
diff -u crates/core/provider-mapping/schemas/v1.json ./contract-preview/connector-primitives.json
```

After reviewing an intentional OpenAPI change, update `contracts/openapi.json` with the generated file. Make primitive changes in the source registry. Review compatibility for changes to required inputs, status codes, permissions, or response fields before releasing the corresponding SDKs.

CI compares both served documents with their source snapshots and reports any differences in the job log. The `tokencraft-contracts` artifact contains the generated documents from the exact gateway that the workflow started. Download that artifact from the run associated with a release.
