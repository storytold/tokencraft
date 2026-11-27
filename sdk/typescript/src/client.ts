import { ArtifactTransferError, readArtifact, readVerifiedContent, sha256, validContentType, validateArtifactRef } from "./content.js";
import type { ContentMetadata, DownloadOptions, RetainedContent, VerifiedArtifact } from "./content.js";
import {
  deadline, gatewayError, hasIdPrefix, isRequestId, milliseconds, object, pause, retryAfter, TokenCraftError, TokenCraftTimeoutError,
} from "./http.js";
import { validateTraceparent } from "./trace.js";
import type { OtlpTrace, RequestTraceSnapshot, TraceBaseline } from "./trace.js";
import {
  invalidWorkflowResponse, maxWorkflowFileBytes, validVersion, workflowInputs, workflowOutputPath, workflowPath,
  workflowRunDone, workflowRunPath,
} from "./workflows.js";
import type { WorkflowEndpoint, WorkflowReceipt, WorkflowRun, WorkflowUpload } from "./workflows.js";
import type {
  ArtifactRef, AudioProviderResult, CallOptions, Catalog, ClientOptions, ImageProviderResult, ListOptions, MeshProviderResult, ModelCatalog, ModelListOptions, ProviderPath, ProviderResult, Quote, QuoteInput, QuoteOptions,
  Receipt, RequestEvents, RequestPage, RequestTrace, RoutePreview, RoutePreviewOptions, SubmitOptions,
  UsageOptions, UsageReport, VideoProviderResult, WaitOptions,
} from "./types.js";

const inspectable = new Set(["succeeded", "failed", "canceled", "awaiting_reconciliation"]);

type GatewayRead<T> = { value: T; headers: Headers };
type RetryOperation = "submission" | "download";

interface WireOptions extends CallOptions {
  auth?: boolean;
  body?: object;
  binary?: { bytes: ArrayBuffer; contentType: string; checksum: string };
  key?: string;
  requestId?: string;
  cache?: RequestCache;
  accept?: string;
  retry?: RetryOperation;
  traceparent?: string;
}

export class TokenCraft {
  readonly #base: URL;
  readonly #apiKey?: string;
  readonly #timeout: number;
  readonly #maxAttempts: Record<RetryOperation, number>;
  readonly #fetch: typeof fetch;

  constructor(options: ClientOptions) {
    const url = new URL(options.baseUrl);
    const local = url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
    if ((!local && url.protocol !== "https:") || url.username || url.password || url.search || url.hash) {
      throw new TypeError("Use an HTTPS gateway URL or loopback HTTP, without credentials, query, or fragment.");
    }
    if (options.apiKey !== undefined && !/^[!-~]{1,512}$/.test(options.apiKey)) {
      throw new TypeError("Supply a valid TokenCraft API key.");
    }
    url.pathname = url.pathname.replace(/\/+$/, "") + "/";
    this.#base = url;
    this.#apiKey = options.apiKey;
    this.#timeout = milliseconds(options.timeoutMs ?? 30_000, "timeoutMs");
    this.#maxAttempts = {
      submission: attemptLimit(options.submitMaxAttempts ?? 1, "submitMaxAttempts"),
      download: attemptLimit(options.downloadMaxAttempts ?? 1, "downloadMaxAttempts"),
    };
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  }

  catalog(options: CallOptions = {}): Promise<Catalog> {
    return this.#call("GET", "v1/catalog/offers", options);
  }

  /** Discover models in the key's project, or the public catalog without a key. */
  models(options: ModelListOptions = {}): Promise<ModelCatalog> {
    const scoped = options.scope === "project" || (options.scope !== "public" && this.#apiKey !== undefined);
    const path = scoped ? "v1/catalog/models/scoped" : "v1/catalog/models";
    return this.#call("GET", path, { signal: options.signal, auth: scoped, cache: scoped ? "no-store" : undefined });
  }

  quote(input: QuoteInput, options: QuoteOptions = {}): Promise<Quote> {
    const scoped = options.scope === "project" || (options.scope !== "public" && this.#apiKey !== undefined);
    if (!scoped && (options.clientRef !== undefined || options.offerId !== undefined)) {
      throw new TypeError("Client attribution and offer pins require a project quote.");
    }
    const path = scoped ? pathWith("v1/quotes/scoped", {
      client_ref: options.clientRef, pinned_offer_id: options.offerId,
    }) : "v1/quotes";
    return this.#call("POST", path, {
      auth: scoped, body: input, signal: options.signal,
    });
  }

  submit(quote: Pick<Quote, "id" | "input" | "pinned_offer_id">, options: SubmitOptions): Promise<Receipt> {
    validateIdempotencyKey(options.idempotencyKey);
    return this.#call("POST", "v1/requests", {
      auth: true, key: options.idempotencyKey, signal: options.signal,
      retry: "submission",
      traceparent: options.traceparent === undefined ? undefined : validateTraceparent(options.traceparent),
      body: {
        quote_id: quote.id,
        pinned_offer_id: quote.pinned_offer_id,
        input: quote.input,
        client_ref: options.clientRef,
        app_ref: options.appRef,
        delivery: options.webhookSubscriptionId ? { webhook_subscription_id: options.webhookSubscriptionId } : undefined,
        messages: options.messages,
        image_prompt: options.imagePrompt,
        video_prompt: options.videoPrompt,
        audio_prompt: options.audioPrompt,
        mesh_prompt: options.meshPrompt,
        chat_options: options.chatOptions,
      },
    });
  }

  get(requestId: string, options: CallOptions = {}): Promise<RequestTrace> {
    return this.#call<RequestTrace>("GET", requestPath(requestId), { ...options, auth: true, requestId })
      .then((trace) => checkedRequest(trace, trace.id, requestId));
  }

  events(requestId: string, options: CallOptions = {}): Promise<RequestEvents> {
    return this.#call<RequestEvents>("GET", `${requestPath(requestId)}/events`, { ...options, auth: true, requestId })
      .then((events) => checkedRequest(events, events.id, requestId));
  }

  trace(requestId: string, options: CallOptions = {}): Promise<RequestTraceSnapshot> {
    return this.#call<RequestTraceSnapshot>("GET", `${requestPath(requestId)}/trace`, { ...options, auth: true, requestId })
      .then((trace) => trace.evidence == null ? trace : checkedRequest(trace, trace.evidence.request_id, requestId));
  }

  traceOtlp(requestId: string, options: CallOptions = {}): Promise<OtlpTrace> {
    return this.#call("GET", `${requestPath(requestId)}/trace/otlp`, { ...options, auth: true, requestId });
  }

  traceBaseline(requestId: string, options: CallOptions = {}): Promise<TraceBaseline> {
    return this.#call("GET", `${requestPath(requestId)}/trace/baseline`, { ...options, auth: true, requestId });
  }

  async providerPath(requestId: string, options: CallOptions = {}): Promise<ProviderPath | null> {
    const trace = await this.trace(requestId, options);
    return trace.provider_path ?? null;
  }

  previewRoute(requestId: string, options: RoutePreviewOptions = {}): Promise<RoutePreview> {
    return this.#call("POST", `${requestPath(requestId)}/route-preview`, {
      auth: true, requestId, signal: options.signal,
      body: { max_total_usd_nanos: options.maxTotalUsdNanos, preferred_providers: options.preferredProviders },
    });
  }

  result(requestId: string, options: CallOptions = {}): Promise<ProviderResult> {
    return this.#call("GET", `${requestPath(requestId)}/result`, { ...options, auth: true, requestId });
  }

  imageResult(requestId: string, options: CallOptions = {}): Promise<ImageProviderResult> {
    return this.#call("GET", `${requestPath(requestId)}/result`, { ...options, auth: true, requestId });
  }

  videoResult(requestId: string, options: CallOptions = {}): Promise<VideoProviderResult> {
    return this.#call("GET", `${requestPath(requestId)}/result`, { ...options, auth: true, requestId });
  }

  audioResult(requestId: string, options: CallOptions = {}): Promise<AudioProviderResult> {
    return this.#call("GET", `${requestPath(requestId)}/result`, { ...options, auth: true, requestId });
  }

  meshResult(requestId: string, options: CallOptions = {}): Promise<MeshProviderResult> {
    return this.#call("GET", `${requestPath(requestId)}/result`, { ...options, auth: true, requestId });
  }

  /** Read dates and restore progress without fetching retained input or output. */
  contentMetadata(requestId: string, options: CallOptions = {}): Promise<ContentMetadata> {
    return this.#call("GET", `${requestPath(requestId)}/content?metadata_only=true`, {
      ...options, auth: true, requestId, cache: "no-store",
    });
  }

  warmContent(requestId: string, options: CallOptions = {}): Promise<RetainedContent> {
    return this.#call("GET", `${requestPath(requestId)}/content`, { ...options, auth: true, requestId, cache: "no-store" });
  }

  /** Read an available archive. A cold object may return `archive_retrieval_required`. */
  retrieveContent(requestId: string, options: CallOptions = {}): Promise<RetainedContent> {
    return this.#call("POST", `${requestPath(requestId)}/content/retrieve`, {
      ...options, auth: true, requestId, cache: "no-store",
    });
  }

  async downloadArtifact(requestId: string, artifact: ArtifactRef, options: DownloadOptions = {}): Promise<VerifiedArtifact> {
    const reference = { ...artifact };
    validateArtifactRef(reference);
    const location = options.location ?? "warm";
    if (location !== "warm" && location !== "archived") throw new TypeError("Use warm or archived artifact storage.");
    const prefix = location === "archived" ? "content/artifacts" : "artifacts";
    const { value } = await this.#exchange("GET", `${requestPath(requestId)}/${prefix}/${reference.slot}`, {
      auth: true, requestId, signal: options.signal, cache: "no-store", accept: "application/octet-stream", retry: "download",
    }, (response, id) => readArtifact(response, reference, id));
    return value;
  }

  cancel(requestId: string, options: CallOptions = {}): Promise<RequestTrace> {
    return this.#call<RequestTrace>("POST", `${requestPath(requestId)}/cancel`, { ...options, auth: true, requestId, body: {} })
      .then((trace) => checkedRequest(trace, trace.id, requestId));
  }

  async list(options: ListOptions = {}): Promise<RequestPage> {
    const page = await this.#call<RequestPage>("GET", pathWith("v1/requests", {
      client_ref: options.clientRef, after: options.after, app_ref: options.appRef, trace_id: options.traceId,
      model: options.model, provider: options.provider, status: options.status, cost_state: options.costState,
      from: options.from, to: options.to, diagnostic: options.diagnostic,
      app_unattributed: options.appUnattributed ? "true" : undefined,
      client_unattributed: options.clientUnattributed ? "true" : undefined,
      exact_model: options.exactModel ? "true" : undefined,
    }), { auth: true, signal: options.signal });
    if (options.diagnostic && page.diagnostic !== options.diagnostic) {
      throw new Error("The gateway did not confirm the requested diagnostic filter.");
    }
    return page;
  }

  usage(options: UsageOptions = {}): Promise<UsageReport> {
    return this.#call("GET", pathWith("v1/usage", {
      client_ref: options.clientRef, after: options.after, from: options.from, to: options.to,
      group_by: options.groupBy, model: options.model, provider: options.provider, app_ref: options.appRef,
    }), { auth: true, signal: options.signal });
  }

  /** Use an application key issued by this published workflow. */
  workflow(projectId: string, workflowId: string): WorkflowEndpoint {
    const path = workflowPath(projectId, workflowId);
    const runPath = (runId: string) => workflowRunPath(path, runId);
    const privateCall = { auth: true, cache: "no-store" } as const;
    return {
      contract: (options = {}) => this.#call("GET", `${path}/openapi.json`, { ...options, ...privateCall }),
      upload: async (contentType, bytes, options = {}) => {
        if (typeof contentType !== "string" || !validContentType(contentType)
            || !(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > maxWorkflowFileBytes) {
          throw new TypeError("Supply a media type and 1 byte to 32 MiB of file content.");
        }
        options.signal?.throwIfAborted();
        const copy = Uint8Array.from(bytes);
        const checksum = await sha256(copy);
        const upload = await this.#call<WorkflowUpload>("POST", `${path}/uploads`, {
          ...options, ...privateCall, binary: { bytes: copy.buffer, contentType, checksum },
        });
        if (!hasIdPrefix(upload.upload_id, "wfu_") || typeof upload.expires_at !== "string") {
          throw invalidWorkflowResponse(201);
        }
        return upload;
      },
      invoke: async (inputs, options) => {
        validateIdempotencyKey(options.idempotencyKey);
        if (options.reuseRunId !== undefined) runPath(options.reuseRunId);
        const receipt = await this.#call<WorkflowReceipt>("POST", `${path}/runs`, {
          ...privateCall, signal: options.signal, key: options.idempotencyKey, retry: "submission",
          body: { inputs: workflowInputs(inputs), client_ref: options.clientRef, reuse_run_id: options.reuseRunId },
        });
        if (!hasIdPrefix(receipt.id, "wfr_") || receipt.status_url !== `/${runPath(receipt.id)}`
            || !validVersion(receipt.workflow_version) || typeof receipt.replayed !== "boolean") {
          throw invalidWorkflowResponse(receipt.replayed === true ? 200 : 202);
        }
        return receipt;
      },
      get: async (runId, options = {}) => {
        const run = await this.#call<WorkflowRun>("GET", runPath(runId), { ...options, ...privateCall, requestId: runId });
        workflowRunDone(run, runId);
        return run;
      },
      wait: (runId, options = {}) => this.#poll<WorkflowRun>(
        runPath(runId), runId, options, (run) => workflowRunDone(run, runId),
      ),
      cancel: async (runId, options = {}) => {
        await this.#exchange("POST", `${runPath(runId)}/cancel`, { ...options, ...privateCall, requestId: runId },
          async (response) => {
            if (response.status !== 204) throw invalidWorkflowResponse(response.status, runId);
          });
      },
      downloadOutput: async (runId, name, options = {}) => {
        const { value } = await this.#exchange("GET", workflowOutputPath(runPath(runId), name), {
          ...options, ...privateCall, requestId: runId, accept: "application/octet-stream", retry: "download",
        }, (response) => readVerifiedContent(response, maxWorkflowFileBytes, runId));
        return value;
      },
    };
  }

  async wait(requestId: string, options: WaitOptions = {}): Promise<RequestTrace> {
    return this.#poll<RequestTrace>(requestPath(requestId), requestId, options, (trace) => {
      checkedRequest(trace, trace.id, requestId);
      if (inspectable.has(trace.status)) return true;
      if (trace.status === "queued" || trace.status === "running") return false;
      throw new TokenCraftError(200, "invalid_response", "The gateway returned an unknown request status.", requestId);
    });
  }

  async #poll<T>(path: string, requestId: string, options: WaitOptions, done: (value: T) => boolean): Promise<T> {
    const interval = milliseconds(options.pollIntervalMs ?? 750, "pollIntervalMs");
    const timeout = milliseconds(options.timeoutMs ?? 60_000, "timeoutMs");
    const poll = deadline(options.signal, timeout, () => requestId);
    try {
      let validator: string | null = null;
      for (;;) {
        let suggestedDelay = 0;
        try {
          const { value, headers }: GatewayRead<T | null> = await this.#read<T>(
            "GET", path, { signal: poll.signal, auth: true, requestId, cache: "no-store" }, validator,
          );
          if (value) {
            if (done(value)) return value;
            validator = headers.get("etag");
          }
          suggestedDelay = retryAfter(headers) ?? 0;
        } catch (error) {
          poll.signal.throwIfAborted();
          const interrupted = error instanceof TokenCraftError && error.status === 0 && error.code === "transport_error";
          if (!interrupted && !(error instanceof TokenCraftTimeoutError)) throw error;
        }
        await pause(Math.min(timeout, Math.max(interval, suggestedDelay)), poll.signal);
      }
    } finally {
      poll.close();
    }
  }

  async #call<T>(method: string, path: string, options: WireOptions): Promise<T> {
    return (await this.#read<T>(method, path, options)).value;
  }

  #read<T>(method: string, path: string, options: WireOptions): Promise<GatewayRead<T>>;
  #read<T>(method: string, path: string, options: WireOptions, validator: string | null): Promise<GatewayRead<T | null>>;
  #read<T>(method: string, path: string, options: WireOptions, validator?: string | null): Promise<GatewayRead<T | null>> {
    return this.#exchange(method, path, options, async (response, requestId) => {
      const payload: unknown = await response.json().catch((error: unknown) => {
        if (error instanceof SyntaxError) return null;
        throw error;
      });
      if (!object(payload)) throw new TokenCraftError(response.status, "invalid_response", "The gateway did not return a JSON object.", requestId);
      return payload as T;
    }, validator);
  }

  #exchange<T>(method: string, path: string, options: WireOptions,
    consume: (response: Response, id?: string) => Promise<T>): Promise<GatewayRead<T>>;
  #exchange<T>(method: string, path: string, options: WireOptions,
    consume: (response: Response, id?: string) => Promise<T>, validator: string | null | undefined): Promise<GatewayRead<T | null>>;
  async #exchange<T>(method: string, path: string, options: WireOptions,
    consume: (response: Response, id?: string) => Promise<T>, validator?: string | null): Promise<GatewayRead<T | null>> {
    const { auth, body, binary, key, requestId, signal, cache, accept, retry, traceparent } = options;
    const headers = new Headers({ Accept: accept ?? "application/json" });
    if (auth) {
      if (!this.#apiKey) throw new TokenCraftError(0, "missing_key", "This operation requires a TokenCraft API key.", requestId);
      headers.set("Authorization", `Bearer ${this.#apiKey}`);
    }
    if (binary) {
      headers.set("Content-Type", binary.contentType);
      headers.set("X-TokenCraft-Content-Sha256", binary.checksum);
    } else if (body) {
      headers.set("Content-Type", "application/json");
    }
    if (key) headers.set("Idempotency-Key", key);
    if (traceparent !== undefined) headers.set("traceparent", traceparent);
    if (typeof validator === "string") headers.set("If-None-Match", validator);
    const encoded = binary?.bytes ?? (body === undefined ? undefined : JSON.stringify(body));
    const maxAttempts = retry ? this.#maxAttempts[retry] : 1;
    const polling = validator !== undefined;
    let id = requestId;
    const request = deadline(signal, this.#timeout, () => id);
    try {
      for (let attempt = 1; ; attempt++) {
        request.signal.throwIfAborted();
        let receivedHeaders = false;
        try {
          const response = await this.#fetch(new URL(path, this.#base), {
            method, headers: new Headers(headers), body: encoded,
            signal: request.signal, credentials: "omit", redirect: "error", cache,
          });
          receivedHeaders = true;
          const observedId = ["tokencraft-request-id", "x-request-id"]
            .map((name) => response.headers.get(name)).find(isRequestId);
          if (observedId) id = observedId;
          request.signal.throwIfAborted();
          const delay = response.status === 429 || response.status === 503 ? retryAfter(response.headers) : undefined;
          if (response.status === 304 && typeof validator === "string") {
            return { value: null, headers: response.headers };
          }
          if (delay !== undefined && (polling || attempt < maxAttempts)) {
            void response.body?.cancel().catch(() => {});
            if (polling) return { value: null, headers: response.headers };
            await pause(Math.min(delay, this.#timeout), request.signal);
            continue;
          }
          if (!response.ok) {
            throw await gatewayError(response, request.signal, id);
          }
          const value = await consume(response, id);
          request.signal.throwIfAborted();
          return { value, headers: response.headers };
        } catch (error) {
          const submissionInterrupted = retry === "submission" && error instanceof TypeError;
          const downloadInterrupted = retry === "download"
            && (receivedHeaders ? error instanceof ArtifactTransferError : error instanceof TypeError);
          if (attempt >= maxAttempts || !(submissionInterrupted || downloadInterrupted)) throw error;
          await pause(Math.min(100 * 2 ** (attempt - 1), 1000), request.signal);
        }
      }
    } catch (error) {
      signal?.throwIfAborted();
      // An expired detail read can still report the known HTTP status.
      if (error instanceof TokenCraftError && error.status >= 300) throw error;
      request.signal.throwIfAborted();
      if (error instanceof TokenCraftError) throw error;
      const cause = error instanceof ArtifactTransferError ? error.cause : error;
      throw new TokenCraftError(0, "transport_error", "Could not reach the TokenCraft gateway.", id, { cause });
    } finally {
      request.close();
    }
  }
}

function attemptLimit(attempts: number, option: string): number {
  if (!Number.isInteger(attempts) || attempts < 1 || attempts > 8) {
    throw new RangeError(`${option} must be an integer from 1 to 8.`);
  }
  return attempts;
}

function validateIdempotencyKey(key: string): void {
  if (typeof key !== "string" || key.length < 8 || key.length > 128 || /[^A-Za-z0-9._:-]/.test(key)) {
    throw new TypeError("Idempotency keys must have 8–128 ASCII letters, numbers, or . _ : -.");
  }
}

function requestPath(id: string): string {
  if (!isRequestId(id)) throw new TypeError("Supply a valid TokenCraft request ID.");
  return `v1/requests/${id}`;
}

function checkedRequest<T>(value: T, observedId: unknown, requestId: string): T {
  if (observedId !== requestId) {
    throw new TokenCraftError(200, "invalid_response", "The response did not match the requested TokenCraft ID.", requestId);
  }
  return value;
}

function pathWith(path: string, values: Record<string, string | undefined>): string {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(values)) if (value !== undefined) query.set(key, value);
  return query.size ? `${path}?${query}` : path;
}
