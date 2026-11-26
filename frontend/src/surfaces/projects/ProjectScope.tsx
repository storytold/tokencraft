import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { ChoicePicker } from "@/components/ChoicePicker";
import { Select } from "@/components/ui";
import { errorMessage, type ProjectScope } from "@/lib/gateway";
import { projectsApi, type Project } from "./api";

type ProjectSelection = {
  projects: Project[];
  project?: Project;
  scope?: ProjectScope;
  loading: boolean;
  error?: string;
  select: (id: string) => void;
  reload: () => void;
};

const Context = createContext<ProjectSelection | null>(null);

export function ProjectProvider({ accountId, includeAccountInUrl = false, children }: {
  accountId?: string; includeAccountInUrl?: boolean; children: ReactNode;
}) {
  const [params, setParams] = useSearchParams();
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<{ accountId: string; revision: number; projects?: Project[]; error?: string }>();

  useEffect(() => {
    if (!accountId) return;
    const controller = new AbortController();
    projectsApi.list(accountId, controller.signal).then((projects) => {
      if (!controller.signal.aborted) setResult({ accountId, revision, projects });
    }).catch((error) => {
      if (!controller.signal.aborted) setResult({ accountId, revision, error: errorMessage(error) });
    });
    return () => controller.abort();
  }, [accountId, revision]);

  const current = result && result.accountId === accountId && result.revision === revision ? result : undefined;
  const projects = current?.projects ?? [];
  const requested = params.get("project_id");
  const project = projects.find((item) => requested === null ? item.is_default : item.id === requested);
  const scope = useMemo(() => accountId && project ? { account_id: accountId, project_id: project.id } : undefined, [accountId, project]);
  const error = current?.error ?? (current && !project && requested !== null ? "This project is unavailable in the selected organization." : undefined);

  function select(id: string) {
    const next = new URLSearchParams(params);
    next.set("project_id", id);
    if (includeAccountInUrl && accountId) next.set("account_id", accountId);
    next.delete("request_id");
    next.delete("workflow_id");
    next.delete("run_id");
    next.delete("workflow_view");
    next.delete("webhook_subscription_id");
    setParams(next);
  }

  return <Context.Provider value={{
    projects, project, scope, loading: Boolean(accountId && !current), error, select,
    reload: () => setRevision((previous) => previous + 1),
  }}>{children}</Context.Provider>;
}

export function useProject() {
  const value = useContext(Context);
  if (!value) throw new Error("ProjectProvider is required");
  return value;
}

export function ProjectPicker({ compact = false }: { compact?: boolean }) {
  const { projects, project, loading, select } = useProject();
  if (!projects.length && !loading) return null;
  const options = projects.map(({ id, name, is_default }) => ({
    value: id, label: is_default ? "Default" : name,
  }));
  const value = project?.id ?? "";
  const placeholder = loading ? "Loading projects…" : "Choose a project";
  if (compact) return <ChoicePicker compact label="Project" value={value} options={options} placeholder={placeholder} onChange={select} />;

  return (
    <label className="block min-w-0 max-w-xs space-y-1 text-xs font-medium text-muted">
      Project
      <Select value={value} disabled={loading} onChange={(event) => select(event.target.value)}>
        {!project && <option value="" disabled>{placeholder}</option>}
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </Select>
    </label>
  );
}

export function ProjectStatus() {
  const { error, loading, reload } = useProject();
  if (error) return <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{error} <button type="button" className="font-medium underline" onClick={reload}>Reload projects</button></p>;
  return <p role="status" className="text-sm text-muted">{loading ? "Loading projects…" : "No project is selected."}</p>;
}
