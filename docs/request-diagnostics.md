# Request diagnostics

Use the durable ID returned by any request endpoint. A project key needs `requests:read` for the request's project and client.

| Endpoint | Evidence |
| --- | --- |
| `GET /v1/requests/{id}/trace` | Summary, recorded steps, logs, and the served provider and compute path |
| `GET /v1/requests/{id}/trace/baseline` | Median queue, elapsed, and publication times with the exact sample IDs |
| `GET /v1/requests/{id}/trace/otlp` | Redacted OpenTelemetry timing in OTLP/HTTP JSON |

The baseline uses earlier requests that succeeded before admission. It compares the same project, client, app, model, workload, API profile, and served route from the previous seven days. The newest 50 matching requests by admission time contribute to the medians. Each timing reports the number with usable timestamps.

For example, use the original receipt ID with a project key.

```sh
curl -fsS -H "Authorization: Bearer $TOKENCRAFT_API_KEY" \
  "http://localhost:4000/v1/requests/$TOKENCRAFT_REQUEST_ID/trace"
```

The [Rust SDK](../sdk/rust/README.md#inspect-a-request) and [TypeScript SDK](../sdk/typescript/README.md) expose all three reads. Provider IDs come from the configured catalog, including custom connectors.

Both SDKs check that returned request status, events, and trace evidence refer to the requested ID. If a response names a different request, the SDK reports `invalid_response` with the requested ID and stops polling.

## Link a caller trace

Send your application's active span in the `traceparent` header on submission. Use `SubmitOptions.traceparent` in TypeScript or `client.clone().with_submit_traceparent(traceparent)?` in Rust. Both SDKs validate the header before sending it and reuse it for retries of that submission. [W3C Trace Context](https://www.w3.org/TR/trace-context/#traceparent-header)

TokenCraft assigns its own execution trace ID and records the caller's trace and span IDs in the request's `correlation` field. The root span in the OTLP export links to the caller span. For a replay, the original link remains attached to the durable request.

Use `GET /v1/requests?trace_id=<trace-id>` to find requests by either execution or caller trace ID. The regular project and client permissions apply.

## Find requests with warnings

Add `diagnostic=warnings` to `GET /v1/requests` for requests with an unconfirmed outcome, a provider availability retry, a compute dispatch warning, or an expired lease among the latest 50 webhook delivery attempts. Use `diagnostic=failed_or_warning` to also include failed requests. Other filters still apply, including client, date, and status. The response echoes the applied diagnostic. Keep the same filters when following `next_after`.

The Requests console uses the same server-side filter. Use `client.list({ diagnostic: "warnings" })` in TypeScript or `client.list_diagnostic(filters, RequestDiagnostic::Warnings)` in Rust.

## Follow a request into a connector

The gateway sends its execution trace ID and the span ID of the root in its OTLP export to the configured provider endpoint in a W3C `traceparent` header. Submit, recovery lookup, status polling, artifact downloads, and compatible chat calls use the same parent across worker restarts and compute failover. The gateway keeps caller trace headers and baggage out of provider requests.

Enable `TOKENCRAFT_PLUGIN_TRACE_LOGS=true` on the fal, Kinovi, or OpenRouter plugin. Find its JSON logs by the execution `trace_id` and match `parent_span_id` to the downloaded OTLP root. Keep these logs in your operator log store. The scoped OTLP download contains gateway evidence.

[Inference plugin setup](../plugins/providers/inference/README.md#correlate-plugin-logs) · [W3C Trace Context](https://www.w3.org/TR/trace-context/#traceparent-header)
