export function parseRetryAfter(header: string | null, now = Date.now()): number | undefined {
  const value = header?.trim() ?? "";
  if (/^\d+$/.test(value)) {
    return Math.min(Number(value) * 1_000, Number.MAX_SAFE_INTEGER);
  }
  const date = new Date(value);
  if (!Number.isFinite(date.getTime()) || date.toUTCString() !== value) return;
  return Math.max(0, date.getTime() - now);
}
