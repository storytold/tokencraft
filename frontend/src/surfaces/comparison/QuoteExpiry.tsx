import { useEffect, useState } from "react";
import { scheduleDeadline } from "@/lib/deadline";
import { quoteExpired } from "./modelSubmission";

const time = new Intl.DateTimeFormat(undefined, {
  hour: "numeric", minute: "2-digit", second: "2-digit", timeZoneName: "short",
});

export function useDeadlineClock(deadlines: (string | undefined)[]) {
  const [, setTick] = useState(0);
  const now = Date.now();
  const next = Math.min(...deadlines.map(value => Date.parse(value ?? "")).filter(time => time > now));
  useEffect(() => {
    if (!Number.isFinite(next)) return;
    return scheduleDeadline(next, () => setTick(tick => tick + 1));
  }, [next]);
  return now;
}

export function useQuoteExpired(expiresAt?: string) {
  return quoteExpired(expiresAt, useDeadlineClock([expiresAt]));
}

export function QuoteExpiry({ expiresAt, expired, stale }: { expiresAt?: string; expired: boolean; stale: boolean }) {
  if (stale) return <span>The gateway requires a fresh estimate. Refresh to continue.</span>;
  if (expired) return <span>Estimate expired. Refresh for a new request.</span>;
  if (!expiresAt) return null;
  return <span>Estimate valid until <time dateTime={expiresAt}>{time.format(new Date(expiresAt))}</time>.</span>;
}
