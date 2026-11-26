# Inference plugins

These independent Node.js services translate TokenCraft's queue or chat protocol to a vendor API.

| Plugin | Workload | Default listener | Gateway adapter |
| --- | --- | --- | --- |
| [fal](fal/README.md) | Image, video, audio, and 3D generation | `127.0.0.1:4021` | `connector_queue` |
| [Kinovi](kinovi/README.md) | Seedance video generation | `127.0.0.1:4023` | `connector_queue` |
| [OpenRouter](openrouter/README.md) | Chat completions with JSON or SSE | `127.0.0.1:4022` | `openai_compatible_chat` |

## Start a plugin

1. Use Node.js 22 or newer. The plugins have no package dependencies. From the repository root, run the fixture tests with `npm --prefix plugins/providers/inference/fal test`, substituting the directory for your provider.
2. Set the vendor credentials and configuration in the plugin's environment as described in its README. Generate a service token for the binding, for example with `openssl rand -hex 32`. Set the same service token on both the gateway and the plugin.
3. Start the service with `npm --prefix plugins/providers/inference/fal start`, substituting your provider again. The sample bindings use loopback, so gateway processes that call the plugin need access to that listener in the same network namespace. To deploy across hosts, expose it through private HTTPS ingress.
4. Copy the plugin's example offer into your reviewed `catalog.json`. Copy its example binding into `connector-bindings.json` for fal or Kinovi, or `chat-bindings.json` for OpenRouter. The offer's `local_binding` must match the binding's `name`. Review the sample model settings and replace the placeholder rates before serving live traffic.
5. Run [binding preflight](../../../docs/provider-connectors.md#check-catalog-bindings) and [start TokenCraft with the reviewed profile](../../../docs/setup.md#start-with-a-reviewed-catalog). For headless deployment, set `TOKENCRAFT_CORE_CATALOG` and the applicable binding files with `TOKENCRAFT_CONNECTORS` and `TOKENCRAFT_LOCAL_CHAT_BINDINGS`.

Give each queued plugin instance a persistent private state directory. It needs that state to resume polling after a restart. Follow the [connector rollout procedure](../../../docs/deployment.md#check-a-connector-rollout) when replacing a live binding.

To register a plugin with [TokenCraft Operations](../../../deploy/monitoring/README.md#attach-providers-and-infrastructure), set a distinct **TOKENCRAFT_PLUGIN_MONITOR_TOKEN** in its environment. This enables the authenticated **GET /internal/monitoring** probe. Request JSON to read bounded, process-level traffic counts and latency buckets. Each documented plugin supports the same protocol and keeps submission and polling credentials independent.

## Correlate plugin logs

Set `TOKENCRAFT_PLUGIN_TRACE_LOGS=true` on the plugin to emit one `connector_request` JSON log to stdout when an authenticated request with valid gateway trace context finishes or disconnects. Log entries include the operation, execution trace ID, parent span ID, completion timestamp, elapsed milliseconds, HTTP status when sent, and an interruption flag. The entry is limited to those diagnostic fields and the plugin service name. The bundled plugins omit trace headers from vendor API and artifact requests. [Request diagnostics](../../../docs/request-diagnostics.md#follow-a-request-into-a-connector)
