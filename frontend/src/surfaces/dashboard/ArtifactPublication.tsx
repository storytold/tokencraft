import { useEffect, useState } from "react";
import { Badge } from "@/components/ui";
import { errorMessage, gatewayRequest, withProject } from "@/lib/gateway";
import { age, latency, providerLabels, workloadLabels } from "./operationLabels";

type PublicationTiming = {
  observed: number;
  published: number;
  pending: number;
  unconfirmed: number;
  timing_samples: number;
  p95_ms: number | null;
  oldest_pending_at: string | null;
};

type PublicationReport = {
  observed_at: string;
  sampled_admissions: number;
  sample_limit: number;
  totals: PublicationTiming;
  groups: { provider: keyof typeof providerLabels; workload: keyof typeof workloadLabels; timing: PublicationTiming }[];
};

export function ArtifactPublication({ accountId, projectId }: { accountId: string; projectId?: string }) {
  const [report, setReport] = useState<PublicationReport>();
  const [error, setError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    const path = withProject("/v1/operations/artifact-publication", projectId, accountId);
    gatewayRequest<PublicationReport>(path, { signal: controller.signal }).then((value) => {
      if (!controller.signal.aborted) setReport(value);
    }).catch((failure: unknown) => {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    });
    return () => controller.abort();
  }, [accountId, projectId]);

  const measured = report?.totals.timing_samples ?? 0;
  return (
    <section aria-labelledby="artifact-publication-heading" className="space-y-3 border-t border-border pt-4">
      <h3 id="artifact-publication-heading" className="text-lg font-semibold">Artifact publication</h3>
      <p className="text-sm text-muted">
        For fal and Kinovi jobs admitted in the past 24 hours. Publication time starts when the gateway observes provider completion.
      </p>
      {error && <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{error}</p>}
      {!report && !error && <p role="status" className="text-sm text-muted">Reading publication timing…</p>}
      {report && (
        <>
          {report.sampled_admissions >= report.sample_limit && <p className="text-sm text-muted">
            Includes the latest {report.sample_limit.toLocaleString()} queue admissions in this scope.
          </p>}
          {report.totals.observed === 0 ? <p className="text-sm text-muted">No provider completions observed in this window.</p> : (
            <>
              <dl className="grid gap-3 sm:grid-cols-3">
                <div className="rounded-lg border border-border px-3 py-2">
                  <dt className="text-sm text-muted">95th percentile to publish</dt>
                  <dd className="font-display text-2xl font-semibold tabular-nums">{latency(report.totals.p95_ms)}</dd>
                  <dd className="text-xs text-muted">{measured.toLocaleString()} {measured === 1 ? "publication" : "publications"} measured</dd>
                </div>
                <div className="rounded-lg border border-border px-3 py-2">
                  <dt className="text-sm text-muted">Awaiting publication</dt>
                  <dd className="font-display text-2xl font-semibold tabular-nums">{report.totals.pending.toLocaleString()}</dd>
                  {report.totals.oldest_pending_at && <dd className="text-xs text-muted">
                    Oldest {age(report.totals.oldest_pending_at, report.observed_at)}
                  </dd>}
                </div>
                <div className="rounded-lg border border-border px-3 py-2">
                  <dt className="text-sm text-muted">Publication unconfirmed</dt>
                  <dd className="font-display text-2xl font-semibold tabular-nums">{report.totals.unconfirmed.toLocaleString()}</dd>
                </div>
              </dl>
              <details className="rounded-lg border border-border">
                <summary className="cursor-pointer px-3 py-2 text-sm font-medium">Publication by provider and workload</summary>
                <ul className="divide-y divide-border border-t border-border">
                  {report.groups.map(({ provider, workload, timing }) => (
                    <li key={`${provider}:${workload}`} className="flex flex-wrap gap-x-3 gap-y-1 px-3 py-2 text-sm">
                      <span className="font-medium">{providerLabels[provider]} · {workloadLabels[workload]}</span>
                      <span>{latency(timing.p95_ms)} at p95</span>
                      <span className="text-muted">{timing.timing_samples.toLocaleString()} measured</span>
                      {timing.pending > 0 && <Badge tone="muted">{timing.pending.toLocaleString()} awaiting</Badge>}
                      {timing.unconfirmed > 0 && <Badge tone="amber">{timing.unconfirmed.toLocaleString()} unconfirmed</Badge>}
                    </li>
                  ))}
                </ul>
              </details>
            </>
          )}
        </>
      )}
    </section>
  );
}
