import { useEffect, useRef, useState, type FormEvent } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { ArrowRight, Search } from "lucide-react";
import { Button, Input } from "@/components/ui";
import type { ProjectScope } from "@/lib/gateway";
import { useOrg } from "@/lib/org";
import { comparisonApi, errorMessage } from "@/surfaces/comparison/api";
import { RequestActivity } from "@/surfaces/comparison/RequestActivity";
import { ProjectStatus, useProject } from "@/surfaces/projects/ProjectScope";

export default function GatewayRequests() {
  const { scope, project } = useProject();
  return (
    <>
      {!scope || !project || project.role === "billing_viewer" ? (
        <>
          <div className="max-w-md"><RequestLookup /></div>
          {!scope || !project ? <ProjectStatus /> : <p className="text-sm text-muted">Your billing viewer role can read usage for this project.</p>}
        </>
      ) : <RequestActivity key={`${scope.account_id}/${scope.project_id}`} scope={scope} historyLookup={<RequestLookup scope={scope} />}
        readOnly={project.role === "client_viewer"} canManageRequests={project.role === "admin"} boundClient={project.client_ref} />}
    </>
  );
}

function RequestLookup({ scope }: { scope?: ProjectScope }) {
  const navigate = useNavigate();
  const location = useLocation();
  const locationRef = useRef(location);
  locationRef.current = location;
  const { accounts, current, reload: reloadAccounts } = useOrg();
  const { projects, reload: reloadProjects } = useProject();
  const [draft, setDraft] = useState("");
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState("");
  const pending = useRef<AbortController | null>(null);

  useEffect(() => () => pending.current?.abort(), []);

  async function find(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const id = draft.trim();
    if (!id || pending.current) return;
    const controller = new AbortController();
    pending.current = controller;
    setSearching(true);
    setError("");
    try {
      const result = await comparisonApi.locateRequest(id, controller.signal);
      if (controller.signal.aborted) return;
      if (!accounts.some((account) => account.account_id === result.account_id)) {
        const updated = await reloadAccounts();
        if (!updated?.some((account) => account.account_id === result.account_id)) {
          if (!controller.signal.aborted) setError("Couldn’t confirm the request’s organization. Try again to open this request.");
          return;
        }
      }
      if (controller.signal.aborted) return;
      if (current?.account_id === result.account_id
        && !projects.some((project) => project.id === result.project_id && project.role !== "billing_viewer")) {
        reloadProjects();
      }
      const sameProject = result.account_id === scope?.account_id && result.project_id === scope.project_id;
      const latest = locationRef.current;
      const query = new URLSearchParams(sameProject ? latest.search : undefined);
      const alreadyOpen = query.has("request_id");
      query.set("project_id", result.project_id);
      query.set("request_id", result.request_id);
      const route = sameProject ? latest.pathname : "/dashboard/" + encodeURIComponent(result.account_id) + "/requests";
      navigate(route + "?" + query, { replace: alreadyOpen });
    } catch (cause) {
      if (!controller.signal.aborted) setError(errorMessage(cause));
    } finally {
      pending.current = null;
      if (!controller.signal.aborted) setSearching(false);
    }
  }

  return (
    <form aria-label="Find request" role="search" onSubmit={find}>
      <div className="relative flex items-center rounded-lg">
        <Search className="pointer-events-none absolute left-3 h-4 w-4 text-muted" aria-hidden />
        <Input aria-label="Request ID" className="min-w-0 pl-9 pr-[4.5rem] font-mono" value={draft} onChange={(event) => {
          setDraft(event.target.value);
          setError("");
        }} required maxLength={64} placeholder="Find by request ID" title="Searches organizations and projects you can access"
          autoCapitalize="none" autoComplete="off" spellCheck={false} readOnly={searching} />
        <Button type="submit" size="sm" variant="ghost" className="absolute right-1 rounded-md px-2 text-xs"
          disabled={searching || !draft.trim()} aria-label={searching ? "Finding request" : "Open request"}>
          {searching ? "Finding…" : "Open"} <ArrowRight className="h-3.5 w-3.5" aria-hidden />
        </Button>
      </div>
      {error && <p role="alert" className="mt-1.5 text-xs text-rose-600 dark:text-rose-300">{error}</p>}
    </form>
  );
}
