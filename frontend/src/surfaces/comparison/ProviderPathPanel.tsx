import { Route, Server } from "lucide-react";
import {
  providerLabel, type ComputeTargetAttempt, type EvidenceStep, type EvidenceTone,
  type ProviderAttemptState, type ProviderPath,
} from "./api";

const states: Record<ProviderAttemptState, { label: string; tone: EvidenceTone }> = {
  rate_limited: { label: "Rate limited before acceptance", tone: "warning" },
  queued: { label: "Queued", tone: "active" },
  dispatching: { label: "Dispatch started", tone: "active" },
  acknowledged: { label: "Acknowledged by provider", tone: "active" },
  succeeded: { label: "Succeeded", tone: "ok" },
  failed: { label: "Failed", tone: "error" },
  canceled: { label: "Canceled", tone: "neutral" },
  outcome_unconfirmed: { label: "Outcome unconfirmed", tone: "warning" },
};

type TraceSelection = {
  steps: EvidenceStep[];
  selected: string;
  onSelect: (id: string) => void;
};

export function ProviderPathPanel({ path, ...selection }: TraceSelection & { path: ProviderPath }) {
  if (!path.attempts.length && !path.downstream) return null;

  return (
    <section aria-label="Execution path" className="min-w-0 space-y-2 border-b border-border px-4 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h4 className="flex items-center gap-2 text-sm font-bold"><Route className="h-4 w-4 text-brand" aria-hidden /> Execution path</h4>
        {path.attempts.length > 1 && path.admitted_offer_id && (
          <p className="min-w-0 break-words text-xs text-muted">Admitted offer <code className="break-all">{path.admitted_offer_id}</code></p>
        )}
      </div>
      <ol className="grid min-w-0 gap-2 sm:grid-cols-2">
        {path.attempts.map((attempt) => {
          const label = attempt.provider ? providerLabel(attempt.provider) : "Provider offer";
          const state = states[attempt.state];
          const targets = attempt.compute_attempts ?? [];
          return (
            <li key={attempt.step_id} className={`min-w-0 space-y-2 rounded-md border border-border bg-surface2/50 p-2.5 ${targets.length ? "sm:col-span-2" : ""}`}>
              <div className="flex flex-wrap items-start justify-between gap-x-2 gap-y-1">
                <button type="button" onClick={() => selection.onSelect(attempt.step_id)}
                  aria-label={`View evidence for ${attempt.offer_id}`}
                  aria-pressed={selection.selected === attempt.step_id}
                  className="min-w-0 text-left text-xs font-semibold hover:text-brand focus-visible:outline-brand">
                  {label}{attempt.adapter === "mock" && " fixture"} <code className="break-all font-normal text-muted">{attempt.offer_id}</code>
                </button>
                <span className="tc-debug-tone text-xs font-medium" data-tone={state.tone}>{state.label}</span>
              </div>
              <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-1 text-xs">
                {attempt.requested_model && <><dt className="text-muted">Requested model</dt><dd className="break-all font-mono">{attempt.requested_model}</dd></>}
                {attempt.reported_model && attempt.reported_model !== attempt.requested_model && (
                  <><dt className="text-muted">Reported model</dt><dd className="break-all font-mono">{attempt.reported_model}</dd></>
                )}
                {attempt.correlation_id && <><dt className="text-muted">{attempt.adapter === "mock" ? "Fixture job ID" : "Upstream ID"}</dt><dd className="break-all font-mono">{attempt.correlation_id}</dd></>}
                {attempt.adapter && attempt.adapter !== "mock" && !targets.length && (
                  <><dt className="text-muted">Compute placement</dt><dd className="text-muted">Not recorded</dd></>
                )}
              </dl>
              {targets.length > 0 && <ComputeAttempts attempts={targets} {...selection} />}
            </li>
          );
        })}
      </ol>
      {path.downstream && (
        <dl className="flex flex-wrap items-baseline gap-x-3 gap-y-1 rounded-md border border-dashed border-border px-2.5 py-2 text-xs">
          <dt className="font-medium">Inference provider</dt>
          <dd className="min-w-0 break-words text-muted">
            {path.downstream.status === "reported" ? (
              <><span className="font-medium text-fg">{path.downstream.provider}</span> · Reported by upstream</>
            ) : path.downstream.status === "pending" ? "Awaiting upstream response" : "Undisclosed"}
          </dd>
        </dl>
      )}
    </section>
  );
}

function ComputeAttempts({ attempts, steps, selected, onSelect }: TraceSelection & { attempts: ComputeTargetAttempt[] }) {
  const { home_cell, stack } = attempts[0].selection;
  return (
    <div className="space-y-2 border-t border-border pt-2">
      <h5 className="flex items-center gap-2 text-xs font-semibold"><Server className="h-3.5 w-3.5 text-brand" aria-hidden /> Compute attempts</h5>
      <dl className="flex flex-wrap gap-x-4 gap-y-1 text-xs">
        <div className="flex min-w-0 gap-1.5"><dt className="shrink-0 text-muted">Home cell</dt><dd className="break-all font-mono">{home_cell}</dd></div>
        <div className="flex min-w-0 gap-1.5"><dt className="shrink-0 text-muted">Compute stack</dt><dd className="break-all font-mono">{stack}</dd></div>
      </dl>
      <ol aria-label="Compute attempts in dispatch order" className="grid gap-1.5 sm:grid-cols-2">
        {attempts.map(({ step_id, selection }) => {
          const step = steps.find(({ id }) => id === step_id);
          if (!step) return null;
          return (
            <li key={step_id} className="min-w-0">
              <button type="button" onClick={() => onSelect(step_id)} aria-pressed={selected === step_id}
                className="w-full space-y-1.5 rounded-md border border-border bg-surface px-2.5 py-2 text-left text-xs hover:border-brand focus-visible:outline-brand aria-pressed:border-brand">
                <span className="flex flex-wrap items-start justify-between gap-x-2 gap-y-1">
                  <span className="min-w-0 font-semibold"><span className="text-muted">{step.name}</span> <code className="break-all">{selection.target}</code></span>
                  <span className="tc-debug-tone font-medium" data-tone={step.tone}>{step.status}</span>
                </span>
                <span className="block break-words text-muted">
                  <span className="sr-only">Cloud </span>{selection.cloud} · <span className="sr-only">Execution region </span>{selection.region} · <span className="sr-only">Zone </span>{selection.zone}
                </span>
              </button>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
