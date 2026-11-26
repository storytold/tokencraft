# Compute placement

A compute stack gives a chat offer a list of OpenAI-compatible endpoints that a cell's workers can use. TokenCraft's existing gateway code selects the target. Provision and scale the endpoints with your own infrastructure tooling.

## Configure a cell

1. Start with the [sample catalog](examples/two-target-chat/catalog.json) and [chat bindings](examples/two-target-chat/chat-bindings.json). The sample points at two local chat endpoints on ports 4031 and 4032. Supply endpoints that serve the same upstream model and capabilities. Replace the example rates before handling live traffic.
2. Set the profile's `cell` to your cell ID. Give each target a distinct ID and URL, and set its cloud, region, zone, and priority. A stack accepts up to four targets and four allowed cloud-region pairs. If the offer declares `data_handling.processing_regions`, include every eligible target region. Deploy the matching catalog and bindings to the API and worker replicas in that cell.
3. Use the exact `/v1/chat/completions` path. HTTP is supported on literal loopback addresses for local development. Remote targets require HTTPS and a `key_env` reference beginning with `TOKENCRAFT_LOCAL_CHAT_KEY_`. Supply the referenced bearer key through the process environment.
4. Run the following command from the repository root. It validates configuration using the gateway's loaders without connecting to the endpoints or database.

```sh
TOKENCRAFT_LOCAL_CHAT_BINDINGS=plugins/providers/compute/examples/two-target-chat/chat-bindings.json \
  api-gateway catalog check-bindings \
  --catalog plugins/providers/compute/examples/two-target-chat/catalog.json
```

Use the files with [a reviewed dev profile](../../../docs/setup.md#start-with-a-reviewed-catalog) or a [headless gateway deployment](../../../docs/provider-connectors.md#attach-an-implementation).

## Target selection and fallback

The worker tries lower priority numbers first and uses a stable order per request for equal priorities. A connection failure can move the same request to the next eligible target. Enable `retry_on_rate_limit` only for a target that guarantees its HTTP 429 means the request was rejected before execution. A timeout with unknown acceptance, an upstream 5xx, or an interrupted response stream ends that attempt without trying another target for the same request. Fallback keeps request state and ownership in the home cell.

Use the [request trace](../../../docs/request-diagnostics.md) to inspect the selected target and attempted compute path.

Point [TokenCraft Operations](../../../deploy/monitoring/README.md#import-a-deployed-compute-profile) at the same catalog and chat-binding files to track model availability, response timing and fallback outcomes for each compute target. Attach a compatible GPU exporter to report utilization, temperature and memory.
