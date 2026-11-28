import { parseRetryAfter } from "./retryAfter.ts";

export type ArtifactReference = { byte_length: number; sha256: string };
export type ArtifactRetryReason = "busy" | "interrupted";

const maxAttempts = 3;
const transferTimeoutMs = 60_000;
const maxRetryDelayMs = 5_000;

class InterruptedTransfer extends Error {
  constructor(cause: unknown) {
    super("The connection was interrupted. Try downloading this output again.", { cause });
  }
}

const extensions: Record<string, string> = {
  "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp", "image/gif": "gif",
  "video/mp4": "mp4", "video/webm": "webm", "video/quicktime": "mov",
  "audio/wav": "wav", "audio/mpeg": "mp3", "audio/flac": "flac", "audio/ogg": "ogg",
  "model/gltf-binary": "glb", "model/gltf+json": "gltf", "text/plain": "txt", "text/vtt": "vtt",
  "application/pdf": "pdf", "application/json": "json", "application/zip": "zip",
};

export function artifactFilename(stem: string, mime: string): string {
  const name = stem.replace(/[^a-zA-Z0-9_-]/g, "-").slice(0, 80) || "tokencraft-output";
  return `${name}.${Object.hasOwn(extensions, mime) ? extensions[mime] : "bin"}`;
}

function pause(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const cancel = () => {
      clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", cancel);
      resolve();
    }, ms);
    signal.addEventListener("abort", cancel, { once: true });
  });
}

function retryDelay(response: Response): number | undefined {
  if (response.status !== 429 && response.status !== 503) return;
  const delay = parseRetryAfter(response.headers.get("Retry-After"));
  return delay !== undefined && delay <= maxRetryDelayMs ? delay : undefined;
}

export async function readArtifact(
  url: string,
  caller: AbortSignal,
  onRetry: (reason?: ArtifactRetryReason) => void,
  reference?: ArtifactReference,
  expectedType?: string,
): Promise<Blob> {
  const signal = AbortSignal.any([caller, AbortSignal.timeout(transferTimeoutMs)]);
  for (let attempt = 1; ; attempt++) {
    signal.throwIfAborted();
    let delay: number;
    let reason: ArtifactRetryReason;
    try {
      const response = await fetch(url, {
        credentials: "include", redirect: "error", cache: "no-store", signal,
        headers: { Accept: "application/octet-stream" },
      }).catch((cause: unknown) => {
        if (cause instanceof TypeError) throw new InterruptedTransfer(cause);
        throw cause;
      });
      signal.throwIfAborted();
      const hint = retryDelay(response);
      if (attempt === maxAttempts || hint === undefined) {
        return await verifyArtifact(response, signal, reference, expectedType);
      }
      void response.body?.cancel().catch(() => {});
      delay = hint;
      reason = "busy";
    } catch (cause) {
      signal.throwIfAborted();
      if (attempt === maxAttempts || !(cause instanceof InterruptedTransfer)) throw cause;
      delay = 250 * 2 ** (attempt - 1);
      reason = "interrupted";
    }
    onRetry(reason);
    await pause(delay, signal);
    onRetry();
  }
}

async function verifyArtifact(
  response: Response,
  signal: AbortSignal,
  reference?: ArtifactReference,
  expectedType?: string,
): Promise<Blob> {
  if (response.status !== 200) {
    void response.body?.cancel().catch(() => {});
    switch (response.status) {
      case 401: throw new Error("Sign in again to download this output.");
      case 403: throw new Error("Your account does not have access to this output.");
      case 404: throw new Error("This output is no longer available in this project.");
      case 409: throw new Error("This output needs to be restored from cold storage.");
      case 429: throw new Error("Downloads are busy. Try again shortly.");
      case 503: throw new Error(response.headers.has("Retry-After")
        ? "Downloads are busy. Try again shortly." : "This output is temporarily unavailable. Try again.");
      default: throw new Error("The gateway could not send this output. Try again.");
    }
  }

  const limit = (reference ? 8 : 32) * 1024 * 1024;
  const invalid = () => new Error("This output failed verification. Try downloading it again.");
  const mime = response.headers.get("Content-Type")?.split(";", 1)[0].trim().toLowerCase();
  const sha256 = response.headers.get("X-TokenCraft-Content-Sha256");
  // Fetch exposes decoded bytes. The HTTP length can describe a compressed representation.
  const expected = reference?.byte_length;
  if (!response.body || !mime || (expectedType && mime !== expectedType)
      || !sha256 || !/^[0-9a-f]{64}$/.test(sha256)
      || (expected !== undefined && (!Number.isInteger(expected) || expected < 1 || expected > limit))
      || (reference && sha256 !== reference.sha256)) {
    void response.body?.cancel().catch(() => {});
    throw invalid();
  }

  const maximum = expected ?? limit;
  let bytes = new Uint8Array(expected ?? 0);
  const reader = response.body.getReader();
  let used = 0;
  try {
    for (;;) {
      const chunk = await reader.read().catch((cause: unknown) => {
        throw new InterruptedTransfer(cause);
      });
      if (chunk.done) break;
      const required = used + chunk.value.byteLength;
      if (required > maximum) {
        void reader.cancel().catch(() => {});
        throw invalid();
      }
      if (required > bytes.length) {
        const expanded = new Uint8Array(Math.min(maximum, Math.max(64 * 1024, bytes.length * 2, required)));
        expanded.set(bytes);
        bytes = expanded;
      }
      bytes.set(chunk.value, used);
      used = required;
    }
  } finally {
    reader.releaseLock();
  }
  if (!used || (expected !== undefined && expected !== used)) throw invalid();
  signal.throwIfAborted();
  const decoded = bytes.subarray(0, used);
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", decoded));
  signal.throwIfAborted();
  if (Array.from(hash, (byte) => byte.toString(16).padStart(2, "0")).join("") !== sha256) throw invalid();
  return new Blob([decoded], { type: Object.hasOwn(extensions, mime) ? mime : "application/octet-stream" });
}
