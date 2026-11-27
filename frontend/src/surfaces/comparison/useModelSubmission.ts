import { useEffect, useRef, useState } from "react";
import { ApiError } from "@/lib/api";
import type { ProjectScope } from "@/lib/gateway";
import { comparisonApi, errorMessage, type ModelQuote } from "./api";
import { prepareModelSubmission, type ModelSubmission, type ModelSubmissionOptions } from "./modelSubmission";
import { quoteExpired } from "./QuoteExpiry";

const RECEIPT_TIMEOUT_MS = 15_000;

function outcomeUnknown(cause: unknown) {
  return !(cause instanceof ApiError) || cause.status === 0 || cause.status === 408 || cause.status >= 500;
}

export function useModelSubmission(scope: ProjectScope, onAccepted: (id: string, recovered: boolean) => void) {
  const attempt = useRef<ModelSubmission | null>(null);
  const active = useRef<AbortController | null>(null);
  const [sending, setSending] = useState(false);
  const [recoverable, setRecoverable] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => () => active.current?.abort(), []);

  async function send(submission: ModelSubmission, recovering: boolean) {
    const controller = new AbortController();
    const deadline = AbortSignal.timeout(RECEIPT_TIMEOUT_MS);
    active.current = controller;
    setSending(true);
    setError("");
    try {
      const id = await comparisonApi.run(submission, AbortSignal.any([controller.signal, deadline]));
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
      setError(deadline.aborted ? "Timed out waiting for the request receipt." : errorMessage(cause));
    } finally {
      active.current = null;
      if (!controller.signal.aborted) setSending(false);
    }
  }

  function submit(quote: ModelQuote, options: ModelSubmissionOptions) {
    if (attempt.current || active.current) return;
    if (quoteExpired(quote.expires_at)) {
      setError("Estimate expired. Refresh for a new request.");
      return;
    }
    const submission = prepareModelSubmission(scope, quote, options);
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

  return { sending, recoverable, error, submit, retry, dismiss };
}
