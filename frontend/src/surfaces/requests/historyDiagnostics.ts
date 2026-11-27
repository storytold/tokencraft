import type { HistoryFilters, RequestDiagnostic } from "@/surfaces/comparison/api";

export const diagnosticNames = {
  warnings: "Has warnings",
  failed_or_warning: "Failed or has warnings",
} satisfies Record<RequestDiagnostic, string>;

export function diagnosticFromUrl(value: string | null): RequestDiagnostic | undefined {
  return value === "warnings" || value === "failed_or_warning" ? value : undefined;
}

export function validDate(value: string) {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value) || value.startsWith("0000")) return false;
  const time = new Date(value + "T00:00:00Z");
  return Number.isFinite(time.getTime()) && time.toISOString().slice(0, 10) === value;
}

export function dateRangeError({ from, to }: HistoryFilters) {
  if (!from && !to) return "";
  if (!from || !to || !validDate(from) || !validDate(to)) return "Enter both UTC dates.";
  const days = (Date.parse(to ?? "") - Date.parse(from ?? "")) / 86_400_000;
  return days >= 1 && days <= 92 ? "" : "Choose a UTC date range of 1 to 92 days.";
}

export function shiftDate(date: string, days: number) {
  if (!validDate(date)) return "";
  const time = new Date(date + "T00:00:00Z");
  time.setUTCDate(time.getUTCDate() + days);
  return time.toISOString().slice(0, 10);
}
