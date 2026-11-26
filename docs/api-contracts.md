# API contracts

Use the [OpenAPI 3.1 snapshot](../contracts/openapi.json) for HTTP clients and the [connector primitive registry](../crates/core/provider-mapping/schemas/v1.json) for provider mappings. A running gateway serves these contracts at `GET /openapi.json` and `GET /v1/schemas/primitives`. The OpenAPI catalog operations distinguish public offers and model quotes from project-scoped quotes.

Open `/docs/api` in the console for individual operation pages. The [API schema walkthrough](api-schema.md) explains the shared conventions from quote through delivery.

The hosted console publishes its release contract at `/openapi.json` and `/docs/openapi.json`. Both URLs serve the same checked-in snapshot used by the reference. Fetch `/openapi.json` from the gateway origin to inspect the contract from the running server.

The [hosted release check](deployment.md#run-behind-an-https-ingress) compares all three contract URLs and confirms that the console's `/v1/` proxy reaches a protected route from the release. A passing readiness probe alone establishes service readiness. Contract and routing checks establish release alignment.

## Manage project webhooks

When the operator configures webhook destinations, organization owners and administrators use their session cookie and the `account_id` query parameter to manage subscriptions and inspect deliveries. List the project's destination IDs, then create a subscription using one of those IDs. Request subscriptions accept an optional `client_ref`. To receive [workflow events](workflows.md#receive-terminal-events), supply `workflow_app_id` instead and omit `client_ref`. Save the signing secret from creation or rotation in the receiver's secret store. [Webhook deployment](deployment.md#run-behind-an-https-ingress)

The subscription and delivery lists return up to 50 items. Send the returned `next_before` value as `before` until it is null. The detail operation includes the latest 50 delivery attempts. Redrive a failed delivery while its subscription is active to queue the original event again. A project API key can attach an allowed subscription to a request using `delivery.webhook_subscription_id` on `POST /v1/requests`.

Delivery records identify their subject with `request_id` or `workflow_run_id`. App subscriptions only receive terminal events for the assigned app.

## Inspect a workflow run

The gateway console uses the saved run graph for the timeline. A project developer or administrator can request that timeline by adding `include_execution=true` and `account_id` to the run status URL with a session cookie. Application keys continue to receive the published workflow status contract.

The timeline includes the original step names and any retained child request IDs, statuses, providers, and timestamps. The step status describes the workflow. The request status describes the child inference request. When run content expires, the timeline continues to link to retained requests. Tool details and output content follow their existing retention settings.

From a child request trace, open its workflow run to return to the saved timeline. Scoped console links include the project, workflow, and run IDs so they can be reopened directly. With a project session, `GET /v1/projects/{project}/workflow-activity/parents/{request}?account_id={account}` returns the parent metadata or `null` for a standalone request.

## Attribute published workflow usage to a client

Send an optional top-level `client_ref` with the published workflow's `inputs`. For example, send `{"inputs":{},"client_ref":"studio-customer-42"}` for a workflow without required inputs. Use a stable opaque identifier. The run status echoes a supplied reference, and child requests use it in normal request history and usage filters. An invocation without it keeps the app ID as the child request reference.

A signed terminal webhook includes the same reference in `data.client_ref`, so a shared app receiver can route notifications to the associated customer. Runs submitted without a reference omit the field. Existing webhook deliveries retain their original payload.

An app key can submit for multiple customers. The application budget still covers all of that app's runs. A configured client budget also covers the tagged customer's runs and native requests. The idempotency key stays scoped to the app and workflow. Reusing it with a different client reference for the same invocation returns HTTP 409.

## Review and export a contract change

Start the [local gateway](setup.md#backend) from the same source you plan to release. Generate the two documents from it.

```sh
bash scripts/export_contracts.sh ./contract-preview
diff -u contracts/openapi.json ./contract-preview/openapi.json
diff -u crates/core/provider-mapping/schemas/v1.json ./contract-preview/connector-primitives.json
```

After reviewing an intentional OpenAPI change, update `contracts/openapi.json` with the generated file. Make primitive changes in the source registry. Review compatibility for changes to required inputs, status codes, permissions, or response fields before releasing the corresponding SDKs.

CI compares both served documents with their source snapshots and reports any differences in the job log. The `tokencraft-contracts` artifact contains the generated documents from the exact gateway that the workflow started. Download that artifact from the run associated with a release.
