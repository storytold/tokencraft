import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { ArrowUpRight, Download, RefreshCw, Route } from "lucide-react";
import { Button, Card } from "@/components/ui";
import { withProject, type ProjectScope } from "@/lib/gateway";
import { ProjectStatus, useProject } from "@/surfaces/projects/ProjectScope";
import { comparisonApi, providerChoices, type Catalog, type UsageGroupBy, type UsageReport } from "@/surfaces/comparison/api";
import { SupplierSettlements } from "./SupplierSettlements";
import { usageRequestsLink } from "./usageRequestsLink";
import { UsageBreakdown } from "./usage/UsageBreakdown";
import { UsageControls } from "./usage/UsageControls";
import { UsageModels } from "./usage/UsageModels";
import { UsageCharges, UsageSummary } from "./usage/UsageSummary";
import { UsageTrend, type Interval } from "./usage/UsageTrend";
import { recentWindow, type MetricId, type UsageSelection } from "./usage/usageData";
import { useUsageReport } from "./usage/useUsageReport";

const linkClass = "inline-flex min-h-9 items-center gap-1.5 rounded-lg border border-border bg-surface px-3 text-sm font-semibold hover:border-brand/60 focus-visible:outline-brand";
const refreshedAt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short", timeZone: "UTC" });

export default function Usage() {
  const { scope, project } = useProject();
  if (!scope || !project) return <ProjectStatus />;
  return <UsageWorkspace key={JSON.stringify([scope, project.client_ref, project.role])} scope={scope}
    projectName={project.is_default ? "Default project" : project.name} boundClient={project.client_ref}
    canSettle={project.role === "admin"} canReadRequests={project.role !== "billing_viewer"} />;
}

function UsageWorkspace({ scope, projectName, boundClient, canSettle, canReadRequests }: {
  scope: ProjectScope; projectName: string; boundClient: string | null; canSettle: boolean; canReadRequests: boolean;
}) {
  const [query, setQuery] = useState<UsageSelection>(() => ({
    ...recentWindow(7), provider: "", model: "", client_ref: "", app_ref: "",
  }));
  const [groupBy, setGroupBy] = useState<UsageGroupBy>("model");
  const [cursors, setCursors] = useState<string[]>([]);
  const [metric, setMetric] = useState<MetricId>("requests");
  const [interval, setInterval] = useState<Interval>("auto");
  const [revision, setRevision] = useState(0);
  const [offers, setOffers] = useState<Catalog["offers"]>([]);
  const after = cursors.at(-1) ?? null;
  const { from, to, provider, client_ref, app_ref } = query;

  useEffect(() => {
    const controller = new AbortController();
    comparisonApi.catalog(controller.signal).then(({ offers }) => {
      if (!controller.signal.aborted) setOffers(offers);
    }).catch(() => {
      // Historical models and manual IDs remain available if the catalog is down.
    });
    return () => controller.abort();
  }, []);

  const dayFilters = useMemo(() => ({ ...query, group_by: "day" as const }), [query]);
  const modelFilters = useMemo(() => ({
    from, to, provider, client_ref, app_ref, model: "", group_by: "model" as const,
  }), [from, to, provider, client_ref, app_ref]);
  const groupFilters = useMemo(() => ({ ...query, group_by: groupBy }), [query, groupBy]);
  const modelFirstPage = groupBy === "model" && !query.model && !after;
  const dayFirstPage = groupBy === "day" && !after;

  const timeline = useUsageReport(scope, dayFilters, null, revision, { allDays: true });
  const modelActivity = useUsageReport(scope, modelFilters, null, revision);
  const groupPage = useUsageReport(scope, groupFilters, after, revision, { enabled: !modelFirstPage && !dayFirstPage });
  const breakdown = modelFirstPage ? modelActivity : dayFirstPage ? timeline : groupPage;
  const report = timeline?.report;
  const chartError = timeline?.error ?? timeline?.seriesError;
  const current = report ?? breakdown?.report;
  const modelChoices = new Set(offers.map((offer) => offer.model));
  const providers = providerChoices([
    query.provider,
    ...offers.map((offer) => offer.provider),
    ...(breakdown?.report?.group_by === "provider" ? breakdown.report.groups.map((row) => row.key) : []),
  ]);
  for (const row of modelActivity?.report?.groups ?? []) if (row.key) modelChoices.add(row.key);
  if (breakdown?.report?.group_by === "model") {
    for (const row of breakdown.report.groups) if (row.key) modelChoices.add(row.key);
  }
  if (query.model) modelChoices.add(query.model);

  function changeFilters(next: UsageSelection) {
    setCursors([]);
    setQuery(next);
  }

  function refresh() {
    setCursors([]);
    setRevision((value) => value + 1);
  }

  return (
    <div className="mx-auto min-w-0 max-w-7xl space-y-5">
      <header className="flex flex-wrap items-end justify-between gap-4 border-b border-border pb-5">
        <div className="min-w-0">
          <h1 className="font-display text-4xl font-extrabold tracking-[-0.045em] sm:text-5xl">Usage</h1>
          <p className="mt-2 max-w-lg break-words text-sm leading-6 text-muted sm:text-base">
            Track requests, outputs, and costs for <span className="font-medium text-fg">{projectName}</span>.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {canReadRequests && current && <Link className={linkClass} to={usageRequestsLink(scope, current)} aria-label="Matching requests">
            <span className="sm:hidden">Requests</span><span className="hidden sm:inline">Matching requests</span>
            <ArrowUpRight className="h-4 w-4" aria-hidden />
          </Link>}
          <Button type="button" variant="outline" className="h-9 rounded-lg px-3" aria-label="Download JSON page" disabled={!breakdown?.report}
            onClick={() => { if (breakdown?.report) downloadReport(breakdown.report); }}>
            <Download className="h-4 w-4" aria-hidden /><span className="sm:hidden">JSON</span><span className="hidden sm:inline">Download JSON page</span>
          </Button>
          <Button type="button" variant="outline" className="h-9 w-9 rounded-lg px-0" aria-label="Refresh usage" title="Refresh usage" onClick={refresh}>
            <RefreshCw className="h-4 w-4" aria-hidden />
          </Button>
        </div>
      </header>

      <UsageControls query={query} boundClient={boundClient} models={[...modelChoices].sort()} providers={providers} onChange={changeFilters} />

      {current && <UsageSummary report={current} />}
      <div className="grid min-w-0 items-start gap-4 xl:grid-cols-[minmax(0,2.6fr)_minmax(15rem,1fr)]">
        {report && timeline.groups ? (
          <UsageTrend report={report} groups={timeline.groups} metric={metric} interval={interval} onMetric={setMetric} onInterval={setInterval}
            requestLink={canReadRequests ? (window) => usageRequestsLink(scope, { ...report, window }) : undefined} />
        ) : (
          <Card className="grid min-h-64 place-items-center p-5 text-center">
            {chartError ? <div role="alert">
              <p className="font-semibold">Usage chart unavailable</p>
              <p className="mt-1 text-sm text-muted">{chartError}</p>
              <Button type="button" size="sm" variant="outline" className="mt-3 rounded-md" onClick={refresh}>Retry usage</Button>
            </div> : <p className="text-sm text-muted" role="status">Loading usage chart…</p>}
          </Card>
        )}
        <UsageModels result={modelActivity} report={current} model={query.model} onModel={(model) => changeFilters({ ...query, model })} />
      </div>

      {current && <UsageCharges report={current} />}
      <UsageBreakdown result={breakdown} groupBy={groupBy} page={cursors.length + 1}
        onGroup={(group) => { setGroupBy(group); setCursors([]); }}
        onPrevious={() => setCursors((pages) => pages.slice(0, -1))}
        onNext={(cursor) => setCursors((pages) => [...pages, cursor])}
        onRetry={refresh} requestLink={canReadRequests ? (entry, group) => usageRequestsLink(scope, entry, group) : undefined} />

      <div className="flex flex-wrap items-center justify-between gap-3 text-xs text-muted">
        {current && <p>Grouped by when requests were accepted. Updated {refreshedAt.format(new Date(current.as_of))} UTC.</p>}
        <Link className="inline-flex items-center gap-1.5 font-semibold text-muted hover:text-brand focus-visible:outline-brand"
          to={withProject("/routing", scope.project_id, scope.account_id)}>
          <Route className="h-3.5 w-3.5" aria-hidden /> Routing diagnostics <ArrowUpRight className="h-3.5 w-3.5" aria-hidden />
        </Link>
      </div>
      {canSettle && <SupplierSettlements scope={scope} onRecorded={refresh} />}
    </div>
  );
}

function downloadReport(report: UsageReport) {
  const blob = new Blob([JSON.stringify(report, null, 2) + "\n"], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `tokencraft-${report.source === "mock_provider" ? "mock-" : ""}usage-${report.window.from}-${report.group_by}.json`;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
}
