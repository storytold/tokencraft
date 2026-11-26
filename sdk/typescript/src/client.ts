import { readArtifact, validateArtifactRef } from "./content.js";
import type { ContentMetadata, DownloadOptions, RetainedContent, VerifiedArtifact } from "./content.js";
import { deadline, isRequestId, milliseconds, object, pause, retryAfter, TokenCraftError } from "./http.js";
import { validateTraceparent } from "./trace.js";
import type { OtlpTrace, RequestTraceSnapshot, TraceBaseline } from "./trace.js";
import type {
  ArtifactRef, AudioProviderResult, CallOptions, Catalog, ClientOptions, ImageProviderResult, ListOptions, MeshProviderResult, ModelCatalog, ModelListOptions, ProviderPath, ProviderResult, Quote, QuoteInput, QuoteOptions,
  Receipt, RequestEvents, RequestPage, RequestTrace, RoutePreview, RoutePreviewOptions, SubmitOptions,
  UsageOptions, UsageReport, VideoProviderResult, WaitOptions,
} from "./types.js";

const inspectable = new Set(["succeeded", "failed", "canceled", "awaiting_reconciliation"]);

type GatewayRead<T> = { value: T; headers: Headers };

interface WireOptions extends CallOptions {
  auth?: boolean;
  body?: object;
  key?: string;
  requestId?: string;
  cache?: RequestCache;
  accept?: string;
  retrySubmission?: boolean;
  traceparent?: string;
}

export class TokenCraft {
  readonly #base: URL;
  readonly #apiKey?: string;
  readonly #timeout: number;
  readonly #submitMaxAttempts: number;
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
    this.#submitMaxAttempts = options.submitMaxAttempts ?? 1;
    if (!Number.isInteger(this.#submitMaxAttempts) || this.#submitMaxAttempts < 1 || this.#submitMaxAttempts > 8) {
      throw new RangeError("submitMaxAttempts must be an integer from 1 to 8.");
    }
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
    if (!/^[A-Za-z0-9._:-]{8,128}$/.test(options.idempotencyKey)) {
      throw new TypeError("Idempotency keys must have 8–128 ASCII letters, numbers, or . _ : -.");
    }
    return this.#call("POST", "v1/requests", {
      auth: true, key: options.idempotencyKey, signal: options.signal,
      retrySubmission: true,
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
    return this.#call("GET", requestPath(requestId), { ...options, auth: true, requestId });
  }

  events(requestId: string, options: CallOptions = {}): Promise<RequestEvents> {
    return this.#call("GET", `${requestPath(requestId)}/events`, { ...options, auth: true, requestId });
  }

  trace(requestId: string, options: CallOptions = {}): Promise<RequestTraceSnapshot> {
    return this.#call("GET", `${requestPath(requestId)}/trace`, { ...options, auth: true, requestId });
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
      auth: true, requestId, signal: options.signal, cache: "no-store", accept: "application/octet-stream",
    }, (response, id) => readArtifact(response, reference, id));
    return value;
  }

  cancel(requestId: string, options: CallOptions = {}): Promise<RequestTrace> {
    return this.#call("POST", `${requestPath(requestId)}/cancel`, { ...options, auth: true, requestId, body: {} });
  }

  list(options: ListOptions = {}): Promise<RequestPage> {
    return this.#call("GET", pathWith("v1/requests", {
      client_ref: options.clientRef, after: options.after, app_ref: options.appRef, trace_id: options.traceId,
      model: options.model, provider: options.provider, status: options.status, cost_state: options.costState,
      from: options.from, to: options.to,
      app_unattributed: options.appUnattributed ? "true" : undefined,
      client_unattributed: options.clientUnattributed ? "true" : undefined,
      exact_model: options.exactModel ? "true" : undefined,
    }), { auth: true, signal: options.signal });
  }

  usage(options: UsageOptions = {}): Promise<UsageReport> {
    return this.#call("GET", pathWith("v1/usage", {
      client_ref: options.clientRef, after: options.after, from: options.from, to: options.to,
      group_by: options.groupBy, model: options.model, provider: options.provider, app_ref: options.appRef,
    }), { auth: true, signal: options.signal });
  }

  async wait(requestId: string, options: WaitOptions = {}): Promise<RequestTrace> {
    const interval = milliseconds(options.pollIntervalMs ?? 750, "pollIntervalMs");
    const timeout = milliseconds(options.timeoutMs ?? 60_000, "timeoutMs");
    const poll = deadline(options.signal, timeout, () => requestId);
    try {
      const path = requestPath(requestId);
      let validator: string | null = null;
      for (;;) {
        const { value: trace, headers }: GatewayRead<RequestTrace | null> = await this.#read<RequestTrace>(
          "GET", path, { signal: poll.signal, auth: true, requestId }, validator,
        );
        if (trace) {
          if (inspectable.has(trace.status)) return trace;
          if (trace.status !== "queued" && trace.status !== "running") {
            throw new TokenCraftError(200, "invalid_response", "The gateway returned an unknown request status.", requestId);
          }
          validator = headers.get("etag");
        }
        const suggestedDelay = retryAfter(headers) ?? 0;
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
      const payload: unknown = await response.json().catch(() => null);
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
    const { auth, body, key, requestId, signal, cache, accept, retrySubmission, traceparent } = options;
    const headers = new Headers({ Accept: accept ?? "application/json" });
    if (auth) {
      if (!this.#apiKey) throw new TokenCraftError(0, "missing_key", "This operation requires a TokenCraft API key.", requestId);
      headers.set("Authorization", `Bearer ${this.#apiKey}`);
    }
    if (body) headers.set("Content-Type", "application/json");
    if (key) headers.set("Idempotency-Key", key);
    if (traceparent !== undefined) headers.set("traceparent", traceparent);
    if (typeof validator === "string") headers.set("If-None-Match", validator);
    const encoded = body === undefined ? undefined : JSON.stringify(body);
    const maxAttempts = retrySubmission ? this.#submitMaxAttempts : 1;
    const polling = validator !== undefined;
    let id = requestId;
    const request = deadline(signal, this.#timeout, () => id);
    try {
      for (let attempt = 1; ; attempt++) {
        request.signal.throwIfAborted();
        const response = await this.#fetch(new URL(path, this.#base), {
          method, headers: new Headers(headers), body: encoded,
          signal: request.signal, credentials: "omit", redirect: "error", cache,
        });
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
          const payload: unknown = await response.json().catch(() => null);
          request.signal.throwIfAborted();
          const error = object(payload) && object(payload.error) ? payload.error : {};
          const code = typeof error.code === "string" ? error.code : "http_error";
          const message = typeof error.message === "string" ? error.message : `Gateway returned HTTP ${response.status}.`;
          throw new TokenCraftError(response.status, code, message, id);
        }
        const value = await consume(response, id);
        request.signal.throwIfAborted();
        return { value, headers: response.headers };
      }
    } catch (error) {
      request.signal.throwIfAborted();
      if (error instanceof TokenCraftError) throw error;
      throw new TokenCraftError(0, "transport_error", "Could not reach the TokenCraft gateway.", id, { cause: error });
    } finally {
      request.close();
    }
  }
}

function requestPath(id: string): string {
  if (!isRequestId(id)) throw new TypeError("Supply a valid TokenCraft request ID.");
  return `v1/requests/${id}`;
}

function pathWith(path: string, values: Record<string, string | undefined>): string {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(values)) if (value !== undefined) query.set(key, value);
  return query.size ? `${path}?${query}` : path;
}
