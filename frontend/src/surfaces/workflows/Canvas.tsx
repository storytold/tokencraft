import { useEffect, useRef, useState, type DragEvent } from "react";
import { Plus, X } from "lucide-react";

import { InputCard, OutputCard, StepCard } from "./CanvasCards";
import { BOARD_LIMIT, CARD_ROW, CARD_WIDTH, loadPositions, placeCards, type Position } from "./canvasLayout";
import { ComponentLibrary, type ComponentChoice } from "./ComponentLibrary";
import { Connections } from "./Connections";
import { ContractEditor } from "./ContractEditor";
import { addStep, addTool, nextId, pruneUnusedToolInputs, setBinding, updateStep, usd } from "./editor";
import { StepInspector } from "./StepInspector";
import type { Capabilities, Graph } from "./types";
import { canWire, COMPONENT_TRANSFER, MOVE_TRANSFER, sourceKind, sourceName, sourcePort, type WireSource, type WireTarget } from "./wiring";

const compact = "(max-width: 720px)";
const clamp = (point: number) => Math.min(BOARD_LIMIT, Math.max(12, Math.round(point / 4) * 4));

function transferable<T>(transfer: DataTransfer, type: string): T | undefined {
  const serialized = transfer.getData(type);
  if (!serialized || serialized.length > 512) return;
  try { return JSON.parse(serialized) as T; }
  catch { return undefined; }
}

export function Canvas({ graph, capabilities, workflowId, onChange }: {
  graph: Graph; capabilities: Capabilities; workflowId: string; onChange: (graph: Graph) => void;
}) {
  const region = useRef<HTMLDivElement>(null);
  const launcher = useRef<HTMLButtonElement>(null);
  const storageKey = `tokencraft:workflow-canvas:${workflowId}`;
  const [positions, setPositions] = useState(() => loadPositions(storageKey));
  const [selected, setSelected] = useState<string | undefined>(graph.nodes[0]?.id);
  const [field, setField] = useState<string>();
  const [layout, setLayout] = useState<"canvas" | "steps">();
  const [isCompact, setIsCompact] = useState(() => matchMedia(compact).matches);
  const [library, setLibrary] = useState(false);
  const [source, setSource] = useState<WireSource>();
  const [feedback, setFeedback] = useState("");
  const spatial = !isCompact && layout !== "steps";
  const step = graph.nodes.find((node) => node.id === selected);
  const cap = graph.nodes.reduce((sum, node) => sum + node.max_usd_nanos, 0);
  const activeSource = source && sourceKind(graph, source) ? source : undefined;
  const active = activeSource && sourceName(graph, activeSource);

  useEffect(() => {
    const media = matchMedia(compact);
    const update = () => setIsCompact(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  useEffect(() => {
    try { localStorage.setItem(storageKey, JSON.stringify(positions)); }
    catch { /* Wiring remains usable when browser storage is unavailable. */ }
  }, [positions, storageKey]);

  const cards = placeCards(graph.nodes, positions);
  const width = Math.max(...cards.map(({ position }) => position.x + CARD_WIDTH + 20), 660);
  const height = Math.max(...cards.map(({ position }) => position.y + CARD_ROW), 366);

  function closeLibrary() {
    setLibrary(false);
    launcher.current?.focus({ preventScroll: true });
  }

  function inspect(id?: string, setting?: string) {
    setSelected(id);
    setField(setting);
  }

  function add(choice: ComponentChoice, position?: Position) {
    if (graph.nodes.length >= 20) return;
    let next: Graph;
    if (choice.kind === "tool") {
      const binding = capabilities.tools?.find((item) => item.revision === choice.binding);
      const operation = binding?.manifest.tools.find((item) => item.id === choice.model && item.revision === choice.revision);
      if (!binding || !operation) return;
      next = addTool(graph, binding, operation);
    } else {
      if (!capabilities.primitives.some((primitive) => primitive.kind === choice.kind)) return;
      if (choice.model && !capabilities.models.some((model) => model.kind === choice.kind && model.id === choice.model)) return;
      next = addStep(graph, choice.kind, capabilities, choice.model);
    }
    const added = next.nodes[next.nodes.length - 1];
    if (position) setPositions((previous) => ({ ...previous, [added.id]: position }));
    inspect(added.id);
    onChange(next);
    setFeedback(`${added.title} added.`);
    if (!position) requestAnimationFrame(() => {
      region.current?.querySelector(`[data-canvas-card="${added.id}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
    });
  }

  function drop(event: DragEvent<HTMLDivElement>) {
    const area = region.current?.getBoundingClientRect();
    if (!area) return;
    const move = transferable<{ id: string; offsetX: number; offsetY: number }>(event.dataTransfer, MOVE_TRANSFER);
    if (move && graph.nodes.some((node) => node.id === move.id)
        && Number.isFinite(move.offsetX) && Number.isFinite(move.offsetY)) {
      event.preventDefault();
      setPositions((previous) => ({ ...previous, [move.id]: {
        x: clamp(event.clientX - area.left - move.offsetX), y: clamp(event.clientY - area.top - move.offsetY),
      } }));
      return;
    }
    const choice = transferable<ComponentChoice>(event.dataTransfer, COMPONENT_TRANSFER);
    if (!choice || typeof choice !== "object") return;
    event.preventDefault();
    add(choice, spatial ? { x: clamp(event.clientX - area.left - 16), y: clamp(event.clientY - area.top - 18) } : undefined);
  }

  function choose(incoming: WireSource, activate = false) {
    setFeedback("");
    setSource((previous) => !activate && previous && sourcePort(previous) === sourcePort(incoming) ? undefined : incoming);
  }

  function connect(incoming: WireSource, target: WireTarget) {
    if (!canWire(graph, incoming, target)) {
      setFeedback("Choose a matching input that keeps the workflow acyclic.");
      return;
    }
    const model = capabilities.models.find((entry) => entry.kind === target.step.kind && entry.id === target.step.model);
    if (target.setting.name === "first_frame" && model && !model.accepts_first_frame) {
      inspect(target.step.id, target.setting.name);
      setFeedback("Choose a video model that accepts a first frame.");
      return;
    }
    onChange(updateStep(graph, setBinding(target.step, target.setting, incoming)));
    inspect(target.step.id, target.setting.name);
    setSource(undefined);
    setFeedback(`${target.setting.label} connected to ${sourceName(graph, incoming)}.`);
  }

  function expose(incoming: WireSource, name?: string) {
    if (incoming.source !== "step" || !graph.nodes.some((node) => node.id === incoming.step)) {
      setFeedback("Choose a step output to return in the response.");
      return;
    }
    const existing = Object.keys(graph.outputs);
    if (!name && existing.length >= 20) {
      setFeedback("The workflow already has 20 response outputs.");
      return;
    }
    const key = name ?? (existing.includes(incoming.step) ? nextId("output", existing) : incoming.step);
    onChange({ ...graph, outputs: { ...graph.outputs, [key]: incoming.step } });
    setSource(undefined);
    setFeedback(`Response output ${key} connected.`);
  }

  function remove() {
    if (!step) return;
    const nodes = graph.nodes.filter((node) => node.id !== step.id).map((node) => ({
      ...node, settings: Object.fromEntries(Object.entries(node.settings)
        .filter(([, binding]) => binding.source !== "step" || binding.step !== step.id)),
    }));
    const outputs = Object.fromEntries(Object.entries(graph.outputs).filter(([, id]) => id !== step.id));
    onChange(pruneUnusedToolInputs({ ...graph, nodes, outputs }));
    setPositions((previous) => {
      const remaining = { ...previous };
      delete remaining[step.id];
      return remaining;
    });
    if (source?.source === "step" && source.step === step.id) setSource(undefined);
    inspect();
  }

  const wire = { graph, source: activeSource, choose, connect, expose, inspect };
  return <section className="wf-editor" onKeyDown={(event) => {
    if (event.key === "Escape" && active) {
      setSource(undefined);
      setFeedback("");
    }
  }}>
    <div className="wf-canvas-shell">
      <div className="wf-toolbar">
        <div className="wf-segment" aria-label="Editor layout">
          <button type="button" aria-pressed={spatial} disabled={isCompact} onClick={() => setLayout("canvas")}>Canvas</button>
          <button type="button" aria-pressed={!spatial} onClick={() => setLayout("steps")}>Steps</button>
        </div>
        <div className="wf-library">
          <button ref={launcher} type="button" className="wf-button" aria-expanded={library}
            onClick={() => setLibrary(!library)}><Plus size={15} /> Add component</button>
          {library && <ComponentLibrary capabilities={capabilities} canAdd={graph.nodes.length < 20}
            onAdd={(choice) => add(choice)} onClose={closeLibrary} />}
        </div>
      </div>
      <div className="wf-canvas-status" role="status">
        <span>{feedback || (active ? `Wiring from ${active}. Choose a compatible input.` : "Select or drag an output to connect it.")}</span>
        {active && <button type="button" className="wf-icon-button" aria-label="Cancel wiring" onClick={() => {
          setSource(undefined);
          setFeedback("");
        }}><X size={14} /></button>}
      </div>
      <div className="wf-board-viewport">
        <div className={`wf-board ${spatial ? "wf-board-spatial" : "wf-board-steps"}`} ref={region}
          style={spatial ? { minWidth: width, height } : undefined} aria-label="Workflow graph"
          onDragOver={(event) => {
            const types = event.dataTransfer.types;
            if (!types.includes(COMPONENT_TRANSFER) && !types.includes(MOVE_TRANSFER)) return;
            event.preventDefault();
            event.dataTransfer.dropEffect = types.includes(MOVE_TRANSFER) ? "move" : "copy";
          }} onDrop={drop}>
          {spatial && <Connections graph={graph} selected={selected} layout={positions} />}
          {cards.map(({ id, node, position }) => {
            const chosen = node ? selected === id : !step;
            return <article className="wf-canvas-card" data-canvas-card={id} data-selected={chosen || undefined}
              key={id} style={spatial ? { left: position.x, top: position.y } : undefined}>
              {node ? <StepCard step={node} primitive={capabilities.primitives.find((item) => item.kind === node.kind)}
                wire={wire} selected={chosen} spatial={spatial} />
                : id === ":inputs" ? <InputCard wire={wire} selected={chosen} /> : <OutputCard wire={wire} selected={chosen} />}
            </article>;
          })}
          {graph.nodes.length === 0 && <button className="wf-add-empty" type="button" onClick={() => setLibrary(true)}>
            <Plus size={18} /> Add your first component
          </button>}
        </div>
      </div>
      <div className="wf-canvas-footer">
        <span>{graph.nodes.length} {graph.nodes.length === 1 ? "step" : "steps"}</span>
        <span className={cap > graph.max_usd_nanos ? "wf-error" : ""}>{usd(cap)} allocated</span>
        <label>Run cap (USD) <input aria-label="Run spending cap (USD)" type="number" min="0" max="100" step="0.000001"
          value={graph.max_usd_nanos / 1e9} onChange={(event) => onChange({
            ...graph, max_usd_nanos: Math.round(Number(event.target.value) * 1e9),
          })} /></label>
      </div>
    </div>
    {step ? <StepInspector key={step.id} graph={graph} step={step} capabilities={capabilities}
      onChange={onChange} onRemove={remove} focusedField={field} />
      : <ContractEditor graph={graph} onChange={onChange} />}
  </section>;
}
