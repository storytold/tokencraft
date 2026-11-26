import type { Binding, Graph, Setting, Step, ValueKind } from "./types";

export type WireSource = Extract<Binding, { source: "request" | "step" }>;
export type WireTarget = { step: Step; setting: Setting };

export const COMPONENT_TRANSFER = "application/x-tokencraft-component";
export const MOVE_TRANSFER = "application/x-tokencraft-move";
export const WIRE_TRANSFER = "application/x-tokencraft-wire";

export const sourcePort = (source: WireSource) =>
  source.source === "request" ? `request:${source.field}` : `step:${source.step}`;
export const inputPort = (step: string, field: string) => `setting:${step}:${field}`;
export const outputPort = (field: string) => `response:${field}`;

export function wireSources(binding: Binding | undefined): WireSource[] {
  if (!binding || binding.source === "fixed") return [];
  if (binding.source !== "template") return [binding];
  return [...binding.text.matchAll(/\{\{\s*(input|step)\.([a-z][a-z0-9_]*)\s*\}\}/g)]
    .map((match) => match[1] === "input" ? { source: "request", field: match[2] } : { source: "step", step: match[2] });
}

export function sourceKind(graph: Graph, source: WireSource): ValueKind | undefined {
  if (source.source === "request") return graph.inputs[source.field]?.kind;
  const node = graph.nodes.find((item) => item.id === source.step);
  return node && stepOutput(node);
}

export function stepOutput(step: Step): ValueKind | undefined {
  return step.kind === "tool" ? step.tool?.output : step.kind;
}

export function stepSettings(step: Step, settings: Setting[] = []): Setting[] {
  if (!step.tool) return settings;
  return Object.entries(step.tool.inputs).map(([name, kind]) => ({
    name, kind, required: true, label: name.replaceAll("_", " "),
  }));
}

export function artifactKind(kind: ValueKind | undefined) {
  return kind === "image" || kind === "video" || kind === "audio" || kind === "mesh" || kind === "document";
}

export function sourceName(graph: Graph, source: WireSource) {
  return source.source === "request" ? graph.inputs[source.field]?.title : graph.nodes.find((node) => node.id === source.step)?.title;
}

export function canWire(graph: Graph, source: WireSource, { step, setting }: WireTarget): boolean {
  const kind = sourceKind(graph, source);
  if (kind !== setting.kind && !(setting.kind === "media" && artifactKind(kind))) return false;
  if (source.source === "request") return true;
  const seen = new Set<string>();
  const steps = new Map(graph.nodes.map((node) => [node.id, node]));
  function dependsOn(id: string): boolean {
    if (id === step.id) return true;
    if (seen.has(id)) return false;
    seen.add(id);
    const node = steps.get(id);
    return !!node && Object.values(node.settings).flatMap(wireSources)
      .some((binding) => binding.source === "step" && dependsOn(binding.step));
  }
  return !dependsOn(source.step);
}

export function readWire(transfer: DataTransfer): WireSource | undefined {
  const [kind, key, extra] = transfer.getData(WIRE_TRANSFER).split(":");
  if (!key || extra !== undefined) return;
  if (kind === "request") return { source: "request", field: key };
  if (kind === "step") return { source: "step", step: key };
}
