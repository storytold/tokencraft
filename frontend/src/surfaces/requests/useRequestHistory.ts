import { useCallback, useEffect, useState } from "react";
import { ApiError } from "@/lib/api";
import { comparisonApi, errorMessage, type HistoryFilters, type TracePage } from "@/surfaces/comparison/api";
import type { ProjectScope } from "@/lib/gateway";

const HISTORY_READ_TIMEOUT_MS = 10_000;
const FIRST_PAGE: readonly string[] = [];

type HistoryScope = { scope: ProjectScope; filters: HistoryFilters };
type Position = HistoryScope & { cursors: readonly string[] };
type LoadedPage = { data: TracePage; cursors: readonly string[] };
type HistoryRead = Position & {
  revision: number;
  page?: LoadedPage;
  error: string;
};

export function useRequestHistory(scope: ProjectScope, filters: HistoryFilters) {
  const [position, setPosition] = useState<Position>();
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<HistoryRead>();
  const refresh = useCallback(() => setRevision((version) => version + 1), []);
  const cursors = position?.scope === scope && position.filters === filters ? position.cursors : FIRST_PAGE;
  const after = cursors.at(-1) ?? null;
  const current = result?.scope === scope && result.filters === filters ? result : undefined;
  const latest = current?.cursors === cursors && current.revision === revision ? current : undefined;
  const loading = latest === undefined;

  useEffect(() => {
    const controller = new AbortController();
    const deadline = AbortSignal.timeout(HISTORY_READ_TIMEOUT_MS);
    const signal = AbortSignal.any([controller.signal, deadline]);
    const read = { scope, filters, cursors, revision };
    void comparisonApi.history(scope, filters, after, signal).then((page) => {
      if (controller.signal.aborted) return;
      signal.throwIfAborted();
      if (filters.diagnostic && page.diagnostic !== filters.diagnostic) {
        setResult({ ...read, error: "This gateway does not support warning filters." });
        return;
      }
      setResult({ ...read, page: { data: page, cursors }, error: "" });
    }).catch((cause) => {
      if (controller.signal.aborted) return;
      const timedOut = deadline.aborted && !(cause instanceof ApiError);
      const status = cause instanceof ApiError ? cause.status : undefined;
      const transient = timedOut || status === 0 || status === 408 || status === 429
        || (status !== undefined && status >= 500);
      let error = timedOut ? "Timed out reading request history." : errorMessage(cause);
      if (filters.diagnostic && cause instanceof ApiError
          && cause.status === 400 && cause.code === "request_failed") {
        error = "The gateway rejected the warning filters. Check the filter values and gateway version.";
      }
      setResult((previous) => ({
        ...read, error,
        page: transient && previous?.scope === scope && previous.filters === filters ? previous.page : undefined,
      }));
    });
    return () => controller.abort();
  }, [scope, filters, cursors, after, revision]);

  function navigate(next: readonly string[]) {
    setPosition(next.length ? { scope, filters, cursors: next } : undefined);
    refresh();
  }

  const history = current?.page?.data;
  const displayedCursors = current?.page?.cursors ?? cursors;
  const nextAfter = history?.next_after;
  return {
    history,
    rows: history?.requests ?? [],
    error: latest?.error ?? "",
    loading,
    page: displayedCursors.length + 1,
    hasOlder: Boolean(nextAfter) && !loading,
    hasNewer: displayedCursors.length > 0 && !loading,
    older() {
      if (nextAfter && !loading) navigate([...displayedCursors, nextAfter]);
    },
    newer() {
      if (displayedCursors.length > 0 && !loading) navigate(displayedCursors.slice(0, -1));
    },
    newest() { navigate(FIRST_PAGE); },
    refresh,
  };
}
