import { useEffect, useState } from "react";
import { ApiError } from "@/lib/api";
import type { ProjectScope } from "@/lib/gateway";
import { comparisonApi, errorMessage, type UsageFilters, type UsageReport } from "@/surfaces/comparison/api";

const USAGE_READ_TIMEOUT_MS = 10_000;

export type UsageResult = { report: UsageReport; groups?: UsageReport["groups"]; seriesError?: string; error?: never; denied?: never }
  | { report?: never; groups?: never; seriesError?: never; error: string; denied?: boolean };

export function useUsageReport(
  scope: ProjectScope, filters: UsageFilters, after: string | null, revision: number,
  { enabled = true, allDays = false } = {},
) {
  // A response belongs to exactly one scope, set of filters, cursor, and refresh.
  const key = JSON.stringify([scope, filters, after, revision, allDays]);
  const [result, setResult] = useState<{ key: string; data: UsageResult }>();

  useEffect(() => {
    if (!enabled) return;
    const controller = new AbortController();
    const deadline = AbortSignal.timeout(USAGE_READ_TIMEOUT_MS);
    const signal = AbortSignal.any([controller.signal, deadline]);
    let firstPage: UsageReport | undefined;

    async function load() {
      const report = await comparisonApi.usage(scope, filters, after, signal);
      signal.throwIfAborted();
      const groups = [...report.groups];
      if (allDays && report.next_after) {
        // A 92-day window can span two gateway pages. Keep the report used for
        // export intact and collect the full series independently of the table.
        firstPage = report;
        setResult({ key, data: { report } });
        const visited = new Set<string>();
        for (let cursor: string | null = report.next_after; cursor; ) {
          if (visited.has(cursor)) throw new Error("Repeated usage cursor");
          visited.add(cursor);
          const page = await comparisonApi.usage(scope, filters, cursor, signal);
          groups.push(...page.groups);
          cursor = page.next_after;
        }
      }
      signal.throwIfAborted();
      setResult({ key, data: { report, groups } });
    }

    void load().catch((failure) => {
      if (controller.signal.aborted) return;
      const denied = failure instanceof ApiError && [401, 403, 404].includes(failure.status);
      const message = deadline.aborted && !(failure instanceof ApiError)
        ? "Timed out reading usage." : errorMessage(failure);
      const data = firstPage && !denied ? { report: firstPage, seriesError: message } : { error: message, denied };
      setResult({ key, data });
    });
    return () => controller.abort();
  }, [scope, filters, after, revision, key, enabled, allDays]);

  return enabled && result?.key === key ? result.data : undefined;
}
