import { useCallback, useEffect, useState } from "react";
import { ApiError } from "@/lib/api";
import { comparisonApi, errorMessage, type HistoryFilters, type TracePage } from "@/surfaces/comparison/api";
import type { ProjectScope } from "@/lib/gateway";

const HISTORY_READ_TIMEOUT_MS = 10_000;

type HistoryScope = { scope: ProjectScope; filters: HistoryFilters };
type Position = HistoryScope & { cursors: string[] };
type HistoryRead = HistoryScope & {
  after: string | null;
  revision: number;
  page?: TracePage;
  error: string;
};

export function useRequestHistory(scope: ProjectScope, filters: HistoryFilters) {
  const [position, setPosition] = useState<Position>();
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<HistoryRead>();
  const refresh = useCallback(() => setRevision((version) => version + 1), []);
  const cursors = position?.scope === scope && position.filters === filters ? position.cursors : [];
  const after = cursors.at(-1) ?? null;
  const current = result?.scope === scope && result.filters === filters && result.after === after ? result : undefined;
  const latest = current?.revision === revision ? current : undefined;
  const loading = latest === undefined;

  useEffect(() => {
    const controller = new AbortController();
    const deadline = AbortSignal.timeout(HISTORY_READ_TIMEOUT_MS);
    const signal = AbortSignal.any([controller.signal, deadline]);
    const read = { scope, filters, after, revision };
    void comparisonApi.history(scope, filters, after, signal).then((page) => {
      if (controller.signal.aborted) return;
      signal.throwIfAborted();
      if (filters.diagnostic && page.diagnostic !== filters.diagnostic) {
        setResult({ ...read, error: "This gateway does not support warning filters." });
        return;
      }
      setResult({ ...read, page, error: "" });
    }).catch((cause) => {
      if (controller.signal.aborted) return;
      let error = deadline.aborted && !(cause instanceof ApiError)
        ? "Timed out reading request history." : errorMessage(cause);
      if (filters.diagnostic && cause instanceof ApiError
          && cause.status === 400 && cause.code === "request_failed") {
        error = "The gateway rejected the warning filters. Check the filter values and gateway version.";
      }
      setResult({ ...read, error });
    });
    return () => controller.abort();
  }, [scope, filters, after, revision]);

  const nextAfter = current?.page?.next_after;
  return {
    history: current?.page,
    rows: current?.page?.requests ?? [],
    error: latest?.error ?? "",
    loading,
    page: cursors.length + 1,
    hasOlder: Boolean(nextAfter) && !loading,
    hasNewer: cursors.length > 0 && !loading,
    older() {
      if (nextAfter && !loading) setPosition({ scope, filters, cursors: [...cursors, nextAfter] });
    },
    newer() {
      if (cursors.length > 0 && !loading) setPosition({ scope, filters, cursors: cursors.slice(0, -1) });
    },
    newest() { setPosition(undefined); },
    refresh,
  };
}
