import type { ProjectScope } from "@/lib/gateway";

export function workflowRunPath(scope: ProjectScope, workflow: string, run: string) {
  const query = new URLSearchParams({ project_id: scope.project_id, workflow_id: workflow, run_id: run });
  return `/dashboard/${encodeURIComponent(scope.account_id)}/workflows?${query}`;
}
