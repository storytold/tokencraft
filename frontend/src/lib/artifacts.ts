import { API_BASE_URL } from "./api";

export type ArtifactReference = { byte_length: number; sha256: string };

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
      window.clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = window.setTimeout(() => {
      signal.removeEventListener("abort", cancel);
      resolve();
    }, ms);
    signal.addEventListener("abort", cancel, { once: true });
  });
}

export async function readArtifact(
  path: string,
  signal: AbortSignal,
  onRetry: (waiting: boolean) => void,
  reference?: ArtifactReference,
  expectedType?: string,
): Promise<Blob> {
  const fetchArtifact = () => fetch(API_BASE_URL + path, {
    credentials: "include", redirect: "error", cache: "no-store", signal,
    headers: { Accept: "application/octet-stream" },
  });
  let response = await fetchArtifact();
  signal.throwIfAborted();
  const retryAfter = response.headers.get("Retry-After")?.trim() ?? "";
  const delay = /^\d+$/.test(retryAfter) ? Number(retryAfter) : Infinity;
  if ((response.status === 429 || response.status === 503) && delay <= 5) {
    void response.body?.cancel().catch(() => {});
    onRetry(true);
    await pause(delay * 1_000, signal);
    onRetry(false);
    response = await fetchArtifact();
  }
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
      const chunk = await reader.read();
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
  if (Array.from(hash, (byte) => byte.toString(16).padStart(2, "0")).join("") !== sha256) throw invalid();
  return new Blob([decoded], { type: Object.hasOwn(extensions, mime) ? mime : "application/octet-stream" });
}
