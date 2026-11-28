import { apiReferencePath } from "./api-reference-path";

type Endpoint = readonly [method: "GET" | "POST" | "PUT", path: string, summary: string, doc?: string];
export type DocsLink = { label: string; href: string; external?: boolean };
export type Snippet = { label: string; code: string; lang: "bash" | "json" };
export type Topic = {
  id: string;
  title: string;
  paragraphs: string[];
  keywords?: string[];
  links?: DocsLink[];
  example?: Snippet;
  endpoints?: Endpoint[];
};
export type DocsSection = { id: string; title: string; group: string; topics: Topic[] };

export const docsTitle = "TokenCraft documentation";
export const docsDescription = "Get an API key, verify your connection, and integrate TokenCraft into your app or coding agent.";

const repositoryDocs = "https://github.com/storytold/tokencraft/blob/main/";
const sourceGuide = (label: string, path: string): DocsLink => ({ label, href: repositoryDocs + path, external: true });

const gateway: Endpoint[] = [
  ["GET", "/v1/catalog/models", "List public model aliases and capabilities."],
  ["GET", "/v1/catalog/models/scoped", "List public models and offers shared with your project."],
  ["POST", "/v1/image", "Preview or submit text-to-image work."],
  ["POST", "/v1/video", "Preview or submit text-to-video work."],
  ["POST", "/v1/audio", "Preview or submit audio work."],
  ["POST", "/v1/3d", "Preview or submit 3D mesh work."],
  ["GET", "/v1/schemas/primitives", "Read the versioned connector primitive registry."],
  ["GET", "/v1/model-requests/{id}", "Poll the generated request and read its selected model."],
];

export function endpointId(method: string, path: string) {
  return `endpoint-${method.toLowerCase()}-${path.slice(1).replace(/[{}]/g, "").replaceAll("/", "-")}`;
}

function shellQuote(value: string) {
  return "'" + value.replaceAll("'", "'\\''") + "'";
}

export function buildDocs({ baseUrl, demo, keyPath }: { baseUrl: string; demo: boolean; keyPath: string }) {
  const connectionPath = "/v1/requests";
  const contractUrl = import.meta.env.DEV ? baseUrl.replace(/\/$/, "") + "/openapi.json" : "/docs/openapi.json";
  const authorization = 'Authorization: Bearer $TOKENCRAFT_API_KEY';
  const connection: Snippet = {
    label: "Shell",
    lang: "bash",
    code: [
      `export TOKENCRAFT_BASE_URL=${shellQuote(baseUrl)}`,
      "",
      `curl -fsS "$TOKENCRAFT_BASE_URL${connectionPath}" \\`,
      `  -H "${authorization}"`,
    ].join("\n"),
  };
  const sections: DocsSection[] = [
    {
      id: "quickstart", title: "Quickstart", group: "Get started", topics: [
        {
          id: "get-api-key", title: "Create an API key",
          paragraphs: [demo
            ? "Create an account with a disposable email or sign in. Choose Create API key to issue a 30-day project key for fixture requests. This key works against your local gateway and requires no provider credentials."
            : "Use a project credential issued by the operator of your TokenCraft gateway."],
          keywords: ["create key", "get a key", "api key", "access token", "sign up", "credentials", "connect"],
          links: [{ label: "Get an API key", href: keyPath }],
        },
        {
          id: "configure-environment", title: "Set your environment",
          paragraphs: [
            "Store the issued value as `TOKENCRAFT_API_KEY` in your shell or server-side secret manager. Keep it out of browser code, your repository, chat, and logs.",
            `Use \`${baseUrl}\` as the gateway base URL.`,
          ],
          keywords: ["env var", "environment variable", "where do i put", "export api key", "secret manager", "configure", "base url"],
          links: [{ label: "Authentication", href: "#authentication" }],
        },
        {
          id: "verify-connection", title: "Verify the connection",
          paragraphs: [`Run the read-only shell command with your key set. A successful response from \`GET ${connectionPath}\` confirms the connection. An empty list is valid.`],
          keywords: ["first request", "test my key", "verify my key", "check connection", "health check", "read only", "how do i start", "setup", "curl"],
          example: connection,
          links: [{ label: "Troubleshooting", href: "#troubleshooting" }],
        },
      ],
    },
    {
      id: "authentication", title: "Authentication", group: "Get started", topics: [
        {
          id: "bearer-auth", title: "Send a bearer token",
          paragraphs: [
            "Send `Authorization: Bearer $TOKENCRAFT_API_KEY` from your server or trusted local process.",
            "Use a project key with `requests:submit` for media generation and `requests:read` to poll it. Compatible chat needs both scopes. Check each endpoint for its accepted credentials.",
          ],
          keywords: ["authenticate", "authentication", "bearer", "authorization", "http header", "project scope", "permissions", "client ref"],
          links: demo
            ? [{ label: "Manage project keys", href: "/dashboard/projects" }, { label: "Manage organization keys", href: "/dashboard/keys" }]
            : [{ label: "Manage keys", href: "/dashboard/keys" }],
        },
      ],
    },
    {
      id: "agent-setup", title: "Set up with an agent", group: "Get started", topics: [
        {
          id: "agent-context", title: "Hand off setup to your agent",
          paragraphs: [
            "Use Copy context to paste this whole guide into your coding agent. It includes the gateway URL, example commands, endpoint reference, and direct links.",
            "Set `TOKENCRAFT_API_KEY` in the environment or secret manager the agent can use. Have the agent run the read-only connection check and report the HTTP status.",
            ...(demo ? ["For a loopback gateway, use an agent running on the same machine as the gateway."] : []),
          ],
          keywords: ["agent", "codex", "claude", "copy context", "paste", "coding assistant", "handoff"],
          links: [
            { label: "Connection check", href: "#verify-connection" },
            { label: "Connect ChatGPT or Claude", href: "/connect/agents" },
          ],
        },
      ],
    },
    {
      id: "model-and-job-guide", title: "Models and generation", group: "Build", topics: [
        {
          id: "discover-models", title: "Choose a primitive or a model",
          paragraphs: ["Start with the output you need and discover the model aliases and capabilities in your gateway’s scoped catalog. Use an alias in the `model` field to pin a model. Omit it on a media endpoint to inherit the configured selection policy."],
          keywords: ["model", "list models", "browse", "discover", "slug", "available models"],
          links: [
            { label: "Image schema", href: "/docs/primitives/image" }, { label: "Video schema", href: "/docs/primitives/video" },
            { label: "Audio preview", href: "/docs/primitives/audio" }, { label: "3D mesh preview", href: "/docs/primitives/3d" },
            { label: "World preview", href: "/docs/primitives/world" }, { label: "Text and chat", href: "/docs/primitives/text" },
            { label: "Browse models", href: "/models" },
          ],
        },
        {
          id: "estimate-generation", title: "Submit directly and follow the job",
          paragraphs: [
            "Send a supported `task` and its inputs to the media endpoint. TokenCraft checks the fields and matches the workload to eligible offers. Set `max_estimated_cost_micros` to cap the estimate in whole micro-USD.",
            "Use `dry_run: true` for a quote preview. It returns a `200` comparison without creating a job. Unknown fields or incompatible task settings are rejected. Review the selected model and any allowed adjustments in the quote.",
            "To submit, omit `dry_run` and send a stable `Idempotency-Key`. For a reviewed estimate, include the preview’s `quote_id` and use its concrete model if the Advisor selected one. A new request returns `202`. Poll `Location` for status.",
          ],
          keywords: ["generate", "image generation", "video generation", "estimate cost", "poll job", "first generation", "price", "how much", "dropped fields", "warnings", "request debugging"],
          example: { label: "Shell", lang: "bash", code: [
            'curl -fsS -i "$TOKENCRAFT_BASE_URL/v1/image" \\',
            '  -H "Authorization: Bearer $TOKENCRAFT_API_KEY" \\',
            "  -H 'Content-Type: application/json' \\",
            "  -d '{",
            '    "task": "text_to_image",',
            '    "prompt": "Studio photograph of a cobalt cup",',
            '    "dry_run": true',
            "  }'",
          ].join("\n") },
          links: [
            { label: "Model and job endpoints", href: "#gateway-endpoints" },
            { label: "Inspect generation jobs", href: demo ? "/dashboard/requests/generation" : "/dashboard/requests" },
          ],
        },
      ],
    },
    {
      id: "troubleshooting", title: "Troubleshooting", group: "Build", topics: [
        {
          id: "auth-errors", title: "Authentication and access",
          paragraphs: [
            "For `401`, check that `TOKENCRAFT_API_KEY` is set, the Authorization header uses Bearer, and the key is current.",
            "For `403`, check that your key belongs to the organization with access to the requested resource.",
          ],
          keywords: ["401", "403", "unauthorized", "forbidden", "access denied", "invalid key", "expired key", "missing key", "permission denied"],
          links: [{ label: "Authentication", href: "#authentication" }],
        },
        {
          id: "connection-errors", title: "Connection errors",
          paragraphs: [
            "Check that the command uses the gateway base URL and that your shell or agent can reach it.",
            ...(demo ? ["For a loopback fixture, confirm the gateway is running and run your client on the same machine. Demo endpoints require a gateway with the local request routes enabled."] : []),
          ],
          keywords: ["connection refused", "network error", "unreachable", "failed to connect", "localhost", "404", "wrong url"],
          links: [{ label: "Connection check", href: "#verify-connection" }],
        },
      ],
    },
    {
      id: "gateway-endpoints", title: "Model and job endpoints", group: "Reference", topics: [
        { id: "model-job-reference", title: "Catalog, generation, and jobs", paragraphs: [], endpoints: gateway },
        {
          id: "openapi-reference", title: "OpenAPI contract",
          paragraphs: ["Browse the complete operation reference or download the OpenAPI contract for this console release. Check the gateway you target for the routes and providers it enables."],
          keywords: ["openapi", "schema", "swagger", "generate client", "download api contract"],
          links: [
            { label: "Browse all endpoint schemas", href: "/docs/api" },
            { label: "Open OpenAPI JSON", href: contractUrl, external: true },
            sourceGuide("Contract versioning and exports", "docs/api-contracts.md"),
          ],
        },
        {
          id: "api-conventions", title: "API conventions",
          paragraphs: [
            "Use a scoped project bearer key for application requests or the session cookie for authorized account operations. Credentials bind the allowed scope. API and worker replicas can change while the durable request ID and home cell ownership stay the same.",
            "Discover the configured model and workload first. A scoped quote records eligible offers, estimates, adjustments, and expiry. Native submission uses the quoted input and an `Idempotency-Key`. A new admission returns `202`. An identical replay returns `200` with the original receipt. Read `Location` for status and wait for a terminal outcome before downloading artifacts.",
            "Parse error codes before retrying. Native gateway errors contain `error.code` and `error.message`. A `429 submission_rate_limit` includes a delay in seconds in `Retry-After`. Retry with the same intent and key. Compatible text endpoints and MCP have their own wire envelopes. Read each operation’s money units and cursor rules.",
          ],
          links: [
            { label: "Scoped quote schema", href: apiReferencePath("POST", "/v1/quotes/scoped") },
            { label: "Submit request schema", href: apiReferencePath("POST", "/v1/requests") },
            { label: "Request status schema", href: apiReferencePath("GET", "/v1/requests/{id}") },
            { label: "Webhook management schemas", href: apiReferencePath("GET", "/v1/projects/{project}/webhooks") },
            { label: "Organization submission limits", href: apiReferencePath("GET", "/v1/rate-limits") },
          ],
        },
      ],
    },
    {
      id: "provider-catalog", title: "Provider catalog endpoint", group: "Reference", topics: [
        {
          id: "artcraft-reference-catalog", title: "Read the ArtCraft reference catalog",
          paragraphs: [
            "The ArtCraft reference catalog is a versioned JSON endpoint for supported provider plugins, model capabilities, and supplier billing terms.",
            "For a self-hosted gateway, import the presets you want with your own plugin bindings and routing rates. Add custom models directly to your gateway catalog.",
          ],
          keywords: ["supplier rates", "list prices", "provider plugins", "self-hosted catalog", "custom models"],
          links: [
            { label: "Open catalog JSON", href: "/catalog/artcraft-v1.json", external: true },
            { label: "Curate a self-hosted catalog", href: "https://github.com/storytold/tokencraft/blob/main/docs/provider-connectors.md#prepare-offers-from-the-artcraft-catalog", external: true },
          ],
        },
      ],
    },
    {
      id: "optional-estimates", title: "Optional cost inspection", group: "Advanced", topics: [
        { id: "legacy-cost-endpoints", title: "Estimate operations", paragraphs: [
          "Public quotes estimate the active public catalog. Scoped quotes apply the caller’s project policy and carry an expiry. A media dry run quotes that primitive using its own request fields.",
        ], endpoints: [
          ["POST", "/v1/quotes", "Compare eligible public offers for a workload."],
          ["POST", "/v1/quotes/scoped", "Compare offers using an authorized project’s policy."],
          ["POST", "/v1/catalog/quotes/scoped", "Quote in the model-oriented response format."],
        ] },
      ],
    },
    {
      id: "project-routing-policy", title: "Project routing policy", group: "Advanced", topics: [
        {
          id: "policy-precedence", title: "How policy precedence works",
          paragraphs: [
            "Set project policy in Projects → Settings → Routing policy. Use Organization → Routing policy for defaults and limits across the organization. Valid changes save automatically as a revision in the selected scope. A routing preview uses the current form values to show how the gateway resolves settings for a sample request.",
            "The gateway resolves each preference independently. It uses the first configured value in this order — request, project, organization, then gateway defaults. A blank project preference inherits the organization value. Request defaults cover image and video aspect-ratio tolerance and a maximum estimated cost per request.",
            "Hard limits tighten the result. When both scopes cap a value, the lower cap wins. An offer-specific ratio limit can tighten an eligible catalog offer further. For example, if a request allows 15% ratio error, the organization caps it at 10%, and the project caps it at 5%, the effective tolerance is at most 5%. Enter zero for an exact aspect ratio or a zero-dollar quote cap.",
          ],
          keywords: ["routing policy", "project scope", "organization scope", "precedence", "inheritance", "hard limits", "aspect ratio", "offer-specific", "cost cap"],
        },
        {
          id: "policy-provider-routing", title: "Provider access, priority, and ranking",
          paragraphs: [
            "A provider must be allowed by every configured allowlist. Turn on Restrict providers and leave the selections empty to pause provider routing in that scope. An unset list adds no restriction.",
            "For an explicit model, provider priority orders eligible offers from preferred providers first. It follows request, project, then organization order. Selecting Lowest estimated cost for provider priority clears inherited provider ordering. Route ranking orders eligible offers within priority groups using estimated price, recent reliability, or settled cost per completion. A project ranking inherits the organization setting when unset.",
          ],
          keywords: ["provider allowlist", "preferred providers", "provider priority", "route ranking", "cost per completion"],
        },
        {
          id: "policy-automatic-models", title: "Automatic model selection",
          paragraphs: [
            "Rating and usage minimums apply when the request uses `model: auto` or an advised selection. Each minimum follows request, project, then organization precedence. For example, a project value of 6 takes priority over an organization value of 8 when the request leaves the rating unset. A supplied request value of 4 takes priority over both.",
            "A scoped media request can set `model: advise` to select a concrete model using the gateway’s catalog, project policy, and scoped ranking. A project can inherit the organization’s Model Advisor setting or set its own to apply this behavior when callers omit the model.",
            "A rating minimum requires at least 10 community ratings. Enter 6 for the equivalent of 3 out of 5 stars. Usage counts organizations with a successful request for that model in the past 30 days.",
          ],
          keywords: ["automatic model selection", "model auto", "min_quality", "ratings", "min_usage_accounts"],
        },
        {
          id: "policy-data-handling", title: "Provider regions and retention",
          paragraphs: [
            "Eligible offers must declare processing regions and retention when the corresponding restrictions apply. The gateway intersects allowed regions from the request, project, and organization and uses the shortest configured retention maximum. Enter catalog region IDs separated by commas. A retention limit of zero requires declared zero retention.",
          ],
          keywords: ["region allowlist", "data handling", "data residency", "retention"],
        },
        {
          id: "policy-spending-capacity", title: "Budgets and active request limits",
          paragraphs: [
            "A monthly budget caps the combined estimates of accepted requests for a UTC calendar month. New requests must fit both configured budgets. An organization budget is shared by its projects. Per-client spending adds a project budget for a matching client reference.",
            "Active request limits cover queued, running, and unconfirmed requests. The overall and per-provider caps for the project and organization apply together. Enter zero to pause new admissions under that cap. Leave a budget or capacity field blank to apply no value at that scope.",
          ],
          keywords: ["monthly budget", "client budget", "spending cap", "active request limit", "admission capacity", "UTC"],
        },
      ],
    },
    ...(demo ? fixtureSections() : []),
    ...(demo ? [{ id: "request-endpoints", title: "Local provider operations", group: "Advanced", topics: fixtureReference }] : []),
    {
      id: "operator-guides", title: "Operator guides", group: "Operate", topics: [
        {
          id: "self-hosting", title: "Self-host and deploy", paragraphs: [],
          links: [
            sourceGuide("Install and run TokenCraft", "docs/setup.md"),
            sourceGuide("Deployment, migration, and rollout", "docs/deployment.md"),
            sourceGuide("Cell autoscaling and rate limits", "docs/capacity.md"),
          ],
        },
        {
          id: "provider-guides", title: "Providers and compute", paragraphs: [],
          links: [
            sourceGuide("Provider plugins and compute setup", "plugins/providers/README.md"),
            sourceGuide("Configure inference and model catalogs", "docs/provider-connectors.md"),
            sourceGuide("Map primitives to provider APIs", "docs/provider-mapping.md"),
          ],
        },
        {
          id: "data-guides", title: "Diagnostics and data", paragraphs: [],
          links: [
            sourceGuide("Request diagnostics and traces", "docs/request-diagnostics.md"),
            sourceGuide("Export project data", "docs/customer-data.md"),
            sourceGuide("Archive and restore events", "docs/event-archives.md"),
            sourceGuide("Retain and retrieve request content", "docs/chat-history.md"),
          ],
        },
        {
          id: "implementation-guides", title: "Architecture and SDKs", paragraphs: [],
          links: [
            { label: "Interactive backend design", href: "/design/", external: true },
            sourceGuide("API schema walkthrough", "docs/api-schema.md"),
            sourceGuide("Rust SDK", "sdk/rust/README.md"),
            sourceGuide("TypeScript SDK", "sdk/typescript/README.md"),
          ],
        },
      ],
    },
  ];

  return { baseUrl, sections };
}

function fixtureSections(): DocsSection[] {
  return [
    {
      id: "local-quickstart", title: "Local routing diagnostics", group: "Advanced", topics: [
        {
          id: "quote-request", title: "Quote a workload",
          paragraphs: [
            "The local provider profile exposes offer eligibility and estimates in integer nano-USD. Public estimates use fixture rates. `POST /v1/quotes/scoped` applies private routing policy for the caller’s project.",
            "The following example uses the default in-process mock catalog. Run the connection check first to set the gateway base URL.",
          ],
          keywords: ["quote", "routing", "scoped quote", "eligibility", "simulated", "price estimate", "nano usd", "cost ceiling", "aspect ratio", "tolerance"],
          example: { label: "Shell", lang: "bash", code: [
            'curl -fsS "$TOKENCRAFT_BASE_URL/v1/quotes" \\',
            "  -H 'Content-Type: application/json' \\",
            "  -d '{",
            '    "model": "studio_image_demo",',
            '    "workload": {"kind": "image", "count": 1, "aspect_ratio": "7:5"}',
            "  }'",
          ].join("\n") },
          links: [{ label: "Quote endpoints", href: "#quote-reference" }],
        },
        {
          id: "submit-request", title: "Submit and follow a request",
          paragraphs: [
            "For prompt-based local image and video testing, `POST /v1/model-requests` selects a connected offer during admission. Send the workload `input`, its `image_prompt` or `video_prompt`, a scoped bearer key, and an `Idempotency-Key`. You can still pass a `quote_id` to pin a prior estimate.",
            "The first submission returns `202` and a request ID. The same key and body recover the original ID. Changed input with that key returns `409`. Persist the key and input before sending, then poll the `Location` URL with the same scope.",
            "For repeat polls, send the prior `ETag` in `If-None-Match`. A `304` means the status view is unchanged. Wait the suggested `Retry-After` seconds while the request is queued or running.",
            "Use `GET /v1/requests/{id}/events` for the timeline and `GET /v1/requests/{id}/result` for stored output while it is available. If the provider outcome is unconfirmed, inspect the existing request before starting another.",
          ],
          keywords: ["idempotency", "retry", "replay", "409", "202", "submit", "request id", "poll", "result", "awaiting reconciliation"],
          links: [{ label: "Request lifecycle endpoints", href: "#request-reference" }, { label: "View requests", href: "/dashboard/requests" }],
        },
      ],
    },
    {
      id: "chat-completions", title: "Chat Completions", group: "Build", topics: [
        {
          id: "send-chat", title: "Send a text completion",
          paragraphs: [
            "Set TOKENCRAFT_REQUEST_KEY to a unique value for this request and keep it for retries. With the local OpenRouter HTTP fixture loaded, use `POST /v1/chat/completions` and a project key with `requests:submit` and `requests:read`. Compatible clients use your gateway base URL with `/v1` appended.",
            "Find the durable request ID in `TokenCraft-Request-Id` or `x-request-id`. If the buffered wait expires, poll the URL from the `Location` header or replay with the same idempotency key.",
          ],
          keywords: ["chat", "openai", "openrouter", "llm", "text completion", "sdk", "timeout", "504", "chat endpoint"],
          example: { label: "Shell", lang: "bash", code: [
            'curl -i "$TOKENCRAFT_BASE_URL/v1/chat/completions" \\',
            '  -H "Authorization: Bearer $TOKENCRAFT_API_KEY" \\',
            '  -H "Idempotency-Key: $TOKENCRAFT_REQUEST_KEY" \\',
            "  -H 'Content-Type: application/json' \\",
            "  -d '{",
            '    "model": "studio_chat_wire_demo",',
            '    "messages": [{"role": "user", "content": "Hello, TokenCraft"}],',
            '    "max_completion_tokens": 128',
            "  }'",
          ].join("\n") },
          links: [{ label: "Request lifecycle endpoints", href: "#request-reference" }],
        },
        {
          id: "stream-chat", title: "Stream a text completion",
          paragraphs: [
            "Set `stream: true` to receive text as server-sent events. Set `stream_options.include_usage` to receive a final usage event when both token counts are available. A successful stream ends with `data: [DONE]`.",
            "A client disconnect leaves the provider attempt running. Replay the same request with the same idempotency key to read from the beginning, or poll the URL from `Location`. The gateway retains encrypted text for one hour after the attempt settles.",
          ],
          keywords: ["streaming", "sse", "server-sent events", "include_usage", "disconnect", "partial response", "stream replay"],
          example: { label: "Shell", lang: "bash", code: [
            'curl -iN "$TOKENCRAFT_BASE_URL/v1/chat/completions" \\',
            '  -H "Authorization: Bearer $TOKENCRAFT_API_KEY" \\',
            '  -H "Idempotency-Key: $TOKENCRAFT_STREAM_REQUEST_KEY" \\',
            "  -H 'Content-Type: application/json' \\",
            "  -d '{",
            '    "model": "studio_chat_wire_demo",',
            '    "messages": [{"role": "user", "content": "Hello, TokenCraft"}],',
            '    "max_completion_tokens": 128,',
            '    "stream": true,',
            '    "stream_options": {"include_usage": true}',
            "  }'",
          ].join("\n") },
        },
      ],
    },
    {
      id: "responses", title: "Responses", group: "Build", topics: [
        {
          id: "create-response", title: "Create a text response",
          paragraphs: [
            "Use `POST /v1/responses` with a chat-capable model alias and a project key with request submission and read permissions. Send `input` as a string or plain-text messages. Optional `instructions` become a system message.",
            "Set `stream: true` for named Responses events. Read text deltas from `response.output_text.delta` and token totals from the terminal response. The OpenAI Node SDK exposes the combined text as `output_text`.",
            "Set `TOKENCRAFT_REQUEST_KEY` to a new value for each request intent. Save `TokenCraft-Request-Id` and follow `Location` to the native trace. Replay the original body and key to recover text during the one-hour encrypted result window. Use `store: false` for this stateless API style.",
          ],
          keywords: ["responses api", "openai", "output_text", "text generation", "sse", "stream", "response.completed", "sdk", "request id"],
          example: { label: "Shell", lang: "bash", code: [
            'curl -iN "$TOKENCRAFT_BASE_URL/v1/responses" \\',
            '  -H "Authorization: Bearer $TOKENCRAFT_API_KEY" \\',
            '  -H "Idempotency-Key: $TOKENCRAFT_REQUEST_KEY" \\',
            "  -H 'Content-Type: application/json' \\",
            "  -d '{",
            '    "model": "studio_chat_wire_demo",',
            '    "input": "Explain idempotency in one sentence.",',
            '    "max_output_tokens": 128, "stream": true, "store": false',
            "  }'",
          ].join("\n") },
          links: [{ label: "Request lifecycle endpoints", href: "#request-reference" }],
        },
      ],
    },
    {
      id: "webhooks", title: "Webhooks", group: "Build", topics: [
        {
          id: "terminal-webhooks", title: "Receive terminal events",
          paragraphs: [
            "With the local webhook sidecar enabled, register a subscription under Projects and save the signing secret. Attach the subscription ID when submitting a quoted request.",
            "Verify the signature and timestamp against the original request bytes. Deduplicate using the `webhook-id` header. Terminal events cover completion, cancellation, and confirmed failure. The status URL remains available for polling.",
          ],
          keywords: ["webhook", "callback", "signature", "signing", "subscription", "terminal event", "notification", "deduplicate"],
          example: { label: "JSON field", lang: "json", code: '{\n  "delivery": { "webhook_subscription_id": "whs_…" }\n}' },
          links: [{ label: "Manage webhooks", href: "/dashboard/projects" }, { label: "Project and webhook endpoints", href: "#project-reference" }],
        },
      ],
    },
  ];
}

const fixtureReference: Topic[] = [
  {
    id: "quote-reference", title: "Catalog, quotes, and policy", paragraphs: [], endpoints: [
      ["GET", "/v1/catalog/models", "List configured model IDs and supported workloads."],
      ["GET", "/v1/catalog/offers", "List fixture offers and the catalog revision."],
      ["POST", "/v1/quotes", "Estimate public costs and eligibility without private policy."],
      ["POST", "/v1/quotes/scoped", "Quote with inherited project defaults and effective limits."],
      ["GET", "/v1/routing-policy", "Read the current organization or project policy revision as an administrator."],
      ["PUT", "/v1/routing-policy", "Save a policy revision with expected_revision."],
      ["POST", "/v1/routing-policy/preview", "Evaluate a draft without saving settings or starting work."],
    ],
  },
  {
    id: "request-reference", title: "Request lifecycle", paragraphs: [
      "Set `app_ref` on submission to identify the calling app. Combine `client_ref`, `app_ref`, `model`, `provider`, `status`, or `cost_state` when listing requests. For a bounded date window, pass both `from` and `to` as UTC dates. The start is inclusive and the end is exclusive.",
      "Pass the returned `next_after` as `after` with the same filters to read the next page.",
    ],
    keywords: ["request history", "app attribution", "status filters", "client filtering", "cost state", "utc dates", "pagination"],
    endpoints: [
      ["POST", "/v1/requests", "Submit quoted input with a quote ID and Idempotency-Key."],
      ["POST", "/v1/model-requests", "Submit to the local provider profile. Prompt-based media can route during admission."],
      ["GET", "/v1/requests", "List scoped requests with combined history filters."],
      ["GET", "/v1/requests/{id}", "Read scoped status, a quote, the provider attempt, and reported cost."],
      ["GET", "/v1/requests/{id}/events", "Read the ordered request timeline."],
      ["GET", "/v1/requests/{id}/trace", "Inspect scoped timing, routing, cost, and delivery evidence."],
      ["POST", "/v1/requests/{id}/route-preview", "Compare a saved workload with current offers and policy. Send {} to preserve its request settings."],
      ["GET", "/v1/requests/{id}/result", "Read stored result data before expiry with the same request scope."],
      ["GET", "/v1/requests/{id}/artifacts/{slot}", "Download a scoped stored artifact. Slots start at zero."],
      ["POST", "/v1/requests/{id}/cancel", "Cancel a queued request or running mock request with the JSON body {}."],
      ["POST", "/v1/chat/completions", "Run a buffered or streamed text completion with a chat-capable offer.", "/docs/primitives/text"],
      ["POST", "/v1/responses", "Create a stateless text response as JSON or named SSE events.", "#create-response"],
    ],
  },
  {
    id: "usage-reference", title: "Usage, exports, and archives", paragraphs: [], endpoints: [
      ["GET", "/v1/usage", "Group totals by provider, model, client, app, or UTC day. Use app_ref to filter."],
      ["POST", "/v1/exports", "Create a fixed project export. Filter by client reference or resume from an event ID."],
      ["GET", "/v1/exports/{id}/data", "Download a versioned NDJSON page and follow its next-event cursor."],
      ["POST", "/v1/archives", "Seal an export partition in configured storage as a project administrator."],
      ["POST", "/v1/archives/{id}/restore", "Queue integrity verification and a temporary download window."],
      ["GET", "/v1/archives/{id}/data", "Download verified NDJSON during the restore window."],
    ],
  },
  {
    id: "project-reference", title: "Projects and deliveries", paragraphs: [], endpoints: [
      ["GET", "/v1/projects", "List projects available to your signed-in account."],
      ["GET", "/v1/projects/{project}/webhooks", "List subscriptions as a project administrator with webhooks enabled."],
      ["GET", "/v1/projects/{project}/webhook-deliveries", "Inspect terminal-event deliveries and retry progress."],
    ],
  },
];

export type Docs = ReturnType<typeof buildDocs>;

export function docsMarkdown(docs: Docs, pageUrl: URL) {
  const link = ({ label, href }: DocsLink) => `[${label}](${new URL(href, pageUrl)})`;
  const lines = [
    `# ${docsTitle}`, "", docsDescription, "",
    `Docs: ${pageUrl.href}`, `Gateway base URL: ${docs.baseUrl}`, "",
    "## Agent setup task", "",
    "Help me connect this project to TokenCraft using the guide below. Read TOKENCRAFT_API_KEY from my environment or secret manager. If it is missing, ask me to set it. Keep the key out of chat, code, and logs. Send it only to the gateway above. Start with the read-only connection check and report the HTTP status.", "",
    "## Table of contents", "",
    ...docs.sections.map(({ title, id }) => `- ${link({ label: title, href: `#${id}` })}`), "",
  ];
  for (const section of docs.sections) {
    lines.push(`## ${link({ label: section.title, href: `#${section.id}` })}`, "");
    for (const topic of section.topics) {
      lines.push(`### ${link({ label: topic.title, href: `#${topic.id}` })}`, "");
      for (const paragraph of topic.paragraphs) lines.push(paragraph, "");
      if (topic.example) lines.push(`\`\`\`${topic.example.lang}`, topic.example.code, "\`\`\`", "");
      for (const [method, path, summary, doc] of topic.endpoints ?? []) {
        lines.push(`- ${link({ label: `\`${method} ${path}\``, href: doc ?? `#${endpointId(method, path)}` })} — ${summary}`);
      }
      if (topic.endpoints) lines.push("");
      for (const item of topic.links ?? []) lines.push(`- ${link(item)}`);
      if (topic.links) lines.push("");
    }
  }
  return lines.join("\n").trimEnd();
}
