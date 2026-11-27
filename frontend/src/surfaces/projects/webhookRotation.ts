export type WebhookRotation = {
  key: string;
  expectedVersion: number;
  stagedAt: number;
};

const recoveryWindowMs = 24 * 60 * 60 * 1000;
const tabAttempts = new Map<string, WebhookRotation>();

export function rotationStorageKey(actor: string, account: string, project: string, subscription: string) {
  return `tokencraft:webhook-rotation:${actor}:${account}:${project}:${subscription}`;
}

export function pendingWebhookRotation(storageKey: string, currentVersion: number): WebhookRotation | undefined {
  let attempt = tabAttempts.get(storageKey);
  if (!attempt) {
    try {
      const entry = sessionStorage.getItem(storageKey);
      if (entry !== null) attempt = JSON.parse(entry);
    } catch {
      // The in-memory attempt still supports retries with storage unavailable.
    }
  }
  if (!attempt || typeof attempt.key !== "string" || !/^[0-9a-f-]{36}$/.test(attempt.key)
      || !Number.isInteger(attempt.expectedVersion) || attempt.expectedVersion < 1
      || (currentVersion !== attempt.expectedVersion && currentVersion !== attempt.expectedVersion + 1)
      || !Number.isFinite(attempt.stagedAt) || attempt.stagedAt > Date.now()
      || attempt.stagedAt <= Date.now() - recoveryWindowMs) {
    return undefined;
  }
  return attempt;
}

export function stageWebhookRotation(storageKey: string, currentVersion: number): WebhookRotation {
  const pending = pendingWebhookRotation(storageKey, currentVersion);
  if (pending) return pending;
  const attempt = { key: crypto.randomUUID(), expectedVersion: currentVersion, stagedAt: Date.now() };
  tabAttempts.set(storageKey, attempt);
  try {
    sessionStorage.setItem(storageKey, JSON.stringify(attempt));
  } catch {
    // A retry in this tab can reuse the in-memory attempt.
  }
  return attempt;
}

export function clearWebhookRotation(storageKey: string) {
  tabAttempts.delete(storageKey);
  try {
    sessionStorage.removeItem(storageKey);
  } catch {
    // Storage may be disabled.
  }
}
