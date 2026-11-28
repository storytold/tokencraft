type Receipt = {
  key: string;
  stagedAt: number;
};

export type WebhookRotation = Receipt & { expectedVersion: number };

export type WebhookCreationInput = {
  name: string;
  destination_id: string;
  client_ref: string | null;
  workflow_app_id?: string;
};
export type WebhookCreation = Receipt & { input: WebhookCreationInput };

const recoveryWindowMs = 24 * 60 * 60 * 1000;
const tabAttempts = new Map<string, Receipt>();

export function rotationStorageKey(actor: string, account: string, project: string, subscription: string) {
  return `tokencraft:webhook-rotation:${actor}:${account}:${project}:${subscription}`;
}

function pendingReceipt(storageKey: string) {
  let attempt = tabAttempts.get(storageKey) as (Receipt & Record<string, unknown>) | undefined;
  if (!attempt) {
    try {
      const entry = sessionStorage.getItem(storageKey);
      if (entry !== null) attempt = JSON.parse(entry);
    } catch {
      // The in-memory attempt still supports retries with storage unavailable.
    }
  }
  const now = Date.now();
  if (!attempt || typeof attempt.key !== "string" || !/^[0-9a-f-]{36}$/.test(attempt.key)
      || !Number.isFinite(attempt.stagedAt) || attempt.stagedAt > now
      || attempt.stagedAt <= now - recoveryWindowMs) {
    return undefined;
  }
  return attempt;
}

export function pendingWebhookRotation(storageKey: string, currentVersion: number): WebhookRotation | undefined {
  const attempt = pendingReceipt(storageKey);
  if (!attempt || typeof attempt.expectedVersion !== "number"
      || !Number.isInteger(attempt.expectedVersion) || attempt.expectedVersion < 1
      || (currentVersion !== attempt.expectedVersion && currentVersion !== attempt.expectedVersion + 1)) return undefined;
  return { key: attempt.key, stagedAt: attempt.stagedAt, expectedVersion: attempt.expectedVersion };
}

export function stageWebhookRotation(storageKey: string, currentVersion: number): WebhookRotation {
  const pending = pendingWebhookRotation(storageKey, currentVersion);
  if (pending) return pending;
  return storeReceipt(storageKey, { key: crypto.randomUUID(), expectedVersion: currentVersion, stagedAt: Date.now() });
}

export function creationStorageKey(actor: string, account: string, project: string) {
  return `tokencraft:webhook-creation:${actor}:${account}:${project}`;
}

export function pendingWebhookCreation(storageKey: string): WebhookCreation | undefined {
  const receipt = pendingReceipt(storageKey);
  const input = receipt?.input as Partial<WebhookCreationInput> | undefined;
  if (!receipt || !input || typeof input.name !== "string" || !input.name || input.name.length > 80
      || typeof input.destination_id !== "string" || !input.destination_id || input.destination_id.length > 64
      || (input.client_ref !== null && (typeof input.client_ref !== "string" || input.client_ref.length > 64))
      || (input.workflow_app_id !== undefined && (typeof input.workflow_app_id !== "string" || input.workflow_app_id.length > 64))) {
    return undefined;
  }
  return { key: receipt.key, stagedAt: receipt.stagedAt, input: {
    name: input.name, destination_id: input.destination_id, client_ref: input.client_ref, workflow_app_id: input.workflow_app_id,
  } };
}

export function stageWebhookCreation(storageKey: string, input: WebhookCreationInput): WebhookCreation {
  return pendingWebhookCreation(storageKey)
    ?? storeReceipt(storageKey, { key: crypto.randomUUID(), input, stagedAt: Date.now() });
}

function storeReceipt<T extends Receipt>(storageKey: string, attempt: T): T {
  tabAttempts.set(storageKey, attempt);
  try {
    sessionStorage.setItem(storageKey, JSON.stringify(attempt));
  } catch {
    // A retry in this tab can reuse the in-memory attempt.
  }
  return attempt;
}

export function clearWebhookReceipt(storageKey: string) {
  tabAttempts.delete(storageKey);
  try {
    sessionStorage.removeItem(storageKey);
  } catch {
    // Storage may be disabled.
  }
}
