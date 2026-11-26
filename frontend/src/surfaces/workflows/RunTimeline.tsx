import { Link } from "react-router-dom";
import type { ProjectScope } from "@/lib/gateway";
import type { Graph, Run } from "./types";

function requestLink(scope: ProjectScope, request: string) {
  const query = new URLSearchParams({ project_id: scope.project_id, request_id: request });
  return `/dashboard/${encodeURIComponent(scope.account_id)}/requests?${query}`;
}

function label(status: string) {
  return status.replaceAll("_", " ");
}

export function RunTimeline({ scope, run, graph }: { scope: ProjectScope; run: Run; graph: Graph }) {
  const steps = run.execution?.nodes.map((node) => ({
    id: node.id, title: node.title, status: node.status, request: node.request,
    requestStatus: node.request_status, provider: node.provider,
  })) ?? [
    ...(run.reuse?.steps ?? []).map((id) => ({
      id, title: graph.nodes.find((node) => node.id === id)?.title ?? id,
      status: "reused", request: null, requestStatus: null, provider: null,
    })),
    ...run.steps.map((step) => ({
      id: step.node, title: graph.nodes.find((node) => node.id === step.node)?.title ?? step.node,
      status: step.status, request: step.request, requestStatus: null, provider: null,
    })),
    ...Object.entries(run.tools ?? {}).map(([id, step]) => ({
      id, title: graph.nodes.find((node) => node.id === id)?.title ?? id,
      status: step.status, request: null, requestStatus: null, provider: null,
    })),
  ];

  return <>
    {run.execution?.evidence_expired && <p className="wf-muted">Run content is unavailable.</p>}
    {steps.length > 0 && <ol className="wf-timeline">{steps.map((step) => <li key={step.id}>
      <div className="wf-timeline-step">
        <strong>{step.title}</strong>
        {step.request
          ? <Link className="wf-trace-link" to={requestLink(scope, step.request)} aria-label={`Open request trace for ${step.title}`}>
            <code>{step.request}</code>
          </Link>
          : run.tools?.[step.id] && <small>{run.tools[step.id].request_id}</small>}
        {step.status === "reused" && run.reuse && <small>From run <code>{run.reuse.source_run_id}</code></small>}
        {(step.provider || (step.requestStatus && step.requestStatus !== step.status)) && <small>
          {[step.provider, step.requestStatus && step.requestStatus !== step.status && `Request ${label(step.requestStatus)}`]
            .filter(Boolean).join(" · ")}
        </small>}
      </div>
      <span>{label(step.status)}</span>
    </li>)}</ol>}
  </>;
}
