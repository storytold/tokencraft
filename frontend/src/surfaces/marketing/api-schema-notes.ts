import { apiReferencePath } from "./api-reference-path";
import type { Operation, Parameter, ResponseSchema, Schema } from "./api-schema-types";

type Note = {
  enrich?: (operation: Operation) => Operation;
  related?: { label: string; href: string };
};

const ref = (name: string): Schema => ({ $ref: "#/components/schemas/" + name });
const json = (schema: Schema, example?: unknown) => ({ "application/json": { schema, example } });
const header = (name: string, description: string, type = "string"): Parameter => ({
  in: "header", name, schema: { type }, description,
});
const receiptHeaders = {
  Location: { schema: { type: "string" }, description: "Follow this URL with an authorized key to read request status." },
  "TokenCraft-Request-Id": { schema: { type: "string" }, description: "Canonical durable request ID." },
  "TokenCraft-Trace-Id": { schema: { type: "string" }, description: "Trace ID when recorded." },
};
const receipt: ResponseSchema = { description: "A new request was accepted.", headers: receiptHeaders, content: json(ref("AcceptedRequest")) };
const withExample = (operation: Operation, example: unknown): Operation["requestBody"] => {
  const body = operation.requestBody;
  return body && {
    ...body, content: { ...body.content, "application/json": { ...body.content["application/json"], example } },
  };
};

function media(primitive: string, summary: string, task: string, workload: Record<string, unknown>, rules: string): Note {
  return {
    related: { label: "Primitive and model capabilities", href: "/docs/primitives/" + primitive },
    enrich: (operation) => ({
      ...operation,
      summary,
      description: rules + " Set dry_run=true for a scoped preview without admitting work. Submit with an Idempotency-Key. An optional quote_id pins the reviewed preview, and a stale quote requires a fresh preview.",
      parameters: [
        ...(operation.parameters ?? []),
        header("X-TokenCraft-Client-Ref", "Authorized client reference."),
        header("X-TokenCraft-App-Ref", "Application reference for history and usage."),
        header("X-TokenCraft-Max-Cost-Nanos", "Integer estimated-cost ceiling in nano-USD. Omit max_estimated_cost_micros when using this header.", "integer"),
        header("X-TokenCraft-Webhook-Subscription-Id", "Authorized terminal-event subscription."),
      ],
      requestBody: withExample(operation, {
        ...(primitive === "image" ? { model: "example_image" } : {}),
        task, prompt: "A paper lantern under a starry sky.", ...workload, dry_run: true,
      }),
      responses: {
        ...operation.responses,
        200: {
          description: "ModelQuote for a dry run, or AcceptedRequest for an identical submission replay. Receipt headers apply only to a replay.",
          headers: receiptHeaders, content: json({ oneOf: [ref("ModelQuote"), ref("AcceptedRequest")] }),
        },
        202: receipt,
        400: { description: "Invalid task, fields, or cost cap.", content: json(ref("GatewayError")) },
        409: { description: "Stale quote, changed intent, or admission conflict.", content: json(ref("GatewayError")) },
      },
    }),
  };
}

const model = "YOUR_CHAT_MODEL";

export const schemaNotes: Record<string, Note> = {
  "post /v1/image": media("image", "Preview or submit an image request", "text_to_image", { count: 1, aspect_ratio: "1:1" },
    "Use text_to_image. Count defaults to one, aspect ratio defaults to 1:1, and the supported output format is png."),
  "post /v1/video": media("video", "Preview or submit a video request", "text_to_video",
    { duration_seconds: 5, aspect_ratio: "16:9", resolution: "720p" },
    "Use text_to_video with duration_seconds, aspect_ratio, and resolution. The request supports one MP4."),
  "post /v1/audio": media("audio", "Preview or submit an audio request", "text_to_music", { duration_seconds: 5, output_format: "wav" },
    "Use text_to_music or text_to_sound_effect with duration_seconds. Output defaults to wav. Select a model whose active offer supports the task."),
  "post /v1/3d": media("3d", "Preview or submit a 3D mesh request", "text_to_3d", { output_format: "glb" },
    "Use text_to_3d. Output defaults to glb and requires a compatible compute offer."),
  "post /v1/quotes/scoped": {
    related: { label: "Submit a reviewed quote", href: apiReferencePath("POST", "/v1/requests") },
    enrich: (operation) => ({
      ...operation,
      requestBody: withExample(operation, { model: "example_image", workload: { kind: "image", count: 1, aspect_ratio: "1:1" } }),
    }),
  },
  "post /v1/requests": {
    related: { label: "Read request status", href: apiReferencePath("GET", "/v1/requests/{id}") },
    enrich: (operation) => ({
      ...operation,
      requestBody: withExample(operation, {
        quote_id: "COPY_THE_SCOPED_QUOTE_ID",
        input: { model: "example_image", workload: { kind: "image", count: 1, aspect_ratio: "1:1" } },
        image_prompt: "A paper lantern under a starry sky.",
      }),
    }),
  },
  "post /v1/chat/completions": {
    related: { label: "Text primitive guide", href: "/docs/primitives/text" },
    enrich: (operation) => ({
      ...operation, requestBody: withExample(operation, {
        model, messages: [{ role: "user", content: "Write one line about rain." }], max_completion_tokens: 64, stream: true,
      }),
    }),
  },
  "post /v1/responses": {
    related: { label: "Read native request status", href: apiReferencePath("GET", "/v1/requests/{id}") },
    enrich: (operation) => {
      const success = operation.responses["200"] as ResponseSchema;
      return {
        ...operation,
        description: "SSE uses named events and a sequence_number on each JSON event. The response begins with response.created and response.in_progress, emits response.output_text.delta as text arrives, and ends with response.completed, response.incomplete, or error.",
        requestBody: withExample(operation, { model, input: "Write one line about rain.", max_output_tokens: 64, stream: true }),
        responses: {
          ...operation.responses,
          200: {
            ...success, content: {
              ...success.content,
              "text/event-stream": {
                schema: { type: "string", description: "Named SSE events with JSON data. The following excerpt shows a text delta." },
                example: 'event: response.output_text.delta\ndata: {"type":"response.output_text.delta","sequence_number":5,"output_index":0,"item_id":"msg_example","content_index":0,"delta":"Rain","logprobs":[]}\n\n',
              },
            },
          },
        },
      };
    },
  },
  "get /v1/requests/{id}": { related: { label: "Inspect a request trace", href: apiReferencePath("GET", "/v1/requests/{id}/trace") } },
  "get /v1/requests/{id}/trace": { related: { label: "Read the OpenTelemetry trace", href: apiReferencePath("GET", "/v1/requests/{id}/trace/otlp") } },
  "get /v1/exports/events": { related: { label: "Create a bounded export snapshot", href: apiReferencePath("POST", "/v1/exports") } },
  "get /v1/rate-limits": {
    related: { label: "Request the next tier", href: apiReferencePath("POST", "/v1/rate-limits/increase") },
    enrich: (operation) => ({
      ...operation,
      description: "Organization owners and admins read the shared submission policy for every project in that organization. The starting policy allows 120 new requests per minute and a 20-request burst. Read next_self_service_limit and next_self_service_at to check eligibility.",
    }),
  },
  "post /v1/rate-limits/increase": {
    related: { label: "Read the current rate limit", href: apiReferencePath("GET", "/v1/rate-limits") },
    enrich: (operation) => ({
      ...operation,
      description: "Send the current tier as the expected value and request only the next advertised tier. The automatic ladder is 120, 240, 480, then 600 RPM. Subsequent increases wait 24 hours.",
      requestBody: withExample(operation, { expected_requests_per_minute: 120, requested_requests_per_minute: 240 }),
      responses: {
        ...operation.responses,
        400: { description: "The requested value is outside the next eligible tier.", content: json(ref("GatewayError")) },
        409: { description: "The expected tier is stale, the cooldown has not elapsed, or the operator manages this organization's policy.", content: json(ref("GatewayError")) },
      },
    }),
  },
};
