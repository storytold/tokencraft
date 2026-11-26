# Workflows

A workflow runs a typed graph of TokenCraft requests in a project. Each draft can be tested. A successful test lets a project administrator publish the tested version to a private, stable endpoint and issue application credentials.

## Enable Workflows

Apply all migrations to the gateway database before starting its API and workers. Configure the scoped gateway catalog as normal. For a hosted deployment, set a persistent independent 32-byte hex `TOKENCRAFT_WORKFLOW_KEY` on both API and worker replicas. Save it with your recovery credentials. Local development can use the key derived from the persistent development payload key. Keep an existing explicit workflow key if you are restoring a database with workflow content and app keys.

## Build and invoke

1. In a project, open **Workflows**, then create a draft. Choose generation steps, connect their inputs and outputs, and set step spending limits and a total run cap.
2. Run the draft as a project developer or admin. Review the original child request IDs and wait for success.
3. Publish the tested draft revision as a project admin. Create an application with only the scopes the caller needs. Save its key in the application secret store.
4. Invoke the endpoint and poll its status URL with the same application key. An identical resubmission with the same idempotency key returns the original run.

```sh
curl --request POST "$TOKENCRAFT_URL/v1/projects/$PROJECT_ID/workflow-endpoints/$WORKFLOW_ID/runs" \
  --header "Authorization: Bearer $WORKFLOW_API_KEY" \
  --header "Idempotency-Key: studio-run-001" \
  --header "Content-Type: application/json" \
  --data '{"inputs":{"brief":"A studio portrait on a blue background."}}'
```

Only published application keys with `runs:submit` can invoke that endpoint. Use `runs:read` to read the application's runs, `outputs:read` to retrieve its output artifacts, and `runs:cancel` to stop new child admission. Rotating a key preserves the application identity. A pending connector job retains its own lifecycle after a run stops.

Workflow versions pin the graph. The backend checkpoints runs under database leases and replays child admission using a stable step key. Each generation step passes the current project allocation and provider policy checks. Media results are accessed under the workflow's existing project and app scope.

For opt-in binding setup and the typed tool standard, see [Media tool plugins](media-tools.md).
