import { gatewayRequest, withProject, type ProjectScope } from "@/lib/gateway";
import type { AppKey, Capabilities, Graph, InvocationInput, IssuedKey, Permission, ProposedStep, Receipt, Release, Run, RunSummary, ToolBinding, UploadReference, Workflow, WorkflowSummary } from "./types";

const scopeSuffix = (scope: ProjectScope) => `?account_id=${encodeURIComponent(scope.account_id)}`;
const projectRoot = (scope: ProjectScope) => `/v1/projects/${encodeURIComponent(scope.project_id)}`;
const consoleRoot = (scope: ProjectScope, id?: string) => `${projectRoot(scope)}/workflows${id ? `/${encodeURIComponent(id)}` : ""}`;

export function workflowEndpoint(scope: ProjectScope, id: string) {
  return `${projectRoot(scope)}/workflow-endpoints/${encodeURIComponent(id)}`;
}

function write(body: object, method = "POST", extra?: HeadersInit): RequestInit {
  return { method, headers: { "Content-Type": "application/json", ...extra }, body: JSON.stringify(body) };
}

export const workflowApi = {
  capabilities: (scope: ProjectScope, signal?: AbortSignal) =>
    gatewayRequest<Capabilities>(withProject("/v1/workflows/capabilities", scope.project_id, scope.account_id), { signal }),
  tools: (scope: ProjectScope, signal?: AbortSignal) =>
    gatewayRequest<ToolBinding[]>(projectRoot(scope) + "/workflow-tools" + scopeSuffix(scope), { signal }),
  plan: (scope: ProjectScope, intent: string, useAdvisor: boolean, signal: AbortSignal) =>
    (useAdvisor
      ? gatewayRequest<{ primitives: ProposedStep[] }>("/v1/model-advisor/workflow-plan", { ...write({ intent }), signal })
      : Promise.resolve(undefined)
    ).then((plan) => gatewayRequest<Graph>(withProject("/v1/workflows/plan", scope.project_id, scope.account_id),
      { ...write({ intent, primitives: plan?.primitives }), signal })),
  list: (scope: ProjectScope, signal?: AbortSignal) => gatewayRequest<WorkflowSummary[]>(consoleRoot(scope) + scopeSuffix(scope), { signal }),
  read: (scope: ProjectScope, id: string, signal?: AbortSignal) => gatewayRequest<Workflow>(consoleRoot(scope, id) + scopeSuffix(scope), { signal }),
  create: (scope: ProjectScope, graph: Graph) => gatewayRequest<Workflow>(consoleRoot(scope) + scopeSuffix(scope), write(graph)),
  save: (scope: ProjectScope, id: string, revision: number, graph: Graph) =>
    gatewayRequest<Workflow>(consoleRoot(scope, id) + scopeSuffix(scope), write({ revision, graph }, "PUT")),
  releases: (scope: ProjectScope, id: string, signal?: AbortSignal) =>
    gatewayRequest<Release[]>(`${consoleRoot(scope, id)}/versions${scopeSuffix(scope)}`, { signal }),
  publish: (scope: ProjectScope, workflow: Workflow) => gatewayRequest<Workflow>(
    `${consoleRoot(scope, workflow.id)}/versions${scopeSuffix(scope)}`,
    write({ revision: workflow.revision, active_version: workflow.active_version }),
  ),
  activation: (scope: ProjectScope, workflow: Workflow, version: number, paused: boolean) => gatewayRequest<Workflow>(
    `${consoleRoot(scope, workflow.id)}/activation${scopeSuffix(scope)}`,
    write({ expected_version: workflow.active_version, version, paused }),
  ),
  upload: (scope: ProjectScope, workflow: string, file: File, type: string, sha256: string) =>
    gatewayRequest<UploadReference & { expires_at: string }>(
      `${workflowEndpoint(scope, workflow)}/uploads${scopeSuffix(scope)}`,
      { method: "POST", headers: { "Content-Type": type, "X-TokenCraft-Content-Sha256": sha256 }, body: file },
    ),
  test: (scope: ProjectScope, workflow: Workflow, inputs: Record<string, InvocationInput>, idempotencyKey: string, reuseRunId?: string) =>
    gatewayRequest<Receipt>(`${consoleRoot(scope, workflow.id)}/test-runs${scopeSuffix(scope)}`,
      write({ revision: workflow.revision, inputs, reuse_run_id: reuseRunId }, "POST", { "Idempotency-Key": idempotencyKey })),
  runs: (scope: ProjectScope, id: string, signal?: AbortSignal) =>
    gatewayRequest<RunSummary[]>(`${consoleRoot(scope, id)}/runs${scopeSuffix(scope)}`, { signal }),
  run: (scope: ProjectScope, workflow: string, run: string, signal?: AbortSignal) =>
    gatewayRequest<Run>(`${workflowEndpoint(scope, workflow)}/runs/${encodeURIComponent(run)}${scopeSuffix(scope)}`, { signal }),
  cancel: (scope: ProjectScope, workflow: string, run: string) =>
    gatewayRequest<void>(`${workflowEndpoint(scope, workflow)}/runs/${encodeURIComponent(run)}/cancel${scopeSuffix(scope)}`, { method: "POST" }),
  openApi: (scope: ProjectScope, workflow: string) =>
    gatewayRequest<{ "x-workflow-example-inputs"?: Record<string, InvocationInput>; "x-workflow-upload-inputs"?: Record<string, string> }>(`${workflowEndpoint(scope, workflow)}/openapi.json${scopeSuffix(scope)}`),
  keys: (scope: ProjectScope, id: string, signal?: AbortSignal) =>
    gatewayRequest<AppKey[]>(`${consoleRoot(scope, id)}/apps${scopeSuffix(scope)}`, { signal }),
  createApp: (scope: ProjectScope, id: string, name: string, scopes: Permission[], days: number) =>
    gatewayRequest<IssuedKey>(`${consoleRoot(scope, id)}/apps${scopeSuffix(scope)}`, write({ name, scopes, days })),
  rotate: (scope: ProjectScope, id: string, app: string, days: number) =>
    gatewayRequest<IssuedKey>(`${consoleRoot(scope, id)}/apps/${encodeURIComponent(app)}/keys${scopeSuffix(scope)}`, write({ days })),
  revoke: (scope: ProjectScope, id: string, key: string) =>
    gatewayRequest<void>(`${consoleRoot(scope, id)}/keys/${encodeURIComponent(key)}${scopeSuffix(scope)}`, { method: "DELETE" }),
};
