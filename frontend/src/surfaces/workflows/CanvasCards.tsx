import { Braces, ChevronDown, PackageCheck, Plus } from "lucide-react";
import type { DragEvent, ReactNode } from "react";

import { modelName, primitiveIcons } from "./ComponentLibrary";
import type { Graph, Primitive, Step, ValueKind } from "./types";
import { canWire, inputPort, outputPort, readWire, sourceKind, sourceName, sourcePort, stepOutput, stepSettings, wireSources,
  MOVE_TRANSFER, WIRE_TRANSFER, type WireSource, type WireTarget } from "./wiring";

type Wiring = {
  graph: Graph;
  source?: WireSource;
  choose: (source: WireSource, activate?: boolean) => void;
  connect: (source: WireSource, target: WireTarget) => void;
  expose: (source: WireSource, name?: string) => void;
  inspect: (id?: string, field?: string) => void;
};

const kindLabel: Record<ValueKind, string> = {
  text: "Text", integer: "Integer", image: "Image", video: "Video", audio: "Audio", media: "Media",
  metadata: "Metadata", mesh: "Mesh", document: "Document", archive: "Archive",
};

function SourcePort({ wire, source, kind, label, details }: {
  wire: Wiring; source: WireSource; kind: ValueKind; label: string; details?: string;
}) {
  const id = sourcePort(source);
  return <button className="wf-port wf-port-out" data-kind={kind} data-wire-port={id} type="button"
    aria-label={`Wire from ${label}`} aria-pressed={!!wire.source && sourcePort(wire.source) === id}
    draggable onDragStart={(event) => {
      event.dataTransfer.setData(WIRE_TRANSFER, id);
      event.dataTransfer.effectAllowed = "link";
      wire.choose(source, true);
    }} onClick={() => wire.choose(source)}>
    <span>{label}</span>{details && <small>{details}</small>}<i aria-hidden="true" />
  </button>;
}

function wireDrop(event: DragEvent<HTMLButtonElement>, enter: (source: WireSource) => void) {
  if (!event.dataTransfer.types.includes(WIRE_TRANSFER)) return;
  event.preventDefault();
  event.stopPropagation();
  const incoming = readWire(event.dataTransfer);
  if (incoming) enter(incoming);
}

function acceptWire(event: DragEvent<HTMLButtonElement>) {
  if (!event.dataTransfer.types.includes(WIRE_TRANSFER)) return;
  event.preventDefault();
  event.dataTransfer.dropEffect = "link";
}

function TargetPort({ wire, target }: { wire: Wiring; target: WireTarget }) {
  const { step, setting } = target;
  const binding = step.settings[setting.name];
  const sources = wireSources(binding).flatMap((source) => sourceName(wire.graph, source) ?? []);
  const connected = sources.length > 0;
  const ready = wire.source && canWire(wire.graph, wire.source, target);
  const needsValue = !binding || (binding.source === "fixed" && typeof binding.value === "string" && !binding.value.trim())
    || (binding.source === "template" && !binding.text.trim())
    || ((binding.source === "step" || binding.source === "request") && !connected);
  return <button className="wf-port wf-port-in" data-kind={setting.kind} data-wire-port={inputPort(step.id, setting.name)}
    data-ready={ready || undefined} data-connected={connected || undefined}
    data-required={setting.required && needsValue || undefined} type="button"
    aria-label={`Wire to ${step.title} · ${setting.label}${connected ? `, connected to ${sources.join(", ")}` : ""}`}
    onClick={() => wire.source ? wire.connect(wire.source, target) : wire.inspect(step.id, setting.name)}
    onDragOver={acceptWire} onDrop={(event) => wireDrop(event, (source) => wire.connect(source, target))}>
    <i aria-hidden="true" /><span>{setting.label}</span>
    <small>{needsValue ? (setting.required ? "Required" : "Optional") : connected ? "Wired" : binding?.source === "fixed" ? "Fixed" : "Set"}</small>
  </button>;
}

function Header({ icon, title, subtitle, selected, onClick, move }: {
  icon: ReactNode; title: string; subtitle: string; selected: boolean; onClick: () => void; move?: (event: DragEvent) => void;
}) {
  return <button type="button" className="wf-node-heading" aria-pressed={selected} onClick={onClick}
    draggable={!!move} onDragStart={move}>
    {icon}
    <span><strong>{title}</strong><small>{subtitle}</small></span>
  </button>;
}

export function InputCard({ wire, selected }: { wire: Wiring; selected: boolean }) {
  const entries = Object.entries(wire.graph.inputs);
  const wired = new Set(wire.graph.nodes.flatMap((step) => Object.values(step.settings)
    .flatMap(wireSources).flatMap((source) => source.source === "request" ? [source.field] : [])));
  const optional = entries.filter(([name, field]) => field.default !== null && !wired.has(name));
  const port = ([name, field]: typeof entries[number]) => <SourcePort key={name} wire={wire}
    source={{ source: "request", field: name }} kind={field.kind} label={field.title}
    details={field.default === null ? "Required" : "Optional"} />;
  return <>
    <Header icon={<span className="wf-node-icon"><Braces size={16} /></span>} title="Request inputs"
      subtitle={entries.length ? `${entries.length} ${entries.length === 1 ? "field" : "fields"}` : "From the caller"} selected={selected}
      onClick={() => wire.inspect()} />
    <div className="wf-node-ports">
      {entries.filter(([name, field]) => field.default === null || wired.has(name)).map(port)}
      {optional.length > 0 && <details className="wf-node-optional">
        <summary><ChevronDown size={12} aria-hidden="true" /> Optional fields</summary>
        {optional.map(port)}
      </details>}
      {entries.length === 0 && <button type="button" className="wf-node-add" onClick={() => wire.inspect()}>
        <Plus size={13} /> Add an input
      </button>}
    </div>
  </>;
}

export function StepCard({ step, primitive, wire, selected, spatial }: {
  step: Step; primitive?: Primitive; wire: Wiring; selected: boolean; spatial: boolean;
}) {
  const Icon = primitiveIcons[step.kind];
  const settings = stepSettings(step, primitive?.settings);
  const output = stepOutput(step);
  const shown = (setting: Primitive["settings"][number]) => setting.required
    || wireSources(step.settings[setting.name]).length > 0;
  const visible = settings.filter(shown);
  const optional = settings.filter((setting) => !shown(setting));
  return <>
    <Header icon={<span className={`wf-node-icon wf-node-${step.kind}`}><Icon size={16} /></span>}
      title={step.title} subtitle={step.kind === "tool" ? step.model : modelName(step.model)} selected={selected} onClick={() => wire.inspect(step.id)}
      move={spatial ? (event) => {
        const bounds = event.currentTarget.getBoundingClientRect();
        event.dataTransfer.setData(MOVE_TRANSFER, JSON.stringify({
          id: step.id, offsetX: event.clientX - bounds.left, offsetY: event.clientY - bounds.top,
        }));
        event.dataTransfer.effectAllowed = "move";
      } : undefined} />
    <div className="wf-node-ports">
      {visible.map((setting) => <TargetPort key={setting.name} wire={wire} target={{ step, setting }} />)}
      {optional.length > 0 && <details className="wf-node-optional">
        <summary><ChevronDown size={12} aria-hidden="true" /> Optional inputs</summary>
        {optional.map((setting) => <TargetPort key={setting.name} wire={wire} target={{ step, setting }} />)}
      </details>}
    </div>
    <footer className="wf-node-result">
      {output && <SourcePort wire={wire} source={{ source: "step", step: step.id }}
        kind={output} label={`${step.title} output`} details={kindLabel[output]} />}
    </footer>
  </>;
}

export function OutputCard({ wire, selected }: { wire: Wiring; selected: boolean }) {
  const outputs = Object.entries(wire.graph.outputs);
  return <>
    <Header icon={<span className="wf-node-icon wf-node-output"><PackageCheck size={16} /></span>} title="Response outputs"
      subtitle={outputs.length ? `${outputs.length} ${outputs.length === 1 ? "field" : "fields"}` : "Returned to the caller"} selected={selected}
      onClick={() => wire.inspect()} />
    <div className="wf-node-ports">
      {outputs.map(([name, id]) => {
        const kind = sourceKind(wire.graph, { source: "step", step: id });
        return <button className="wf-port wf-port-in" key={name} type="button" data-kind={kind}
          data-wire-port={outputPort(name)} data-connected={!!kind} data-ready={wire.source?.source === "step" || undefined}
          aria-label={`Wire to response · ${name}`}
          onClick={() => wire.source ? wire.expose(wire.source, name) : wire.inspect()}
          onDragOver={acceptWire} onDrop={(event) => wireDrop(event, (source) => wire.expose(source, name))}>
          <i aria-hidden="true" /><span>{name}</span>{kind && <small>{kindLabel[kind]}</small>}
        </button>;
      })}
      <button className="wf-node-add" type="button" onDragOver={acceptWire}
        onDrop={(event) => wireDrop(event, (source) => wire.expose(source))}
        onClick={() => wire.source ? wire.expose(wire.source) : wire.inspect()}>
        <Plus size={13} /> Add response output
      </button>
    </div>
  </>;
}
