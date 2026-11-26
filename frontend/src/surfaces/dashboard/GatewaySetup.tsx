import { useEffect, useState } from "react";
import { Link, Navigate, useLocation, useSearchParams } from "react-router-dom";
import { Badge, Button, Card } from "@/components/ui";
import { errorMessage, gatewayRequest, withProject } from "@/lib/gateway";
import { QueueBacklog } from "./QueueBacklog";
import { DispatchControls } from "./DispatchControls";

type Check = "checking" | "ready" | "unavailable" | "not_configured" | "external";
type SetupReport = {
  ready: boolean;
  database: Check;
  migrations: Check;
  worker: Check;
  provider_worker: Check;
  artifact_storage: Check;
  artifact_backend: "filesystem" | "postgresql" | "s3";
};

const states = {
  checking: { label: "Checking", tone: "muted" },
  ready: { label: "Ready", tone: "green" },
  unavailable: { label: "Unavailable", tone: "red" },
  not_configured: { label: "Not configured", tone: "muted" },
  external: { label: "Separate worker", tone: "muted" },
} as const;

function checks(report: SetupReport) {
  const backends = {
    filesystem: { label: "filesystem", help: "Check that the artifact directory is private and writable by the gateway." },
    postgresql: { label: "PostgreSQL", help: "Check the database connection and migrations." },
    s3: { label: "S3-compatible bucket", help: "Check access to the shared artifact bucket from the gateway and workers." },
  };
  const backend = backends[report.artifact_backend];
  return [
    { title: "Gateway API", status: "ready" as const },
    { title: "Database connection", status: report.database, help: "Check the PostgreSQL service and gateway database connection." },
    { title: "Database migrations", status: report.migrations, help: "Check that the database migration ledger matches the running gateway. Apply pending migrations with admin-cli migrate." },
    { title: "Request worker", status: report.worker, help: "Check the gateway logs for the request worker." },
    { title: "Provider queue worker", status: report.provider_worker, help: "Check the gateway logs for provider task claims." },
    { title: `Artifact storage · ${backend.label}`, status: report.artifact_storage, help: backend.help },
  ];
}

export default function GatewaySetup() {
  const { pathname } = useLocation();
  const [params] = useSearchParams();
  const search = new URLSearchParams(params);
  const suffix = search.get("view") === "providers" ? "" : "/operations";
  search.delete("view");
  return <Navigate replace to={{
    pathname: pathname.replace(/\/setup\/?$/, `/providers${suffix}`),
    search: search.toString(),
  }} />;
}

export function GatewayOperations({ accountId, projectId }: { accountId: string; projectId?: string }) {
  const [revision, setRevision] = useState(0);
  return (
    <>
      <SetupStatus accountId={accountId} />
      <DispatchControls key={accountId} accountId={accountId} onSaved={() => setRevision((value) => value + 1)} />
      <QueueBacklog accountId={accountId} initialProjectId={projectId} revision={revision} />
    </>
  );
}

function SetupStatus({ accountId }: { accountId: string }) {
  const [report, setReport] = useState<SetupReport>();
  const [error, setError] = useState("");
  const [checking, setChecking] = useState(true);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    const query = new URLSearchParams({ account_id: accountId });
    gatewayRequest<SetupReport>(`/v1/setup/status?${query}`, { signal: controller.signal })
      .then((value) => {
        if (controller.signal.aborted) return;
        setReport(value);
        setChecking(false);
      }).catch((failure: unknown) => {
        if (controller.signal.aborted) return;
        setReport(undefined);
        setError(errorMessage(failure));
        setChecking(false);
      });
    return () => controller.abort();
  }, [accountId, revision]);

  return (
    <Card className="min-w-0 p-4 sm:p-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p role="status" className="font-semibold">
          {checking ? "Checking gateway…" : report?.ready ? "Ready for requests" : report ? "Gateway needs attention" : "Gateway checks unavailable"}
        </p>
        <Button type="button" variant="outline" size="sm" disabled={checking} onClick={() => {
          setChecking(true);
          setError("");
          setRevision((value) => value + 1);
        }}>Refresh checks</Button>
      </div>
      {report && <dl className="mt-4 divide-y divide-border border-y border-border" aria-busy={checking}>
        {checks(report).map(({ title, status, help }) => (
          <div key={title} className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 py-3">
            <dt className="font-medium">{title}</dt>
            <dd><Badge tone={states[status].tone}>{states[status].label}</Badge></dd>
            {status === "unavailable" && help && <dd className="basis-full text-sm text-muted">{help}</dd>}
          </div>
        ))}
      </dl>}
      {report?.worker === "external" && <p className="mt-4 text-sm text-muted">Readiness covers this API process. Check the worker’s own readiness endpoint for dispatch and polling.</p>}
      {error && <p role="alert" className="mt-4 text-sm text-rose-600 dark:text-rose-300">{error}</p>}
      {report?.ready && !checking && (
        <Link className="mt-4 inline-block text-sm font-semibold text-brand hover:underline" to={withProject("/routing", undefined, accountId)}>
          Open routing diagnostics
        </Link>
      )}
    </Card>
  );
}
