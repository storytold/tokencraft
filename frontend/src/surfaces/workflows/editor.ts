import { acceptsKind, canWire, isFileInput } from "./wiring";
import type { Binding, Capabilities, Graph, PrimitiveKind, Setting, Step, ToolBinding, ToolOperation } from "./types";

export const EMPTY: Graph = { schema: 1, title: "Untitled workflow", inputs: {}, nodes: [], outputs: {}, max_usd_nanos: 0 };
export const usd = (nanos: number) => new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 6 }).format(nanos / 1e9);

export function nextId(prefix: string, occupied: string[]) {
  let suffix = 1;
  while (occupied.includes(`${prefix}_${suffix}`)) suffix += 1;
  return `${prefix}_${suffix}`;
}

export function addStep(graph: Graph, kind: PrimitiveKind, capabilities: Capabilities, modelId?: string): Graph {
  const model = capabilities.models.find((item) => item.kind === kind && (!modelId || item.id === modelId));
  const primitive = capabilities.primitives.find((item) => item.kind === kind);
  const id = nextId(kind, graph.nodes.map((item) => item.id));
  const settings: Step["settings"] = { prompt: { source: "fixed", value: "" } };
  if (kind === "text") settings.max_output_tokens = { source: "fixed", value: 256 };
  if (kind !== "text") settings.aspect_ratio = { source: "fixed", value: model?.aspect_ratios[0] ?? "16:9" };
  if (kind === "video") {
    settings.duration_seconds = { source: "fixed", value: model?.durations[0] ?? 5 };
    settings.resolution = { source: "fixed", value: model?.resolutions[0] ?? "540p" };
  }
  const cap = { text: 50_000_000, image: 250_000_000, video: 1_000_000_000 }[kind];
  return { ...graph, max_usd_nanos: graph.max_usd_nanos + cap,
    nodes: [...graph.nodes, { id, kind, settings, model: model?.id ?? "", title: primitive?.label ?? kind, max_usd_nanos: cap }] };
}

export function addTool(graph: Graph, binding: ToolBinding, operation: ToolOperation): Graph {
  const id = nextId("tool", graph.nodes.map((step) => step.id));
  const step: Step = {
    id, title: operation.label, kind: "tool", model: operation.id, max_usd_nanos: 0, settings: {},
    tool: {
      binding_revision: binding.revision, revision: operation.revision, output: operation.output.kind,
      inputs: Object.fromEntries(Object.entries(operation.inputs).map(([name, input]) => [name, input.kind])),
      parameters: {},
    },
  };
  return { ...graph, nodes: [...graph.nodes, step] };
}

export function updateStep(graph: Graph, node: Step): Graph {
  return { ...graph, nodes: graph.nodes.map((item) => item.id === node.id ? node : item) };
}

export function pruneUnusedToolInputs(graph: Graph): Graph {
  const used = new Set(graph.nodes.flatMap((step) => Object.values(step.tool?.parameter_inputs ?? {})));
  const inputs = Object.fromEntries(Object.entries(graph.inputs)
    .filter(([name, input]) => input.kind !== "parameter" || used.has(name)));
  return { ...graph, inputs };
}

export function setBinding(step: Step, setting: Setting, binding?: Binding): Step {
  const settings = { ...step.settings };
  if (binding) settings[setting.name] = binding;
  else delete settings[setting.name];
  return { ...step, settings };
}

export function defaultBinding(source: Binding["source"], setting: Setting, graph: Graph, step: Step): Binding {
  if (source === "request") {
    return { source, field: Object.entries(graph.inputs).find(([, field]) => acceptsKind(setting.kind, field.kind))?.[0] ?? "" };
  }
  if (source === "step") {
    return { source, step: graph.nodes.find((item) => canWire(graph, { source, step: item.id }, { step, setting }))?.id ?? "" };
  }
  if (source === "template") return { source, text: "" };
  return { source, value: setting.kind === "integer" ? 1 : "" };
}

export function bindingModes(setting: Setting): { value: Binding["source"]; label: string }[] {
  if (isFileInput(setting.kind)) return [
    { value: "step", label: "Connected step" },
    { value: "request", label: "Uploaded file" },
  ];
  return [
    { value: "fixed", label: "Fixed value" },
    { value: "request", label: "Request input" },
    ...(setting.kind === "text" ? [
      { value: "step" as const, label: "Connected step" },
      { value: "template" as const, label: "Template" },
    ] : []),
  ];
}
