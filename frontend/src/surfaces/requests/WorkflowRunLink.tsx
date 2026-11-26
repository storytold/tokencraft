import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import type { ProjectScope } from "@/lib/gateway";
import { workflowApi } from "@/surfaces/workflows/api";
import { workflowRunPath } from "@/surfaces/workflows/links";
import type { WorkflowRequestParent } from "@/surfaces/workflows/types";

export function WorkflowRunLink({ scope, request }: { scope: ProjectScope; request: string }) {
  const [resolved, setResolved] = useState<{ scope: ProjectScope; request: string; parent: WorkflowRequestParent }>();
  useEffect(() => {
    const controller = new AbortController();
    workflowApi.requestParent(scope, request, controller.signal).then((result) => {
      if (result && !controller.signal.aborted) setResolved({ scope, request, parent: result });
    }).catch(() => {
      if (!controller.signal.aborted) setResolved(undefined);
    });
    return () => controller.abort();
  }, [scope, request]);
  const parent = resolved?.scope === scope && resolved.request === request ? resolved.parent : undefined;
  if (!parent) return null;
  return <p className="mb-3 break-words text-xs text-muted">
    Workflow run{" "}
    <Link className="font-medium text-brand underline underline-offset-2"
      to={workflowRunPath(scope, parent.workflow_id, parent.run_id)}
      aria-label={`Open workflow run for ${parent.title}`}>{parent.title}</Link>
  </p>;
}
