import { useEffect, useRef, useState } from "react";
import { ApiError } from "@/lib/api";
import { scheduleDeadline } from "@/lib/deadline";
import type { ProjectScope } from "@/lib/gateway";
import { comparisonApi, errorMessage, type Trace, type TraceSnapshot } from "@/surfaces/comparison/api";

const TRACE_READ_TIMEOUT_MS = 10_000;
const monotonicNow = () => performance.now();

type ReadRequest<T> = (scope: ProjectScope, id: string, signal: AbortSignal, validator: string | null) =>
  Promise<{ data: T | null; validator: string | null; delay?: number; retryAfterMs?: number }>;

type Observation<T> = { key: string; data?: T; error?: string; retrying?: boolean };

export function requestIsActive(status: Trace["status"] | undefined) {
  return status === "queued" || status === "running";
}

async function readStatus(scope: ProjectScope, id: string, signal: AbortSignal, validator: string | null) {
  const response = await comparisonApi.trace(scope, id, signal, validator);
  return {
    data: response.trace,
    validator: response.validator,
    delay: response.trace ? requestDelay(response.trace.status) : undefined,
    retryAfterMs: response.retryAfterMs,
  };
}

async function readEvidence(scope: ProjectScope, id: string, signal: AbortSignal) {
  const data = await comparisonApi.debugTrace(scope, id, signal);
  const { status, delivery_status } = data.evidence;
  const deliveryActive = delivery_status === "pending" || delivery_status === "sending";
  return { data, validator: null, delay: deliveryActive ? 2_000 : requestDelay(status) };
}

function requestDelay(status?: Trace["status"]) {
  if (!status || requestIsActive(status)) return 1_000;
  if (status === "awaiting_reconciliation") return 5_000;
  return 15_000;
}

export function useRequestStatus(scope: ProjectScope, id: string | null) {
  return useRequestListener<Trace>(scope, id, readStatus);
}

export function useRequestEvidence(scope: ProjectScope, id: string, version?: string) {
  return useRequestListener<TraceSnapshot>(scope, id, readEvidence, version);
}

function useRequestListener<T>(
  scope: ProjectScope, id: string | null, read: ReadRequest<T>,
  version?: string,
) {
  const { account_id, project_id } = scope;
  const target = JSON.stringify([account_id, project_id, id]);
  const key = JSON.stringify([account_id, project_id, id, version]);
  const [observation, setObservation] = useState<Observation<T>>();
  const [revision, setRevision] = useState(0);
  const retry = useRef<{ target: string; until: number } | undefined>(undefined);
  const current = observation?.key === key ? observation : undefined;

  useEffect(() => {
    if (!id) return;
    const requestId = id;
    const controller = new AbortController();
    let cancelTimer: (() => void) | undefined;
    let reading = false;
    let stopped = false;
    let failures = 0;
    let validator: string | null = null;
    let delay = 1_000;

    function retryUntil() {
      return retry.current?.target === target ? retry.current.until : 0;
    }

    function rememberRetryHint(wait?: number) {
      if (wait !== undefined) {
        retry.current = { target, until: Math.min(monotonicNow() + wait, Number.MAX_SAFE_INTEGER) };
      }
    }

    function schedule(wait: number) {
      cancelTimer?.();
      if (!document.hidden && !controller.signal.aborted) {
        const due = Math.max(monotonicNow() + wait, retryUntil());
        cancelTimer = scheduleDeadline(due, () => void observe(), monotonicNow);
      }
    }

    async function observe() {
      if (reading || stopped || controller.signal.aborted || document.hidden) return;
      if (retryUntil() > monotonicNow()) {
        schedule(0);
        return;
      }
      reading = true;
      const deadline = AbortSignal.timeout(TRACE_READ_TIMEOUT_MS);
      const signal = AbortSignal.any([controller.signal, deadline]);
      try {
        const next = await read({ account_id, project_id }, requestId, signal, validator);
        signal.throwIfAborted();
        failures = 0;
        validator = next.data ? next.validator : next.validator ?? validator;
        rememberRetryHint(next.retryAfterMs);
        if (next.data) {
          delay = next.delay ?? delay;
          setObservation({ key, data: next.data });
        } else {
          setObservation((previous) => previous?.key === key && previous.retrying
            ? { key, data: previous.data }
            : previous);
        }
        schedule(delay);
      } catch (cause) {
        if (controller.signal.aborted) return;
        stopped = cause instanceof ApiError && cause.status >= 400 && cause.status < 500
          && cause.status !== 408 && cause.status !== 429;
        const error = !stopped && deadline.aborted ? "Timed out reading the request trace." : errorMessage(cause);
        setObservation((previous) => ({
          key, data: !stopped && previous?.key === key ? previous.data : undefined,
          error, retrying: !stopped,
        }));
        if (!stopped) {
          if (cause instanceof ApiError) rememberRetryHint(cause.retryAfterMs);
          schedule(Math.min(15_000, 1_000 * 2 ** Math.min(failures++, 4)));
        }
      } finally {
        reading = false;
      }
    }

    function resume() {
      cancelTimer?.();
      if (!document.hidden) void observe();
    }

    void observe();
    document.addEventListener("visibilitychange", resume);
    window.addEventListener("online", resume);
    return () => {
      controller.abort();
      cancelTimer?.();
      document.removeEventListener("visibilitychange", resume);
      window.removeEventListener("online", resume);
    };
  }, [account_id, project_id, id, key, target, read, revision]);

  return {
    ...current,
    loading: Boolean(id && !current),
    refresh: () => setRevision((value) => value + 1),
    replace: (data: T) => setObservation((previous) => previous?.key === key ? { key, data } : previous),
  };
}
