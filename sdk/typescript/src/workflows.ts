import { hasIdPrefix, object, TokenCraftError } from "./http.js";
import type { CallOptions, WaitOptions } from "./types.js";
import type { VerifiedArtifact } from "./content.js";

export interface WorkflowUploadRef {
  upload_id: string;
}

export interface WorkflowUpload extends WorkflowUploadRef {
  expires_at: string;
}

export type WorkflowInput = string | number | WorkflowUploadRef;

export interface WorkflowInvokeOptions extends CallOptions {
  idempotencyKey: string;
  /** Opaque customer reference used for usage attribution. */
  clientRef?: string;
  /** A successful run from the same application, workflow version, and customer. */
  reuseRunId?: string;
}

export interface WorkflowReceipt {
  id: string;
  workflow_version: number;
  status_url: string;
  replayed: boolean;
}

export type WorkflowRunStatus = "queued" | "running" | "reconciling" | "succeeded" | "failed" | "canceled" | "expired";

export type WorkflowOutput =
  | { type: "text"; text: string }
  | { type: "media"; kind: string; download_url: string }
  | { type: "fixture"; kind: string };

export interface WorkflowRun {
  id: string;
  status: WorkflowRunStatus;
  workflow_version: number;
  draft_revision: number;
  created_at: string;
  client_ref?: string;
  steps: { node: string; request: string; status: string }[];
  tools?: Record<string, { request_id: string; status: string; admitted_at: string; finished_at: string | null }>;
  reuse?: { source_run_id: string; steps: string[] };
  error: string | null;
  outputs?: Record<string, WorkflowOutput>;
}

/** A published endpoint scoped to one project and workflow. */
export interface WorkflowEndpoint {
  /** Fetch the active version's OpenAPI document. Requires `runs:submit`. */
  contract(options?: CallOptions): Promise<Record<string, unknown>>;
  /** Stage media for a later invocation. Requires `runs:submit`. */
  upload(contentType: string, bytes: Uint8Array, options?: CallOptions): Promise<WorkflowUpload>;
  /** Invoke the endpoint. Reuse the key and exact inputs to recover an admitted run. */
  invoke(inputs: Readonly<Record<string, WorkflowInput>>, options: WorkflowInvokeOptions): Promise<WorkflowReceipt>;
  /** Read the application's run. Requires `runs:read`. */
  get(runId: string, options?: CallOptions): Promise<WorkflowRun>;
  /** Poll to a terminal status with a deadline and conditional requests. */
  wait(runId: string, options?: WaitOptions): Promise<WorkflowRun>;
  /** Stop further admission for the run. Requires `runs:cancel`. */
  cancel(runId: string, options?: CallOptions): Promise<void>;
  /** Download a named output and verify its response checksum. Requires `outputs:read`. */
  downloadOutput(runId: string, name: string, options?: CallOptions): Promise<VerifiedArtifact>;
}

export const maxWorkflowFileBytes = 32 * 1024 * 1024;

const terminal = new Set<WorkflowRunStatus>(["succeeded", "failed", "canceled", "expired"]);
const pending = new Set<WorkflowRunStatus>(["queued", "running", "reconciling"]);

export function workflowPath(projectId: string, workflowId: string): string {
  if (!hasIdPrefix(projectId, "proj_") || !hasIdPrefix(workflowId, "wf_")) {
    throw new TypeError("Supply valid TokenCraft project and workflow IDs.");
  }
  return `v1/projects/${projectId}/workflow-endpoints/${workflowId}`;
}

export function workflowRunPath(path: string, runId: string): string {
  if (!hasIdPrefix(runId, "wfr_")) throw new TypeError("Supply a valid TokenCraft workflow run ID.");
  return `${path}/runs/${runId}`;
}

export function workflowOutputPath(runPath: string, name: string): string {
  if (typeof name !== "string" || name.length > 48 || !/^[a-z]/.test(name) || /[^a-z0-9_]/.test(name)) {
    throw new TypeError("Supply a valid workflow output name.");
  }
  return `${runPath}/outputs/${name}`;
}

export function workflowInputs(inputs: Readonly<Record<string, WorkflowInput>>): Record<string, WorkflowInput> {
  if (!object(inputs)) throw new TypeError("Supply workflow inputs as an object.");
  const values = Object.entries(inputs).map(([name, value]) => {
    if (typeof value === "string" || (typeof value === "number" && Number.isFinite(value))) {
      return [name, value] as const;
    }
    if (object(value) && hasIdPrefix(value.upload_id, "wfu_")) {
      return [name, { upload_id: value.upload_id }] as const;
    }
    throw new TypeError("Supply workflow input values as text, finite numbers, or upload references.");
  });
  return Object.fromEntries(values);
}

export function workflowRunDone(run: WorkflowRun, runId: string): boolean {
  if (run.id !== runId || !validVersion(run.workflow_version) || !Array.isArray(run.steps)) {
    throw invalidWorkflowResponse(200, runId);
  }
  if (terminal.has(run.status)) return true;
  if (pending.has(run.status)) return false;
  throw invalidWorkflowResponse(200, runId);
}

export function validVersion(version: number): boolean {
  return Number.isSafeInteger(version) && version > 0;
}

export function invalidWorkflowResponse(status: number, runId?: string): TokenCraftError {
  return new TokenCraftError(status, "invalid_response", "The response did not match the published workflow operation.", runId);
}
