import { hasIdPrefix, isRequestId, object } from "./http.js";
import type { VerifyWebhookOptions, WebhookEvent, WorkflowWebhookEvent } from "./types.js";

export class WebhookVerificationError extends Error {
  constructor(readonly code: "headers" | "timestamp" | "secret" | "signature" | "payload", message: string) {
    super(message);
    this.name = "WebhookVerificationError";
  }
}

const eventTypes = new Set(["request.succeeded", "request.canceled", "request.failed"]);
const terminalStates = new Set(["succeeded", "failed", "canceled", "expired"]);
const bytes = new TextEncoder();
const utf8 = new TextDecoder("utf-8", { fatal: true });

export async function verifyWebhook(
  body: Uint8Array,
  headers: HeadersInit,
  signingSecrets: string | readonly string[],
  options: VerifyWebhookOptions = {},
): Promise<WebhookEvent> {
  const { event, type, data } = await verifyEnvelope(body, headers, signingSecrets, options);
  if (!eventTypes.has(type) || !isRequestId(data.request_id)
      || (data.client_ref != null && !isClientReference(data.client_ref))) {
    throw invalidPayload();
  }
  return event as unknown as WebhookEvent;
}

export async function verifyWorkflowWebhook(
  body: Uint8Array,
  headers: HeadersInit,
  signingSecrets: string | readonly string[],
  options: VerifyWebhookOptions = {},
): Promise<WorkflowWebhookEvent> {
  const { event, type, data } = await verifyEnvelope(body, headers, signingSecrets, options);
  if (!hasIdPrefix(data.run_id, "wfr_") || !hasIdPrefix(data.workflow_id, "wf_") || !hasIdPrefix(data.project_id, "proj_")
      || typeof data.workflow_version !== "number" || !Number.isSafeInteger(data.workflow_version) || data.workflow_version < 1
      || typeof data.status !== "string" || !terminalStates.has(data.status) || type !== `workflow.run.${data.status}`
      || (data.client_ref != null && !isClientReference(data.client_ref))
      || data.fetch_url !== `/v1/projects/${data.project_id}/workflow-endpoints/${data.workflow_id}/runs/${data.run_id}`) {
    throw invalidPayload();
  }
  return event as unknown as WorkflowWebhookEvent;
}

async function verifyEnvelope(
  body: Uint8Array, headers: HeadersInit, signingSecrets: string | readonly string[], options: VerifyWebhookOptions,
) {
  const incoming = new Headers(headers);
  const id = incoming.get("webhook-id");
  const timestamp = incoming.get("webhook-timestamp");
  const signature = incoming.get("webhook-signature");
  if (!id || !/^whd_[0-9a-hjkmnp-tv-z]{26}$/.test(id) || !timestamp || !/^[0-9]{1,12}$/.test(timestamp)
      || !signature || signature.length > 1024) {
    throw new WebhookVerificationError("headers", "A required webhook header is invalid.");
  }

  const now = options.now ?? Math.floor(Date.now() / 1000);
  const tolerance = options.toleranceSeconds ?? 300;
  if (!Number.isSafeInteger(now) || !Number.isSafeInteger(tolerance) || tolerance < 0) {
    throw new RangeError("Webhook timestamp options must be whole seconds.");
  }
  if (Math.abs(now - Number(timestamp)) > tolerance) {
    throw new WebhookVerificationError("timestamp", "The webhook timestamp is outside the allowed window.");
  }
  if (body.byteLength > 1_048_576) {
    throw new WebhookVerificationError("payload", "The webhook body is too large.");
  }

  const signatures = signature.split(/\s+/).flatMap((part) => {
    const candidate = part.startsWith("v1,") ? decodeSha256(part.slice(3)) : undefined;
    return candidate ? [candidate] : [];
  });
  if (!signatures.length) throw new WebhookVerificationError("signature", "A valid v1 signature is required.");

  const secrets = typeof signingSecrets === "string" ? [signingSecrets] : signingSecrets;
  if (!Array.isArray(secrets) || !secrets.length || secrets.length > 4) {
    throw new WebhookVerificationError("secret", "Supply one to four TokenCraft signing secrets.");
  }
  const keys = secrets.map((secret) => {
    const raw = typeof secret === "string" && secret.startsWith("whsec_") ? decodeSha256(secret.slice(6)) : undefined;
    if (!raw) throw new WebhookVerificationError("secret", "A TokenCraft signing secret is invalid.");
    return raw;
  });

  const prefix = bytes.encode(`${id}.${timestamp}.`);
  const message = new Uint8Array(prefix.length + body.byteLength);
  message.set(prefix);
  message.set(body, prefix.length);
  const matches = await Promise.all(keys.map(async (raw) => {
    const key = await crypto.subtle.importKey("raw", raw, { name: "HMAC", hash: "SHA-256" }, false, ["verify"]);
    const checked = await Promise.all(signatures.map((candidate) => crypto.subtle.verify("HMAC", key, candidate, message)));
    return checked.some(Boolean);
  }));
  if (!matches.some(Boolean)) {
    throw new WebhookVerificationError("signature", "No signature matched the webhook body.");
  }

  let event: unknown;
  try {
    event = JSON.parse(utf8.decode(message.subarray(prefix.length)));
  } catch {
    throw new WebhookVerificationError("payload", "The signed body is not UTF-8 JSON.");
  }
  if (!object(event) || event.id !== id || event.schema_version !== 1
      || typeof event.type !== "string"
      || typeof event.occurred_at !== "string" || !Number.isFinite(Date.parse(event.occurred_at))
      || typeof event.sequence !== "number" || !Number.isSafeInteger(event.sequence) || event.sequence < 1
      || !object(event.data)) {
    throw invalidPayload();
  }
  return { event, type: event.type, data: event.data };
}

function invalidPayload() {
  return new WebhookVerificationError("payload", "The signed event does not match the TokenCraft v1 contract.");
}

function isClientReference(value: unknown): value is string {
  return typeof value === "string" && value.length <= 64
    && /^[A-Za-z0-9]/.test(value) && !/[^A-Za-z0-9._:-]/.test(value);
}

function decodeSha256(value: string) {
  if (!/^[A-Za-z0-9+/]{43}=$/.test(value)) return undefined;
  const binary = atob(value);
  if (btoa(binary) !== value) return undefined;
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}
