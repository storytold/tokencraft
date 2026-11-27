// Handler-backed additions for routes omitted from contracts/openapi.json.
// Keep overlapping schemas in OpenAPI and add source-derived fields only where needed.
import type { Content, HttpMethod, Parameter, ResponseSchema, Schema, SupplementalOperation } from "./api-schema-types";

const string = { type: "string" };
const integer = { type: "integer" };
const boolean = { type: "boolean" };
const timestamp = { type: "string", format: "date-time" };
const int64 = { type: "integer", format: "int64" };
const usdNanos = { type: "string", description: "Integer nano-USD encoded as a decimal string." };
const array = (items: Schema): Schema => ({ type: "array", items });
const map = (value: Schema): Schema => ({ type: "object", additionalProperties: value });
const nullable = (schema: Schema): Schema => ({ anyOf: [schema, { type: "null" }] });
const union = (...oneOf: Schema[]): Schema => ({ oneOf });
const choice = (...values: string[]): Schema => ({ type: "string", enum: values });
const literal = (value: string | number): Schema => ({ type: typeof value === "number" ? "integer" : "string", const: value });
const ref = (name: string): Schema => ({ $ref: "#/components/schemas/" + name });
const object = (properties: Record<string, Schema>, optional: string[] = []): Schema => ({
  type: "object", properties, required: Object.keys(properties).filter((name) => !optional.includes(name)),
});
const described = (schema: Schema, description: string): Schema => ({ ...schema, description });
const json = (schema: Schema): Content => ({ "application/json": { schema } });
const form = (schema: Schema): Content => ({ "application/x-www-form-urlencoded": { schema } });
const query = (name: string, schema: Schema = string, required = false, description?: string): Parameter => ({
  in: "query", name, schema, required, description,
});
const header = (name: string, description: string, required = false): Parameter => ({
  in: "header", name, schema: string, description, required,
});
const reply = (description: string, schema?: Schema): ResponseSchema => ({ description, content: schema ? json(schema) : undefined });
const error = (description: string) => reply(description, ref("GatewayError"));

type Details = {
  auth?: string;
  params?: Parameter[];
  body?: Content;
  responses: Record<string, ResponseSchema>;
  description?: string;
};

function route(method: HttpMethod, path: string, handler: string, summary: string, details: Details): SupplementalOperation {
  const pathParams = Array.from(path.matchAll(/\{([^}]+)\}/g), ([, name]): Parameter => ({
    name, in: "path", required: true, schema: string,
  }));
  return {
    method, path, source: "crates/bin/api-gateway/src/" + handler,
    auth: details.auth,
    operation: {
      summary, description: details.description,
      parameters: [...pathParams, ...(details.params ?? [])],
      requestBody: details.body ? { content: details.body, required: true } : undefined,
      responses: details.responses,
    },
  };
}

const gateway = (name: string) => "gateway_demo/" + name + ".rs";
const session = "Signed-in session cookie.";
const admin = "Signed-in session cookie with organization owner or administrator access.";
const account = query("account_id", string, true, "Organization ID from an authorized membership.");
const project = query("project_id", string, false, "Select the project layer. Omit it for the organization layer where supported.");
const scope = [
  query("account_id", string, false, "Required for session accounts. Bearer credentials carry their account."),
  query("project_id", string, false, "Bearer credentials are scope-bound. Session callers can select their project."),
];
const scoped = "Project bearer key or an authorized account credential or session.";
const exportsAuth = scoped + " Project keys require exports:read.";
const outOfScope = error("Resource absent or outside the caller's authorized scope.");
const exportBusy = {
  ...error("The API replica is busy serving exports. Retry with the same cursor."),
  headers: { "Retry-After": { schema: integer, description: "Seconds to wait before retrying." } },
};
const invalid = error("Invalid input or selection.");
const revisionConflict = error("Revision is stale. Reload the current policy or resource before retrying.");
const browserOrigin = header("Origin", "The configured browser origin.", true);

const projectView = object({
  id: string, name: string, is_default: boolean, role: string, client_ref: nullable(string),
});
const grantView = object({ user_id: string, role: string, client_ref: nullable(string) });
const scopes = array(choice("requests:read", "requests:submit", "requests:cancel", "usage:read", "exports:read"));
const principalView = object({
  id: string, name: string, scopes, client_ref: nullable(string), revoked_at: nullable(timestamp),
});
const keyMetadata = object({
  id: string, principal_id: string, key_prefix: string, expires_at: timestamp, revoked_at: nullable(timestamp),
});
const issuedKey = object({
  id: string, principal_id: string, key_prefix: string, expires_at: timestamp,
  token: described(string, "Full bearer secret. Returned on issuance only."),
});
const review = object({
  rating: { ...integer, minimum: 1, maximum: 10 },
  review_text: described(nullable(string), "Plain text up to 1200 UTF-8 bytes."),
}, ["review_text"]);
const settlement = object({
  revision: integer, amount_usd_nanos: usdNanos, delta_usd_nanos: usdNanos, evidence_ref: string, recorded_at: timestamp,
});

const projectOperations = [
  route("get", "/v1/projects", gateway("projects"), "List projects you can access", {
    auth: session, params: [account], responses: { 200: reply("Projects, with your effective role.", array(projectView)) },
  }),
  route("post", "/v1/projects", gateway("projects"), "Create a project", {
    auth: admin, params: [account], body: json(object({ name: string })),
    responses: { 201: reply("Project created.", projectView), 400: invalid },
  }),
  route("get", "/v1/projects/{project}/grants", gateway("projects"), "List project grants", {
    auth: admin, params: [account], responses: { 200: reply("Grants to users in the selected organization.", array(grantView)), 404: outOfScope },
  }),
  route("put", "/v1/projects/{project}/grants/{user}", gateway("projects"), "Set a project grant", {
    auth: admin, params: [account],
    body: json(object({
      role: choice("developer", "billing_viewer", "client_viewer"),
      client_ref: described(nullable(string), "Required for client_viewer."),
    }, ["client_ref"])),
    responses: { 204: reply("Grant saved."), 400: invalid, 404: outOfScope },
  }),
  route("delete", "/v1/projects/{project}/grants/{user}", gateway("projects"), "Revoke a project grant", {
    auth: admin, params: [account], responses: { 204: reply("Grant removed."), 404: outOfScope },
  }),
  route("get", "/v1/projects/{project}/credentials", gateway("project_keys"), "Read project credential metadata", {
    auth: admin, params: [account],
    responses: { 200: reply("Credential inventory.", object({
      attached_principal_id: nullable(string), principals: array(principalView), keys: array(keyMetadata),
    })), 404: outOfScope },
  }),
  route("put", "/v1/projects/{project}/credentials/attached", gateway("project_keys"), "Choose the attached project principal", {
    auth: admin, params: [account], body: json(object({ principal_id: string })),
    responses: { 204: reply("Active principal attached."), 404: outOfScope },
  }),
  route("post", "/v1/projects/{project}/service-principals", gateway("project_keys"), "Create a service principal and issue a key", {
    auth: admin, params: [account],
    body: json(object({
      name: string, scopes, client_ref: nullable(string), expires_in_days: nullable(integer),
      attach: nullable(boolean), bootstrap_default: { ...boolean, default: false },
    }, ["client_ref", "expires_in_days", "attach", "bootstrap_default"])),
    responses: { 201: reply("Principal and first key created.", issuedKey), 400: invalid, 404: outOfScope },
  }),
  route("post", "/v1/projects/{project}/service-principals/{principal}/keys", gateway("project_keys"), "Issue another key for the principal", {
    auth: admin, params: [account], body: json(object({ expires_in_days: nullable(integer) }, ["expires_in_days"])),
    responses: { 201: reply("Key issued. Revoke old keys separately after rotation.", issuedKey), 404: outOfScope },
  }),
  route("delete", "/v1/projects/{project}/service-principals/{principal}", gateway("project_keys"), "Revoke a project principal", {
    auth: admin, params: [account], responses: { 204: reply("Principal and its access revoked."), 404: outOfScope },
  }),
  route("delete", "/v1/projects/{project}/keys/{key}", gateway("project_keys"), "Revoke a project key", {
    auth: admin, params: [account], responses: { 204: reply("Key revoked."), 404: outOfScope },
  }),
  route("get", "/v1/projects/{project}/requests/{request}/settlements", gateway("settlements"), "Read supplier settlement history", {
    auth: admin, params: [account],
    responses: { 200: reply("Eligibility, estimate and append-only settlement entries.", object({
      request_id: string, eligible: boolean, provider: string, estimated_usd_nanos: usdNanos, entries: array(settlement),
    })), 404: outOfScope },
  }),
  route("post", "/v1/projects/{project}/requests/{request}/settlements", gateway("settlements"), "Record a supplier settlement or correction", {
    auth: admin, params: [account, header("Idempotency-Key", "Reuse for the same settlement revision and amount.", true)],
    body: json(object({ expected_revision: integer, amount_usd_nanos: usdNanos, evidence_ref: string })),
    responses: { 200: reply("Identical replay.", settlement), 201: reply("Settlement recorded.", settlement), 409: revisionConflict },
  }),
  route("get", "/v1/requests/{id}/location", gateway("request_location"), "Find the project for a request", {
    auth: session,
    responses: { 200: reply("Authorized request ownership.", object({ request_id: string, account_id: string, project_id: string })), 404: outOfScope },
  }),
  route("get", "/v1/models/{slug}/reviews/me", "model_reviews.rs", "Read your review", {
    auth: session, responses: { 200: reply("Review or null if you have no review.", nullable(review)), 404: outOfScope },
  }),
  route("put", "/v1/models/{slug}/reviews/me", "model_reviews.rs", "Save your model review", {
    auth: session + " Your own session is required for changes.", body: json(review),
    responses: { 200: reply("Saved review with normalized text.", review), 400: invalid, 404: outOfScope },
  }),
  route("delete", "/v1/models/{slug}/reviews/me", "model_reviews.rs", "Delete your model review", {
    auth: session + " Your own session is required for changes.", responses: { 204: reply("Review absent.") },
  }),
];

const exportSnapshot = object({
  id: string, project_id: string, client_ref: nullable(string), after_event_id: nullable(string),
  pending_at_creation: boolean, created_at: timestamp, expires_at: timestamp,
  format: literal("ndjson"), schema_version: literal(1),
});
const eventLine = object({
  schema_version: literal(1), id: string, project_id: string, request_id: string, client_ref: nullable(string),
  type: string, occurred_at: timestamp, data: described({}, "Event-dependent JSON or null."),
});
const exportScope = { account_id: nullable(string), project_id: nullable(string) };
const archive = object({
  id: string, project_id: string, client_ref: nullable(string), export_id: string, state: string,
  schema_version: integer, event_count: nullable(integer), first_event_id: nullable(string),
  last_event_id: nullable(string), has_more: nullable(boolean), content_sha256: nullable(string),
  error_code: nullable(string), created_at: timestamp, stored_at: nullable(timestamp),
  restored_until: nullable(timestamp), restore_check_at: nullable(timestamp), automatic: boolean,
  automatic_retry_at: timestamp, retired_at: timestamp, retirement_error_code: string,
}, ["automatic", "automatic_retry_at", "retired_at", "retirement_error_code"]);
const ndjson: Content = { "application/x-ndjson": { schema: eventLine } };
const downloadHeaders = {
  "Content-Disposition": { schema: string, description: "Suggested download filename." },
  "X-TokenCraft-Content-SHA256": { schema: string, description: "SHA-256 of returned NDJSON bytes." },
};

const dataOperations = [
  route("get", "/v1/exports", gateway("exports"), "List event export snapshots", {
    auth: exportsAuth, params: [...scope, query("client_ref"), query("before", string, false, "Use next_before from the previous page.")],
    responses: { 200: reply("Up to 20 snapshot descriptors.", object({ exports: array(exportSnapshot), next_before: nullable(string) })) },
  }),
  route("post", "/v1/exports", gateway("exports"), "Create an event export snapshot", {
    auth: exportsAuth, body: json(object({ ...exportScope, client_ref: nullable(string), after: nullable(string) },
      ["account_id", "project_id", "client_ref", "after"])),
    responses: { 201: reply("Snapshot with a fixed high watermark.", exportSnapshot), 404: outOfScope, 503: exportBusy },
  }),
  route("get", "/v1/exports/{id}", gateway("exports"), "Get an export snapshot descriptor", {
    auth: exportsAuth, params: scope, responses: { 200: reply("Snapshot descriptor.", exportSnapshot), 404: outOfScope },
  }),
  route("get", "/v1/exports/{id}/data", gateway("exports"), "Download events from a snapshot", {
    auth: exportsAuth, params: [...scope, query("after", string, false, "Continue after a returned event ID."), query("limit", integer)],
    responses: {
      200: {
        description: "NDJSON. The schema below describes each line independently.", content: ndjson,
        headers: {
          ...downloadHeaders,
          "X-TokenCraft-Has-More": { schema: boolean, description: "Fetch another page when true." },
          "X-TokenCraft-Next-After": { schema: string, description: "Present when at least one event is returned." },
        },
      },
      404: outOfScope,
      503: exportBusy,
    },
  }),
  route("get", "/v1/archives", gateway("archives"), "List event archives", {
    auth: exportsAuth, params: [...scope, query("before", string, false, "Use next_before from the previous page.")],
    responses: { 200: reply("Archive page. Requires configured archive storage.", object({
      archives: array({ allOf: [archive, object({ can_request_retrieval: boolean })] }),
      next_before: nullable(string),
    })) },
  }),
  route("post", "/v1/archives", gateway("archives"), "Queue archival of an export", {
    auth: admin,
    body: json(object({ ...exportScope, export_id: string, after: nullable(string) }, ["account_id", "project_id", "after"])),
    responses: { 202: reply("Archive queued for storage.", archive), 404: outOfScope },
  }),
  route("get", "/v1/archives/{id}", gateway("archives"), "Get an archive manifest", {
    auth: exportsAuth, params: scope, responses: { 200: reply("Archive state and integrity metadata.", archive), 404: outOfScope },
  }),
  route("post", "/v1/archives/{id}/restore", gateway("archives"), "Queue an archive restore", {
    auth: exportsAuth + " Cold retrieval additionally requires a project administrator session.",
    params: [...scope, query("retrieve", { type: "boolean", default: false }),
      header("Content-Type", "Use application/json when retrieve=true, even with an empty body.")],
    responses: { 202: reply("RESTORE queued or already in progress.", archive), 404: outOfScope, 409: error("Archive is not ready for restoration.") },
  }),
  route("get", "/v1/archives/{id}/data", gateway("archives"), "Download a restored archive", {
    auth: exportsAuth, params: scope,
    responses: {
      200: { description: "Integrity-checked NDJSON. Schema applies to each line.", content: ndjson, headers: downloadHeaders },
      404: outOfScope, 409: error("Restore the archive before downloading."),
      503: { ...exportBusy, description: "Both archive download slots are busy. Retry the same archive during its restore window." },
    },
  }),
];

const routingScope = [account, project];
const requiredProject = { ...project, required: true };
const revision = { ...integer, minimum: 0 };
const bounds = object({
  max_relative_error_bps: nullable(integer), video_max_relative_error_bps: nullable(integer), max_total_usd_nanos: nullable(int64),
}, ["max_relative_error_bps", "video_max_relative_error_bps", "max_total_usd_nanos"]);
const policy = object({
  defaults: bounds, limits: bounds, offer_overrides: map(object({ max_relative_error_bps: integer })),
  allowed_providers: nullable(array(string)), preferred_providers: nullable(array(string)),
  route_ranking: nullable(choice("price", "recent_reliability", "cost_per_completion")),
  min_quality: nullable(integer), advisor_enabled: nullable(boolean), min_usage_accounts: nullable(integer),
  monthly_estimated_budget_usd_nanos: nullable(int64),
  active_request_limits: object({ total: nullable(integer), by_provider: map(integer) }, ["total", "by_provider"]),
  data_requirements: nullable(ref("DataRequirements")),
  excluded_connections: array(string), compute_order: map(array(string)), connection_projects: map(nullable(array(string))),
}, ["defaults", "limits", "offer_overrides", "allowed_providers", "preferred_providers", "route_ranking", "min_quality",
  "advisor_enabled", "min_usage_accounts", "monthly_estimated_budget_usd_nanos", "active_request_limits", "data_requirements",
  "excluded_connections", "compute_order", "connection_projects"]);
const policyView = object({
  revision, policy,
  monthly_budget: described(object({ reserved_usd_nanos: usdNanos, limit_usd_nanos: nullable(int64), resets_at: timestamp }),
    "Present when the selected layer has a configured monthly estimated budget."),
  active_requests: described(object({ total: int64, by_provider: map(int64) }),
    "Present when the selected layer has active-request limits."),
}, ["monthly_budget", "active_requests"]);
const accessChange = union(
  object({ kind: literal("share"), projects: described(nullable(array(string)), "Null shares with all projects.") }, ["projects"]),
  object({ kind: literal("exclude"), excluded: boolean }),
);
const location = object({ cloud: string, region: string });
const targetDraft = object({
  id: string, location, zone: string, priority: { ...integer, minimum: 0, maximum: 255 },
  endpoint: string, key_env: nullable(string), retry_on_rate_limit: { ...boolean, default: false }, organization_id: string,
}, ["key_env", "retry_on_rate_limit", "organization_id"]);
const targetView = object({ id: string, cloud: string, region: string, zone: string, priority: integer });
const computePool = object({ binding: string, cell: string, models: array(string), targets: array(targetView), order: nullable(array(string)) });
const binding = union(
  object({ kind: literal("connector"), name: string, origin: string, revision: string, token_env: string }),
  object({ kind: literal("chat"), name: string, endpoint: string, key_env: nullable(string) }, ["key_env"]),
);
const offerDraft = object({ id: string, model: string, upstream_model: string, pricing: ref("Pricing") });
const providerDraft = object({
  base_revision: string, projects: nullable(array(string)), change: union(
    object({ kind: literal("attach"), name: string, provider_id: nullable(string), binding, models: array(offerDraft) }, ["provider_id"]),
    object({ kind: literal("add_models"), binding_name: string, models: array(offerDraft) }),
    object({ kind: literal("update_price"), offer_id: string, primary_usd_nanos: int64, output_usd_nanos: nullable(int64) }, ["output_usd_nanos"]),
  ),
}, ["projects"]);
const providerInventory = object({
  base_revision: string, active_revision: string, catalog_revision: string, policy_revision: revision,
  discovery: boolean, provider_key_management: boolean, credential_bindings: array(string),
  offers: array(object({
    id: string, provider: string, model: string, adapter: ref("Adapter"), binding: nullable(string),
    pricing: ref("Pricing"), reviewed_pricing: ref("Pricing"), allowed_by_policy: boolean, connection_id: string,
    selected_projects: nullable(array(string)), available_in_project: boolean, excluded_in_project: boolean,
  })),
});
const providerPreviewFields = {
  base_revision: string, base_catalog_revision: string, proposed_revision: string, provider_name: nullable(string),
  catalog: object({
    version: integer, offers: array(ref("Offer")), connection_access: map(object({ account: string, projects: nullable(array(string)) })),
    rate_feed_offers: array(string),
  }, ["connection_access", "rate_feed_offers"]),
  binding: nullable(binding), changed_offer_ids: array(string), rate_feed_active: boolean, catalog_patch: boolean,
};

const policyOperations = [
  route("get", "/v1/routing-policy", gateway("routing_settings"), "Read a routing policy layer", {
    auth: admin, params: routingScope, responses: { 200: reply("Policy and current revision.", policyView), 404: outOfScope },
  }),
  route("put", "/v1/routing-policy", gateway("routing_settings"), "Update routing policy", {
    auth: admin, params: routingScope, body: json(object({ expected_revision: revision, policy })),
    description: "Updates the selected layer. Sharing, exclusions, and compute target order use their own endpoints.",
    responses: { 200: reply("Saved policy and revision.", policyView), 400: invalid, 409: revisionConflict },
  }),
  route("post", "/v1/routing-policy/preview", gateway("routing_settings"), "Preview a routing policy change", {
    auth: admin, params: [account, requiredProject],
    body: json(object({ layer: choice("organization", "project"), policy, input: ref("GatewayQuoteInput") })),
    responses: { 200: reply("Routing comparison using the draft policy.", ref("GatewayQuote")), 400: invalid },
  }),
  route("get", "/v1/routing-policy/history", gateway("provider_access"), "List recent routing policy revisions", {
    auth: admin, params: routingScope, responses: { 200: reply("Up to 20 revisions, newest first.", array(object({
      revision, created_at: timestamp, created_by: string, policy,
    }))) },
  }),
  route("put", "/v1/provider-access", gateway("provider_access"), "Change provider sharing or project exclusions", {
    auth: admin, params: routingScope,
    body: json(object({ connection_id: string, expected_revision: revision, change: accessChange })),
    description: "Use share on an organization scope and exclude on a project scope.",
    responses: { 200: reply("Current revision after the change.", object({ revision })), 400: invalid, 409: revisionConflict },
  }),
  route("get", "/v1/providers", gateway("provider_onboarding"), "List organization provider connections", {
    auth: admin, params: [account], responses: { 200: reply("Visible connections, catalog revisions, and reviewed prices.", providerInventory) },
  }),
  route("post", "/v1/providers", gateway("provider_onboarding"), "Preview an organization provider change", {
    auth: admin, params: [account], body: json(providerDraft),
    responses: { 200: reply("Reviewable catalog and binding changes.", object(providerPreviewFields)), 400: invalid, 409: revisionConflict },
  }),
  route("get", "/v1/projects/{project}/providers", gateway("provider_onboarding"), "List providers available in a project", {
    auth: admin, params: [account], responses: { 200: reply("Offers and their project visibility.", providerInventory), 404: outOfScope },
  }),
  route("post", "/v1/projects/{project}/providers", gateway("provider_onboarding"), "Preview a project provider change", {
    auth: admin, params: [account], body: json(providerDraft),
    responses: { 200: reply("Project-scoped catalog draft.", object({ project_id: string, ...providerPreviewFields })), 400: invalid, 409: revisionConflict },
  }),
  route("get", "/v1/compute-providers", gateway("compute_providers"), "List eligible compute pools and endpoints", {
    auth: admin, params: routingScope,
    responses: { 200: reply("Sanitized target placements and the project order, where configured.", object({ revision, pools: array(computePool) })) },
  }),
  route("put", "/v1/compute-providers", gateway("compute_providers"), "Set a project's compute fallback order", {
    auth: admin, params: [account, requiredProject],
    body: json(object({
      expected_revision: revision, binding: string,
      order: described(nullable({ ...array(string), uniqueItems: true, minItems: 1, maxItems: 4 }), "Omit or use null to clear the override."),
    }, ["order"])),
    responses: { 200: reply("New or unchanged revision as a JSON integer.", revision), 400: invalid, 404: outOfScope, 409: revisionConflict },
  }),
  route("post", "/v1/compute-providers/preview", gateway("compute_providers"), "Preview an organization compute target", {
    auth: admin, params: [account],
    body: json(object({ plugin: choice("aws", "google_cloud", "render", "custom"), binding: string, cell: string, target: targetDraft })),
    responses: { 200: reply("Normalized target patch for an operator to review.", object({
      version: literal(1), cell: string, binding: string, target: targetDraft,
    })), 400: invalid, 409: error("The binding or target conflicts with active configuration.") },
  }),
];

const modelRequest = object({
  id: string, created_at: timestamp, client_ref: nullable(string), app_ref: string, provider: string,
  cost_state: choice("estimated", "mock_reported", "provider_reported", "supplier_settled"),
  webhook_subscription_id: nullable(string),
  status: choice("queued", "running", "succeeded", "canceled", "failed", "awaiting_reconciliation"),
  can_cancel: boolean, dispatch_paused: boolean, recheck: ref("ProviderRecheck"), correlation: ref("TraceCorrelation"),
  client_budget: ref("AppliedClientBudget"), model: string, estimated_usd_nanos: nullable(int64),
  routing: nullable(ref("Resolution")), chat_options: ref("ChatOptions"), mock_reported_usd_nanos: nullable(int64),
  mock_result: nullable(ref("MockRequestResult")), supplier_settlement: nullable(ref("SupplierSettlement")), content_removed_at: timestamp,
  upstream: nullable(object({
    result_expires_at: nullable(timestamp), reported_usd_nanos: nullable(usdNanos), provider_charge: ref("ProviderChargeView"),
    input_tokens: nullable(integer), output_tokens: nullable(integer), finish_reason: nullable(string),
    queue_state: nullable(string), result_observed_at: timestamp,
  }, ["provider_charge", "result_observed_at"])),
}, ["app_ref", "dispatch_paused", "recheck", "correlation", "client_budget", "content_removed_at"]);

const oauthError = object({ error: string });
const oauthClient = {
  client_id: nullable(string), client_secret: nullable(string),
};
const oauthBody = (fields: Record<string, Schema>, required: string[]) => {
  const properties = { ...oauthClient, ...fields };
  return form(object(properties, Object.keys(properties).filter((key) => !required.includes(key))));
};
const oauthAuthentication = "Registered OAuth client. Client authentication follows the registered client type.";
const redirect = (description: string): ResponseSchema => ({
  description, headers: { Location: { schema: string, description: "Redirect destination." } },
});
const html = (description: string): ResponseSchema => ({ description, content: { "text/html": { schema: string } } });
const rpcId = union(int64, string);
const protocols = choice("2025-11-25", "2025-06-18", "2025-03-26");

const mcpOperations = [
  route("get", "/.well-known/oauth-protected-resource/mcp", "gateway_demo/mcp/oauth.rs", "Discover the MCP OAuth resource", {
    auth: "Public when MCP is enabled.",
    responses: { 200: reply("Protected resource metadata.", object({
      resource: string, authorization_servers: array(string), scopes_supported: array(string), bearer_methods_supported: array(literal("header")),
    })) },
  }),
  route("get", "/.well-known/oauth-authorization-server", "gateway_demo/mcp/oauth.rs", "Discover the MCP OAuth issuer", {
    auth: "Public when MCP is enabled.",
    responses: { 200: reply("Authorization server metadata.", object({
      issuer: string, authorization_endpoint: string, token_endpoint: string, revocation_endpoint: string,
      authorization_response_iss_parameter_supported: boolean, response_types_supported: array(literal("code")),
      grant_types_supported: array(choice("authorization_code", "refresh_token")),
      code_challenge_methods_supported: array(literal("S256")),
      token_endpoint_auth_methods_supported: array(choice("none", "client_secret_post", "client_secret_basic")),
      scopes_supported: array(string),
    })) },
  }),
  route("get", "/mcp/oauth/authorize", "gateway_demo/mcp/consent.rs", "Show the OAuth consent screen", {
    auth: "Browser session for consent. Guests are redirected to sign-in.",
    params: [query("response_type", literal("code"), true), query("client_id", string, true), query("redirect_uri", string, true),
      query("state"), query("resource"), query("scope"), query("code_challenge", string, true),
      query("code_challenge_method", literal("S256"), true)],
    responses: { 200: html("Browser consent form."), 303: redirect("Sign-in continuation."), 400: invalid },
  }),
  route("post", "/mcp/oauth/authorize", "gateway_demo/mcp/consent.rs", "Approve or deny agent access", {
    auth: session, params: [browserOrigin],
    body: form(object({
      request: described(string, "Consent form value generated by the authorize page."),
      project: described(string, "account_id/project_id. Required when approving."),
      decision: choice("allow", "deny"), submit: string, cancel: string, usage: string,
    }, ["project", "submit", "cancel", "usage"])),
    responses: { 303: redirect("Registered callback with code or error, iss, and state when supplied."), 400: invalid },
  }),
  route("post", "/mcp/oauth/token", "gateway_demo/mcp/oauth.rs", "Exchange a code or refresh token", {
    auth: oauthAuthentication,
    description: "Authorization codes require the PKCE verifier and original redirect URI. Refresh grants supply refresh_token and can narrow scope.",
    body: oauthBody({
      grant_type: choice("authorization_code", "refresh_token"), resource: nullable(string), code: nullable(string),
      code_verifier: nullable(string), redirect_uri: nullable(string), refresh_token: nullable(string), scope: nullable(string),
    }, ["grant_type"]),
    responses: {
      200: reply("Tokens scoped to one project.", object({
        access_token: string, refresh_token: string, token_type: literal("Bearer"), expires_in: literal(900), scope: string,
      })),
      400: reply("Invalid grant, resource, scope, or grant type.", oauthError),
      401: reply("Client authentication failed.", oauthError),
    },
  }),
  route("post", "/mcp/oauth/revoke", "gateway_demo/mcp/oauth.rs", "Revoke an OAuth token", {
    auth: oauthAuthentication, body: oauthBody({ token: string }, ["token"]),
    responses: { 200: reply("Revocation accepted."), 401: reply("Client authentication failed.", oauthError) },
  }),
  route("get", "/mcp/oauth/connections", "gateway_demo/mcp/consent.rs", "Review your connected OAuth agents", {
    auth: "Browser session for management. Guests are redirected to sign-in.",
    responses: { 200: html("Connections page."), 303: redirect("Sign-in continuation.") },
  }),
  route("post", "/mcp/oauth/connections", "gateway_demo/mcp/consent.rs", "Disconnect an OAuth agent", {
    auth: session, params: [browserOrigin], body: form(object({ id: string })),
    responses: { 303: redirect("Connections page after revocation."), 400: invalid },
  }),
  route("post", "/mcp", "gateway_demo/mcp/protocol.rs", "Call a TokenCraft MCP method", {
    auth: "OAuth access token issued for the MCP resource. Each tool checks its own permission.",
    params: [{ ...header("MCP-Protocol-Version", "Optional negotiated protocol version."), schema: protocols },
      header("Origin", "When present, must match the configured origin.")],
    description: "Single JSON-RPC 2.0 message. Use tools/list for the live tool names and their inputSchema. An omitted id denotes a notification.",
    body: json(object({
      jsonrpc: literal("2.0"), id: rpcId, method: string,
      params: union(
        object({ protocolVersion: string, capabilities: { type: "object" }, clientInfo: { type: "object" } }, ["capabilities", "clientInfo"]),
        object({ name: string, arguments: { type: "object" } }, ["arguments"]),
        { type: "null" },
      ),
    }, ["id", "params"])),
    responses: {
      200: reply("JSON-RPC result or protocol error.", union(
        object({ jsonrpc: literal("2.0"), id: rpcId, result: described({}, "Method-specific JSON.") }),
        object({ jsonrpc: literal("2.0"), id: nullable(rpcId), error: object({ code: integer, message: string }) }),
      )),
      202: reply("Notification accepted."),
      400: reply("Unsupported protocol version."),
      401: reply("OAuth authentication required."),
      403: reply("Origin or tool scope rejected."),
      415: reply("Expected application/json."),
    },
  }),
];

const capacityProbe = route("get", "/internal/capacity", gateway("capacity"), "Inspect one replica’s capacity signals", {
  auth: "Dedicated capacity bearer token. Connect directly to the private replica listener.",
  description: "Optional internal probe for the autoscaling collector. The cell’s public HAProxy configuration blocks this path. Readiness, SSE readers, worker utilization, and binding digests describe only the responding replica.",
  responses: {
    200: reply("Current replica signals. Counters for inactive roles are null.", object({
      role: choice("combined", "api", "worker"), ready: boolean,
      sse_readers: nullable(integer), sse_capacity: nullable(integer),
      provider_tasks: nullable(integer), provider_capacity: nullable(integer),
      local_bindings: array(string), connector_bindings: array(string),
    })),
    401: reply("A valid capacity token is required."),
  },
});

export const sourceOperations = [...projectOperations, ...dataOperations, ...policyOperations, ...mcpOperations, capacityProbe];
export { modelRequest };
