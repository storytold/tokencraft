import { useEffect, useRef, useState } from "react";
import { ApiError } from "@/lib/api";
import type { ProjectScope } from "@/lib/gateway";
import { comparisonApi, errorMessage, type ModelQuote } from "./api";
import { prepareModelSubmission, quoteExpired, type ModelSubmission, type ModelSubmissionOptions } from "./modelSubmission";

const RECEIPT_TIMEOUT_MS = 15_000;

type SubmissionAttempt = { wire: ModelSubmission; quote: ModelQuote };

function outcomeUnknown(cause: unknown) {
  return !(cause instanceof ApiError) || cause.status === 0 || cause.status === 408 || cause.status >= 500;
}

export function useModelSubmission(scope: ProjectScope, onAccepted: (id: string, recovered: boolean) => void) {
  const attempt = useRef<SubmissionAttempt | null>(null);
  const active = useRef<AbortController | null>(null);
  const [sending, setSending] = useState(false);
  const [recoverable, setRecoverable] = useState(false);
  const [error, setError] = useState("");
  const [staleQuote, setStaleQuote] = useState<ModelQuote>();

  useEffect(() => () => active.current?.abort(), []);

  async function send(submission: SubmissionAttempt, recovering: boolean) {
    const controller = new AbortController();
    const deadline = AbortSignal.timeout(RECEIPT_TIMEOUT_MS);
    active.current = controller;
    setSending(true);
    setError("");
    try {
      const id = await comparisonApi.run(submission.wire, AbortSignal.any([controller.signal, deadline]));
      if (controller.signal.aborted) return;
      attempt.current = null;
      setRecoverable(false);
      onAccepted(id, recovering);
    } catch (cause) {
      if (controller.signal.aborted) return;
      // Admission checks for an existing receipt before rejecting a stale quote.
      const quoteRejected = cause instanceof ApiError && cause.status === 409 && cause.code === "quote_stale";
      const retain = outcomeUnknown(cause) || (recovering && !quoteRejected);
      if (!retain) attempt.current = null;
      setRecoverable(retain);
      if (quoteRejected) {
        setStaleQuote(submission.quote);
        if (recovering) setError("The gateway rejected the original estimate. Use a fresh estimate for a new request.");
      } else {
        setError(deadline.aborted ? "Timed out waiting for the request receipt." : errorMessage(cause));
      }
    } finally {
      active.current = null;
      if (!controller.signal.aborted) setSending(false);
    }
  }

  function submit(quote: ModelQuote, options: ModelSubmissionOptions) {
    if (attempt.current || active.current || staleQuote === quote) return;
    if (quoteExpired(quote.expires_at)) {
      setError("Estimate expired. Refresh for a new request.");
      return;
    }
    const submission = { wire: prepareModelSubmission(scope, quote, options), quote };
    attempt.current = submission;
    void send(submission, false);
  }

  function retry() {
    if (attempt.current && !active.current) void send(attempt.current, true);
  }

  function dismiss() {
    if (active.current) return;
    attempt.current = null;
    setRecoverable(false);
    setError("");
  }

  return { sending, recoverable, error, staleQuote, submit, retry, dismiss };
}
