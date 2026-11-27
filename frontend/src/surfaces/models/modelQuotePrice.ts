import type { ModelQuote } from "../comparison/api";

type PriceQuote = Pick<ModelQuote, "expires_at" | "candidates">;

function validAt(deadline: string | undefined, now: number) {
  return deadline === undefined || Date.parse(deadline) > now;
}

export function lowestCurrentQuotePrice(quote: PriceQuote, now: number): number | null {
  if (!validAt(quote.expires_at, now)) return null;

  let lowest = Infinity;
  for (const candidate of quote.candidates) {
    const amount = candidate.estimated_usd_nanos;
    if (candidate.reason === "eligible" && amount !== null && Number.isSafeInteger(amount) && amount >= 0
        && validAt(candidate.rate_valid_until, now)) {
      lowest = Math.min(lowest, amount);
    }
  }
  return Number.isFinite(lowest) ? lowest / 1_000_000_000 : null;
}
