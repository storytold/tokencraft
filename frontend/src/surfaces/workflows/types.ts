export type PrimitiveKind = "text" | "image" | "video";
export type ArtifactKind = "image" | "video" | "audio" | "metadata" | "mesh" | "document" | "archive";
export type ValueKind = "text" | "integer" | "media" | ArtifactKind;
export type ToolParameter = { type: "integer" | "number"; minimum: number; maximum: number; multipleOf?: number; default?: number }
  | { type: "string"; enum: string[]; default?: string };
export type ToolOperation = {
  id: string;
  revision: number;
  label: string;
  inputs: Record<string, { kind: UploadKind }>;
  parameters: { properties: Record<string, ToolParameter>; required: string[] };
  output: { kind: ArtifactKind; content_types: string[] };
};
export type ToolBinding = { name: string; revision: string; manifest: { tools: ToolOperation[] } };
export type ToolStep = {
  binding_revision: string;
  revision: number;
  inputs: Record<string, UploadKind>;
  output: ArtifactKind;
  parameters: Record<string, Scalar>;
};
export type Scalar = string | number;
export type UploadKind = Exclude<ArtifactKind, "metadata" | "archive"> | "media";
export type UploadReference = { upload_id: string };
export type InvocationInput = Scalar | UploadReference;
export type Bindings = Record<string, Binding>;
export type Binding =
  | { source: "fixed"; value: Scalar }
  | { source: "request"; field: string }
  | { source: "step"; step: string }
  | { source: "template"; text: string };

export type Setting = { name: string; label: string; kind: ValueKind; required: boolean };
export type Primitive = { kind: PrimitiveKind; label: string; output: ValueKind; settings: Setting[] };
export type WorkflowModel = {
  id: string;
  kind: PrimitiveKind;
  providers: string[];
  aspect_ratios: string[];
  durations: number[];
  resolutions: string[];
  accepts_first_frame: boolean;
};
export type Capabilities = { primitives: Primitive[]; models: WorkflowModel[]; tools?: ToolBinding[] };
export type ProposedStep = { kind: PrimitiveKind; title: string; animated_image?: boolean };
export type WorkflowInput = { title: string; kind: "text" | "integer" | UploadKind; default: Scalar | null };
export type Step = {
  id: string;
  title: string;
  kind: PrimitiveKind | "tool";
  model: string;
  tool?: ToolStep;
  max_usd_nanos: number;
  settings: Bindings;
};
export type Graph = {
  schema: 1;
  title: string;
  inputs: Record<string, WorkflowInput>;
  nodes: Step[];
  outputs: Record<string, string>;
  max_usd_nanos: number;
};
export type WorkflowSummary = { id: string; title: string; revision: number; active_version: number | null; paused: boolean };
export type Workflow = WorkflowSummary & { draft: Graph };
export type Release = { version: number; draft_revision: number; published_at: string };
export type RunStatus = "queued" | "running" | "reconciling" | "succeeded" | "failed" | "canceled" | "expired";
export type RunSummary = { id: string; status: RunStatus; version: number | null; draft_revision: number; created_at: string };
export type WorkflowRequestParent = { workflow_id: string; run_id: string; title: string };
export type Receipt = { id: string; workflow_version: number | null; status_url: string; replayed: boolean };
type RunOutput =
  | { type: "text"; text: string }
  | { type: "media"; kind: ArtifactKind; download_url: string }
  | { type: "fixture"; kind: ValueKind };
export type ExecutionNode = {
  id: string;
  title: string;
  kind: PrimitiveKind | "tool";
  dependencies: string[];
  status: string;
  request: string | null;
  request_status: string | null;
  provider: string | null;
  admitted_at: string | null;
  finished_at: string | null;
};
export type Run = {
  id: string;
  workflow_version: number | null;
  draft_revision: number;
  status: RunStatus;
  created_at: string;
  client_ref?: string;
  steps: { node: string; request: string; status: string }[];
  tools?: Record<string, { request_id: string; status: string; admitted_at: string; finished_at: string | null }>;
  error: string | null;
  outputs?: Record<string, RunOutput>;
  execution?: { title: string; evidence_expired: boolean; nodes: ExecutionNode[] };
};
export type Permission = "runs:submit" | "runs:read" | "outputs:read" | "runs:cancel";
export type AppKey = { app_id: string; key_id: string; scopes: Permission[]; name: string; prefix: string; expires_at: string; revoked_at: string | null };
export type IssuedKey = { app_id: string; key_id: string; secret: string; expires_at: string };

export const TERMINAL: ReadonlySet<RunStatus> = new Set(["succeeded", "failed", "canceled", "expired"]);
export const statusLabel: Record<RunStatus, string> = {
  queued: "Queued", running: "Running", reconciling: "Reconciling", succeeded: "Succeeded",
  failed: "Failed", canceled: "Canceled", expired: "Expired",
};
