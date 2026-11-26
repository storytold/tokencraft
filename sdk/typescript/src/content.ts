import { TokenCraftError } from "./http.js";
import type {
  ArtifactRef, AudioProviderResult, CallOptions, ImageProviderResult, MeshProviderResult,
  ProviderResult, VideoProviderResult,
} from "./types.js";

export type ContentPhase = "processing" | "available" | "archiving" | "archived" | "removed" | "expired";

export interface RestoreProgress {
  state: "pending" | "ready" | "failed";
  ready_objects: number;
  total_objects: number;
  requested_at: string;
  retry_at: string;
}

export interface ContentMetadata {
  phase: ContentPhase;
  media?: boolean;
  archive_at: string | null;
  expires_at: string | null;
  restore?: RestoreProgress;
}

export type RetainedResult = ProviderResult["result"] | ImageProviderResult["result"] | VideoProviderResult["result"]
  | AudioProviderResult["result"] | MeshProviderResult["result"];

export interface RetainedContent extends ContentMetadata {
  /** The original chat or connector input as recorded by the gateway. */
  input?: Record<string, unknown>;
  result?: RetainedResult;
}

export interface DownloadOptions extends CallOptions {
  location?: "warm" | "archived";
}

export interface VerifiedArtifact {
  contentType: string;
  bytes: Uint8Array;
}

const maxArtifactBytes = 8 * 1024 * 1024;
const mediaTypes = new Set(["image/png", "video/mp4", "audio/wav", "model/gltf-binary"]);

export function validateArtifactRef(artifact: ArtifactRef) {
  if (!Number.isInteger(artifact?.slot) || artifact.slot < 0 || artifact.slot > 7
      || !Number.isInteger(artifact.byte_length) || artifact.byte_length < 1 || artifact.byte_length > maxArtifactBytes
      || typeof artifact.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(artifact.sha256)) {
    throw new TypeError("Supply a valid saved artifact reference.");
  }
}

export function readArtifact(response: Response, artifact: ArtifactRef, requestId?: string): Promise<VerifiedArtifact> {
  return readVerifiedContent(response, maxArtifactBytes, requestId, artifact);
}

export function validContentType(contentType: string): boolean {
  return contentType.length <= 127
    && /^[a-z][a-z0-9.+-]*\/[a-z][a-z0-9.+-]*$/.test(contentType)
    && !/\s/.test(contentType);
}

export async function sha256(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export async function readVerifiedContent(
  response: Response,
  maxBytes: number,
  requestId?: string,
  reference?: ArtifactRef,
): Promise<VerifiedArtifact> {
  const invalid = () => new TokenCraftError(response.status, "invalid_artifact", "The gateway artifact did not match its declared content.", requestId);
  const contentType = response.headers.get("content-type");
  const contentLength = response.headers.get("content-length");
  const declared = contentLength === null ? undefined : Number(contentLength);
  const checksum = response.headers.get("x-tokencraft-content-sha256");
  if (response.status !== 200 || !contentType || !response.body
      || !(reference ? mediaTypes.has(contentType) : validContentType(contentType))
      || checksum?.length !== 64 || !/^[0-9a-f]{64}$/.test(checksum)
      || (declared !== undefined && (declared < 1 || declared > maxBytes || String(declared) !== contentLength
        || !Number.isInteger(declared)))
      || (reference && (reference.sha256 !== checksum || (declared !== undefined && declared !== reference.byte_length)))) {
    void response.body?.cancel().catch(() => undefined);
    throw invalid();
  }

  const expected = reference?.byte_length ?? declared;
  const limit = expected ?? maxBytes;
  let buffer = new Uint8Array(expected ?? 0);
  const reader = response.body.getReader();
  let length = 0;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      if (value.byteLength > limit - length) {
        void reader.cancel().catch(() => undefined);
        throw invalid();
      }
      const nextLength = length + value.byteLength;
      if (nextLength > buffer.length) {
        const capacity = Math.min(limit, Math.max(64 * 1024, buffer.length * 2, nextLength));
        const grown = new Uint8Array(capacity);
        grown.set(buffer);
        buffer = grown;
      }
      buffer.set(value, length);
      length = nextLength;
    }
  } finally {
    reader.releaseLock();
  }
  if (length === 0 || (expected !== undefined && length !== expected)) throw invalid();
  const bytes = buffer.length === length ? buffer : buffer.slice(0, length);
  if (await sha256(bytes) !== checksum) throw invalid();
  return { contentType, bytes };
}
