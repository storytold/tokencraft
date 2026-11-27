import { useEffect, useState } from "react";
import { ApiError } from "@/lib/api";
import type { ProjectScope } from "@/lib/gateway";
import { comparisonApi, errorMessage, type Trace, type TraceSnapshot } from "@/surfaces/comparison/api";

const TRACE_READ_TIMEOUT_MS = 10_000;

type ReadRequest<T> = (scope: ProjectScope, id: string, signal: AbortSignal, validator: string | null) =>
  Promise<{ data: T | null; validator: string | null; delay: number }>;

type Observation<T> = { key: string; data?: T; error?: string; retrying?: boolean };

export function requestIsActive(status: Trace["status"] | undefined) {
  return status === "queued" || status === "running";
}

async function readStatus(scope: ProjectScope, id: string, signal: AbortSignal, validator: string | null) {
  const response = await comparisonApi.trace(scope, id, signal, validator);
  const delay = response.trace ? requestDelay(response.trace.status) : 0;
  return {
    data: response.trace,
    validator: response.validator,
    delay: Math.max(delay, response.retryAfterMs),
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
  const key = JSON.stringify([account_id, project_id, id, version]);
  const [observation, setObservation] = useState<Observation<T>>();
  const [revision, setRevision] = useState(0);
  const current = observation?.key === key ? observation : undefined;

  useEffect(() => {
    if (!id) return;
    const requestId = id;
    const controller = new AbortController();
    let timer: number | undefined;
    let reading = false;
    let stopped = false;
    let failures = 0;
    let validator: string | null = null;
    let delay = 1_000;

    function schedule(delay: number) {
      if (!document.hidden && !controller.signal.aborted) {
        timer = window.setTimeout(() => void observe(), delay);
      }
    }

    async function observe() {
      if (reading || stopped || controller.signal.aborted) return;
      reading = true;
      const deadline = AbortSignal.timeout(TRACE_READ_TIMEOUT_MS);
      const signal = AbortSignal.any([controller.signal, deadline]);
      try {
        const next = await read({ account_id, project_id }, requestId, signal, validator);
        if (controller.signal.aborted) return;
        failures = 0;
        validator = next.validator;
        if (next.data) {
          delay = next.delay;
          setObservation({ key, data: next.data });
        } else {
          setObservation((previous) => previous?.key === key && previous.error
            ? { key, data: previous.data }
            : previous);
        }
        schedule(Math.max(delay, next.delay));
      } catch (cause) {
        if (controller.signal.aborted) return;
        stopped = cause instanceof ApiError && cause.status >= 400 && cause.status < 500
          && cause.status !== 408 && cause.status !== 429;
        const error = !stopped && deadline.aborted ? "Timed out reading the request trace." : errorMessage(cause);
        setObservation((previous) => ({
          key, data: !stopped && previous?.key === key ? previous.data : undefined,
          error, retrying: !stopped,
        }));
        if (!stopped) schedule(Math.min(15_000, 1_000 * 2 ** Math.min(failures++, 4)));
      } finally {
        reading = false;
      }
    }

    function resume() {
      window.clearTimeout(timer);
      if (!document.hidden) void observe();
    }

    void observe();
    document.addEventListener("visibilitychange", resume);
    window.addEventListener("online", resume);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", resume);
      window.removeEventListener("online", resume);
    };
  }, [account_id, project_id, id, key, read, revision]);

  return {
    ...current,
    loading: Boolean(id && !current),
    refresh: () => setRevision((value) => value + 1),
    replace: (data: T) => setObservation((previous) => previous?.key === key ? { key, data } : previous),
  };
}
