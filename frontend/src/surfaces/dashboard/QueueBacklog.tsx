import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Badge, Button, Card, Select } from "@/components/ui";
import { errorMessage, gatewayRequest, withProject } from "@/lib/gateway";
import { useOrg } from "@/lib/org";
import { useProject } from "@/surfaces/projects/ProjectScope";
import { ArtifactPublication } from "./ArtifactPublication";
import { WorkflowOperations, type WorkflowSnapshot } from "./WorkflowOperations";
import { age, providerLabels, workloadLabels } from "./operationLabels";

const labels = {
  queued: "Queued",
  running: "Running",
  awaiting_reconciliation: "Outcome unconfirmed",
  webhook_pending: "Webhook deliveries pending",
  webhook_dead: "Dead deliveries in the past 24 hours",
  export_pending: "Unpublished export events",
} as const;

type QueueKind = keyof typeof labels;
type QueueDepth = {
  kind: QueueKind;
  provider: keyof typeof providerLabels | null;
  workload: keyof typeof workloadLabels | null;
  count: number;
  dispatch_paused: boolean;
  oldest_at: string | null;
};
type Snapshot = { observed_at: string; queues: QueueDepth[]; workflows?: WorkflowSnapshot };

const requestKinds = ["queued", "running", "awaiting_reconciliation"] as const;
const deliveryKinds = ["webhook_pending", "webhook_dead", "export_pending"] as const;
const time = new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "medium" });

export function QueueBacklog({ accountId, initialProjectId, revision }: { accountId: string; initialProjectId?: string; revision: number }) {
  const { projects, loading, error } = useProject();
  const [projectId, setProjectId] = useState(initialProjectId ?? "");
  const selected = projects.find((project) => project.id === projectId);

  return (
    <section aria-labelledby="queue-backlog-heading">
      <Card className="min-w-0 space-y-4 p-4 sm:p-6">
        <div className="flex flex-wrap items-end justify-between gap-3">
          <h2 id="queue-backlog-heading" className="font-display text-2xl font-semibold">Queue backlog</h2>
          <label className="block w-full max-w-xs space-y-1 text-sm font-medium">
            Backlog scope
            <Select value={selected?.id ?? ""} disabled={loading} onChange={(event) => setProjectId(event.target.value)}>
              <option value="">All projects</option>
              {projects.map((project) => <option key={project.id} value={project.id}>{project.name}</option>)}
            </Select>
          </label>
        </div>
        {error && <p role="alert" className="text-sm text-muted">{error}</p>}
        {loading ? <p role="status" className="text-sm text-muted">Loading projects…</p> : (
          <BacklogSnapshot key={`${selected?.id ?? "all"}:${revision}`} accountId={accountId} projectId={selected?.id} />
        )}
      </Card>
    </section>
  );
}

function BacklogSnapshot({ accountId, projectId }: { accountId: string; projectId?: string }) {
  const { withScope } = useOrg();
  const [snapshot, setSnapshot] = useState<Snapshot>();
  const [error, setError] = useState("");
  const [pending, setPending] = useState(true);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    gatewayRequest<Snapshot>(withProject("/v1/operations/queues", projectId, accountId), { signal: controller.signal })
      .then((value) => {
        if (!controller.signal.aborted) setSnapshot(value);
      }).catch((failure: unknown) => {
        if (!controller.signal.aborted) {
          setSnapshot(undefined);
          setError(errorMessage(failure));
        }
      }).finally(() => {
        if (!controller.signal.aborted) setPending(false);
      });
    return () => controller.abort();
  }, [accountId, projectId, revision]);

  const requests = snapshot?.queues.filter((queue) => queue.provider !== null) ?? [];

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p role="status" className="text-sm text-muted">
          {pending ? "Checking queue backlog…" : snapshot
            ? <>Updated <time dateTime={snapshot.observed_at}>{time.format(new Date(snapshot.observed_at))}</time></>
            : "Backlog snapshot unavailable"}
        </p>
        <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => {
          setPending(true);
          setError("");
          setRevision((previous) => previous + 1);
        }}>Refresh backlog</Button>
      </div>
      {error && <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{error}</p>}
      {snapshot && (
        <>
          <dl className="grid gap-3 sm:grid-cols-3" aria-label="Request backlog">
            {requestKinds.map((kind) => <Depth key={kind} kind={kind} snapshot={snapshot} />)}
          </dl>
          <dl className="grid gap-3 sm:grid-cols-3" aria-label="Delivery and export backlog">
            {deliveryKinds.map((kind) => <Depth key={kind} kind={kind} snapshot={snapshot} />)}
          </dl>
          {requests.length > 0 && (
            <details className="rounded-lg border border-border">
              <summary className="cursor-pointer px-3 py-2 text-sm font-medium">By provider and workload</summary>
              <ul className="divide-y divide-border border-t border-border">
                {requests.map(({ kind, provider, workload, count, oldest_at, dispatch_paused }) => (
                  <li key={`${kind}:${provider}:${workload}`} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-sm">
                    <span className="font-medium">{providerLabels[provider ?? "other"]} · {workloadLabels[workload ?? "other"]}</span>
                    <Badge tone={kind === "awaiting_reconciliation" ? "amber" : "muted"}>{labels[kind]}</Badge>
                    <span>{count.toLocaleString()}</span>
                    {dispatch_paused && <Badge tone="amber">Paused</Badge>}
                    {oldest_at && <span className="text-muted">Oldest {age(oldest_at, snapshot.observed_at)}</span>}
                  </li>
                ))}
              </ul>
            </details>
          )}
          {snapshot.workflows && <WorkflowOperations snapshot={snapshot.workflows} observedAt={snapshot.observed_at} />}
          {projectId && (
            <div className="flex flex-wrap gap-4 text-sm font-semibold text-brand">
              <Link className="hover:underline" to={withProject(withScope("/requests"), projectId)}>Open project requests</Link>
              <Link className="hover:underline" to={withProject(withScope("/requests/exports"), projectId)}>Open project exports</Link>
              {snapshot.workflows && <Link className="hover:underline" to={withProject(withScope("/workflows"), projectId)}>Open project workflows</Link>}
            </div>
          )}
        </>
      )}
      <ArtifactPublication key={revision} accountId={accountId} projectId={projectId} />
    </div>
  );
}

function Depth({ kind, snapshot }: { kind: QueueKind; snapshot: Snapshot }) {
  const rows = snapshot.queues.filter((queue) => queue.kind === kind);
  const count = rows.reduce((total, row) => total + row.count, 0);
  const paused = rows.reduce((total, row) => total + (row.dispatch_paused ? row.count : 0), 0);
  const oldest = rows.reduce<string | null>((current, row) => {
    if (row.oldest_at && (!current || Date.parse(row.oldest_at) < Date.parse(current))) return row.oldest_at;
    return current;
  }, null);
  return (
    <div className="rounded-lg border border-border px-3 py-2">
      <dt className="text-sm text-muted">{labels[kind]}</dt>
      <dd className="font-display text-2xl font-semibold tabular-nums">{count.toLocaleString()}</dd>
      {paused > 0 && <dd className="text-xs font-medium text-muted">{paused.toLocaleString()} paused</dd>}
      {oldest && <dd className="text-xs text-muted">Oldest {age(oldest, snapshot.observed_at)}</dd>}
    </div>
  );
}
