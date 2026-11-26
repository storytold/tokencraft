# Workflows

A workflow runs a typed graph of TokenCraft requests in a project. Each draft can be tested. A successful test lets a project administrator publish the tested version to a private, stable endpoint and issue application credentials.

## Enable Workflows

Apply all migrations to the gateway database before starting its API and workers. Configure the scoped gateway catalog as normal. For a hosted deployment, set a persistent independent 32-byte hex `TOKENCRAFT_WORKFLOW_KEY` on both API and worker replicas. Save it with your recovery credentials. Local development can use the key derived from the persistent development payload key. Keep an existing explicit workflow key if you are restoring a database with workflow content and app keys.

## Build and invoke

1. In a project, open **Workflows**, then create a draft. Choose generation steps and any approved tools from the component library. Connect inputs and outputs and set generation spending limits and a total run cap.
2. Run the draft as a project developer or admin. Review the child request IDs and tool statuses, then wait for success.
3. Publish the tested draft revision as a project admin. Create an application with only the scopes the caller needs. Save its key in the application secret store.
4. Invoke the endpoint and poll its status URL with the same application key. An identical resubmission with the same idempotency key returns the original run.

```sh
curl --request POST "$TOKENCRAFT_URL/v1/projects/$PROJECT_ID/workflow-endpoints/$WORKFLOW_ID/runs" \
  --header "Authorization: Bearer $WORKFLOW_API_KEY" \
  --header "Idempotency-Key: studio-run-001" \
  --header "Content-Type: application/json" \
  --data '{"inputs":{"brief":"A studio portrait on a blue background."}}'
```

Fetch the workflow's OpenAPI document at `GET /v1/projects/{project_id}/workflow-endpoints/{workflow_id}/openapi.json` with an application key that has `runs:submit`. The document includes the active version's input fields, accepted upload media types, and named outputs. Each operation lists its required permission in `x-workflow-required-scope`. Set your deployed gateway URL as the base URL when importing the contract. Use `Retry-After` from the run receipt and `ETag` from the status response to poll efficiently.

Only published application keys with `runs:submit` can invoke that endpoint. Use `runs:read` to read the application's runs, `outputs:read` to retrieve its output artifacts, and `runs:cancel` to stop new child admission. Rotating a key preserves the application identity. A pending connector job retains its own lifecycle after a run stops.

An application can have two active runs across all its keys and API replicas. A workflow can have ten active runs across all applications and draft tests. Queued, running, and reconciling runs count toward capacity. On `429` with `active_request_limit`, wait for the `Retry-After` delay and retry with the same body and idempotency key. An identical replay of an already admitted run still returns its receipt.

Workflow versions pin the graph. The backend checkpoints runs under database leases and replays child admission using a stable step key. Each generation step passes the current project allocation and provider policy checks. Each tool step pins a reviewed project binding and an operation revision. Media results are accessed under the workflow's existing project and app scope.

Admission reserves the sum of the generation step caps in the current UTC month. That reservation counts toward organization and project estimate budgets and toward the published app’s client budget, if configured. A run that exceeds a budget returns `409 monthly_budget_exceeded` before any step is admitted. As each child is admitted, its estimated spend moves from the run reservation into that request’s budget record in the same transaction. A run ending releases its unused reservation. Child requests keep their existing accounting and use the parent’s UTC month even across a month boundary. Runs already admitted before schema 76 continue using individual child admission for budget accounting.

Runs have a 30-minute deadline. When the worker processes a due run past that deadline, PostgreSQL records `expired` and releases its unused reservation even if the saved checkpoint is unreadable. The status endpoint reports `run_deadline_exceeded`. Workers need a current lease to start a child or save a checkpoint. Previously admitted children retain their own execution and accounting.

Use the uploaded-file source in the step inspector when a workflow starts from existing media. For opt-in binding setup, upload API examples, and the typed tool standard, see [Media tool plugins](media-tools.md).

## Receive terminal events

Copy the app ID from the workflow's endpoint settings. In the same project's **Routing and access → Delivery** settings, select **Workflow app**, choose an operator-configured destination, and enter the app ID. An administrator can also create the project subscription with `workflow_app_id` through the [webhook management API](api-contracts.md#manage-project-webhooks). Each app can have one active subscription. Save its signing secret on the receiver.

The gateway pins the subscription when it admits a published run. Draft tests and runs admitted before registration are excluded. The gateway saves the terminal state and enqueues `workflow.run.succeeded`, `workflow.run.failed`, `workflow.run.canceled`, or `workflow.run.expired` in the same transaction. The event includes the run ID, workflow ID, project ID, version, status, and relative status URL. Use an app key with `runs:read` to fetch the run.

Verify the signature against the raw request body and timestamp with `verify_workflow_webhook` in the [Rust SDK](../sdk/rust/README.md) or `verifyWorkflowWebhook` in the [TypeScript SDK](../sdk/typescript/README.md). Acknowledge promptly and deduplicate using `webhook-id`. Retries and redrives preserve the original event identity and body. Inspect attempts and disable or rotate subscriptions from **Webhooks**.

## Verify a release

The **Gateway, connector, and SDKs** CI job runs the published workflow against disposable PostgreSQL, separate API and worker processes, and the standard-library delivery pack. From a source checkout with matching gateway and migration binaries and PostgreSQL tools on `PATH`, reproduce the check with `python3 -B scripts/smoke_workflow_contract.py target/debug/api-gateway target/debug/admin-cli`.
