export class TokenCraftError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly requestId?: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "TokenCraftError";
  }
}

export class TokenCraftTimeoutError extends Error {
  constructor(readonly timeoutMs: number, readonly requestId?: string) {
    super(`TokenCraft operation exceeded ${timeoutMs} ms.`);
    this.name = "TokenCraftTimeoutError";
  }
}

const maxGatewayErrorBytes = 64 * 1024;
const gatewayErrorBodyTimeoutMs = 1_000;

export async function gatewayError(
  response: Response, signal: AbortSignal, requestId?: string,
): Promise<TokenCraftError> {
  const detail = await readErrorDetail(response, signal);
  const code = typeof detail?.code === "string" ? detail.code : "http_error";
  const message = typeof detail?.message === "string" ? detail.message : `Gateway returned HTTP ${response.status}.`;
  return new TokenCraftError(response.status, code, message, requestId);
}

async function readErrorDetail(response: Response, signal: AbortSignal): Promise<Record<string, unknown> | undefined> {
  const body = response.body;
  if (!body || body.locked) return;
  if (Number(response.headers.get("content-length")) > maxGatewayErrorBytes) {
    void body.cancel().catch(() => {});
    return;
  }

  const reader = body.getReader();
  let stopped = false;
  const stop = () => {
    stopped = true;
    void reader.cancel().catch(() => {});
  };
  const timer = setTimeout(stop, gatewayErrorBodyTimeoutMs);
  signal.addEventListener("abort", stop, { once: true });
  if (signal.aborted) stop();
  try {
    const decoder = new TextDecoder("utf-8", { fatal: true });
    let text = "";
    let bytes = 0;
    for (;;) {
      const { value, done } = await reader.read();
      if (stopped) return;
      if (done) break;
      bytes += value.byteLength;
      if (bytes > maxGatewayErrorBytes) return;
      text += decoder.decode(value, { stream: true });
    }
    const payload: unknown = JSON.parse(text + decoder.decode());
    if (object(payload) && object(payload.error)) return payload.error;
  } catch {
    // The status and request ID are available even when the error body cannot be decoded.
  } finally {
    clearTimeout(timer);
    signal.removeEventListener("abort", stop);
    stop();
    reader.releaseLock();
  }
}

export function milliseconds(value: number, name: string): number {
  if (!Number.isInteger(value) || value <= 0 || value > 2_147_483_647) {
    throw new RangeError(`${name} must be a positive integer in milliseconds.`);
  }
  return value;
}

export function deadline(parent: AbortSignal | undefined, timeoutMs: number, requestId: () => string | undefined) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(new TokenCraftTimeoutError(timeoutMs, requestId())), timeoutMs);
  const relay = () => controller.abort(parent?.reason);
  if (parent?.aborted) relay();
  else parent?.addEventListener("abort", relay, { once: true });

  return {
    signal: controller.signal,
    close() {
      clearTimeout(timer);
      parent?.removeEventListener("abort", relay);
    },
  };
}

export function pause(duration: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const abort = () => {
      clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, duration);
    signal.addEventListener("abort", abort, { once: true });
  });
}

export function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function retryAfter(headers: Headers): number | undefined {
  const value = headers.get("retry-after")?.trim();
  if (!value) return;
  if (/^\d+$/.test(value)) return Number(value) * 1_000;

  const now = Date.now();
  const date = httpDate(value, now);
  if (date !== undefined) return Math.max(0, date - now);
}

function httpDate(value: string, now: number): number | undefined {
  let canonical = value;
  const rfc850 = /^(Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday), (\d{2})-([A-Z][a-z]{2})-(\d{2}) (\d{2}:\d{2}:\d{2}) GMT$/.exec(value);
  if (rfc850) {
    const [, weekday, day, month, shortYear, time] = rfc850;
    const currentYear = new Date(now).getUTCFullYear();
    const nextCentury = Math.floor((currentYear + 50) / 100) * 100;
    const candidate = nextCentury + Number(shortYear);
    const year = candidate > currentYear + 50 ? candidate - 100 : candidate;
    canonical = `${weekday.slice(0, 3)}, ${day} ${month} ${year} ${time} GMT`;
  }
  const asctime = /^([A-Z][a-z]{2}) ([A-Z][a-z]{2}) (\d{2}| \d) (\d{2}:\d{2}:\d{2}) (\d{4})$/.exec(value);
  if (asctime) {
    const [, weekday, month, day, time, year] = asctime;
    canonical = `${weekday}, ${day.trim().padStart(2, "0")} ${month} ${year} ${time} GMT`;
  }
  if (canonical.length !== 29) return;
  const timestamp = Date.parse(canonical);
  // Round trip to reject invalid calendar dates and combined or malformed headers.
  if (Number.isFinite(timestamp) && new Date(timestamp).toUTCString() === canonical) return timestamp;
}

export function isRequestId(value: unknown): value is string {
  return hasIdPrefix(value, "req_");
}

export function hasIdPrefix(value: unknown, prefix: string): value is string {
  return typeof value === "string" && value.length === prefix.length + 26
    && value.startsWith(prefix) && /^[0-9a-hjkmnp-tv-z]{26}$/.test(value.slice(prefix.length));
}
