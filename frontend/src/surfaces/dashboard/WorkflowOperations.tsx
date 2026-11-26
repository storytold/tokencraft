import { Badge } from "@/components/ui";
import { age, latency } from "./operationLabels";

const states = [
  ["queued", "Queued"],
  ["running", "Running"],
  ["reconciling", "Reconciling"],
] as const;

const outcomes = [
  ["succeeded", "Succeeded"],
  ["failed", "Failed"],
  ["canceled", "Canceled"],
  ["expired", "Expired"],
] as const;

type Depth = { count: number; oldest_at: string | null };

export type WorkflowSnapshot = {
  queued: Depth;
  running: Depth;
  reconciling: Depth;
  due_queued: number;
  overdue: number;
  outcomes: {
    sampled: number;
    sample_limit: number;
    succeeded: number;
    failed: number;
    canceled: number;
    expired: number;
    timing_samples: number;
    p95_succeeded_ms: number | null;
  };
};

export function WorkflowOperations({ snapshot, observedAt }: { snapshot: WorkflowSnapshot; observedAt: string }) {
  const recent = snapshot.outcomes;
  return (
    <section aria-labelledby="workflow-operations-heading" className="space-y-3 border-t border-border pt-4">
      <div className="flex flex-wrap items-center gap-2">
        <h3 id="workflow-operations-heading" className="text-lg font-semibold">Workflow runs</h3>
        {snapshot.overdue > 0 && <Badge tone="amber">{snapshot.overdue.toLocaleString()} past deadline</Badge>}
      </div>
      <dl className="grid gap-3 sm:grid-cols-3" aria-label="Active workflow runs">
        {states.map(([state, label]) => {
          const depth = snapshot[state];
          return (
            <div key={state} className="rounded-lg border border-border px-3 py-2">
              <dt className="text-sm text-muted">{label}</dt>
              <dd className="font-display text-2xl font-semibold tabular-nums">{depth.count.toLocaleString()}</dd>
              {state === "queued" && <dd className="text-xs text-muted">{snapshot.due_queued.toLocaleString()} ready to claim</dd>}
              {depth.oldest_at && <dd className="text-xs text-muted">Oldest admitted {age(depth.oldest_at, observedAt)} ago</dd>}
            </div>
          );
        })}
      </dl>
      <h4 className="text-sm font-semibold">Run outcomes in the past 24 hours</h4>
      {recent.sampled === 0 ? <p className="text-sm text-muted">No runs ended in this window.</p> : (
        <>
          {recent.sampled >= recent.sample_limit && <p className="text-sm text-muted">
            Includes the latest {recent.sample_limit.toLocaleString()} finished runs in this scope.
          </p>}
          <dl className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-5" aria-label="Recent workflow outcomes">
            {outcomes.map(([state, label]) => (
              <div key={state}>
                <dt className="text-sm text-muted">{label}</dt>
                <dd className="font-semibold tabular-nums">{recent[state].toLocaleString()}</dd>
              </div>
            ))}
            {recent.timing_samples > 0 && (
              <div className="col-span-2 sm:col-span-1">
                <dt className="text-sm text-muted">Successful run p95</dt>
                <dd className="font-semibold tabular-nums">{latency(recent.p95_succeeded_ms)}</dd>
              </div>
            )}
          </dl>
        </>
      )}
    </section>
  );
}
