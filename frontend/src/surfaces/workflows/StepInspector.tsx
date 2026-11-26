import { useEffect, useRef, useState } from "react";
import { Plus, Search, Sparkles, Trash2 } from "lucide-react";
import { MODEL_BY_SLUG } from "@/lib/catalog";
import { modelName } from "./ComponentLibrary";
import { errorMessage, gatewayRequest } from "@/lib/gateway";
import { catalogAdvice, type Advice } from "@/surfaces/models/model-advisor";
import { bindingModes, defaultBinding, nextId, setBinding, updateStep } from "./editor";
import { acceptsKind, canWire, isFileInput, stepSettings } from "./wiring";
import type { Binding, Capabilities, Graph, Scalar, Setting, Step, ToolStep, WorkflowModel } from "./types";

function ModelSearch({ models, step, framesOnly, onSelect }: {
  models: WorkflowModel[]; step: Step; framesOnly: boolean; onSelect: (id: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [advice, setAdvice] = useState<Advice>();
  const [error, setError] = useState("");
  const request = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => request.current?.abort(), []);
  const eligible = models.filter((model) => model.kind === step.kind && (!framesOnly || model.accepts_first_frame));
  const match = eligible.filter((model) => [modelName(model.id), model.id, ...model.providers]
    .some((value) => value.toLowerCase().includes(query.toLowerCase())));

  async function advise() {
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setError("");
    const job = `${step.title}. Recommend models for this ${step.kind} step${framesOnly ? " with an image first frame" : ""}.`;
    const ids = eligible.map(({ id }) => id).filter((id) => Object.hasOwn(MODEL_BY_SLUG, id)).slice(0, 32);
    const fallback = () => {
      const hints = catalogAdvice(job).recommendations.filter(({ slug }) => eligible.some(({ id }) => id === slug));
      return { sources: [], recommendations: hints.length ? hints : eligible.slice(0, 3).map(({ id, providers }) => ({
        slug: id, reason: `Configured for this step through ${providers.join(" or ")}.`,
      })) };
    };
    try {
      const data = ids.length
        ? await gatewayRequest<Advice>("/v1/model-advisor", {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ job, catalog_candidates: ids }), signal: controller.signal,
        }) : fallback();
      if (!controller.signal.aborted) {
        const recommendations = data.recommendations.filter(({ slug }) => eligible.some(({ id }) => id === slug));
        setAdvice(recommendations.length ? { ...data, recommendations } : fallback());
      }
    } catch (failure) {
      if (!controller.signal.aborted) {
        setAdvice(fallback());
        setError(`${errorMessage(failure)} Showing configured catalog matches.`);
      }
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return <div className="wf-stack">
    <div className="wf-model-selection">
      <div><small>Model</small><strong>{modelName(step.model)}</strong></div>
      <button className="wf-button wf-small" type="button" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Search size={14} /> Search
      </button>
    </div>
    {open && <section className="wf-model-options" aria-label="Compatible models">
      <input className="wf-control" type="search" autoFocus aria-label="Search compatible models" placeholder="Search models or providers"
        value={query} onChange={(event) => setQuery(event.target.value)} />
      {match.map((model) => <button className="wf-option" key={model.id} type="button" aria-pressed={model.id === step.model}
        onClick={() => { onSelect(model.id); setOpen(false); }}>
        <strong>{modelName(model.id)}</strong><small>{model.providers.join(" · ")}</small>
      </button>)}
      {!match.length && <p className="wf-muted">No compatible models match this step.</p>}
    </section>}
    <button type="button" className="wf-button wf-small" disabled={!eligible.length || busy} onClick={() => void advise()}>
      <Sparkles size={14} /> {busy ? "Researching…" : "Ask Model Advisor"}
    </button>
    {advice && <div className="wf-advice">
      {error && <p role="status" className="wf-muted">{error}</p>}
      {advice.recommendations.map((model) => <article key={model.slug} className="wf-advice-item">
        <strong>{modelName(model.slug)}</strong><p>{model.reason}</p>
        {model.tradeoff && <p className="wf-muted">{model.tradeoff}</p>}
        <button className="wf-button wf-small" type="button" onClick={() => onSelect(model.slug)}>Select model</button>
      </article>)}
    </div>}
  </div>;
}

function BindingEditor(props: {
  graph: Graph; step: Step; setting: Setting; model?: WorkflowModel; focused?: boolean; onChange: (graph: Graph) => void;
}) {
  const { graph, step, setting, model, focused, onChange } = props;
  const binding = step.settings[setting.name];
  const remembered = useRef<Partial<Record<Binding["source"], Binding>>>({});
  const connected = graph.nodes.filter((node) => canWire(graph, { source: "step", step: node.id }, { step, setting }));
  const requestInputs = Object.entries(graph.inputs).filter(([, field]) => acceptsKind(setting.kind, field.kind));
  const assigned = (value?: Binding) => onChange(updateStep(graph, setBinding(step, setting, value)));
  const choices = setting.name === "aspect_ratio" ? model?.aspect_ratios
    : setting.name === "resolution" ? model?.resolutions
    : setting.name === "duration_seconds" ? model?.durations : undefined;

  function addInput() {
    if (setting.kind !== "text" && setting.kind !== "integer" && !isFileInput(setting.kind)) return;
    const key = nextId(setting.name, Object.keys(graph.inputs));
    onChange(updateStep({ ...graph, inputs: { ...graph.inputs, [key]: {
      title: setting.label, kind: setting.kind, default: null,
    } } }, setBinding(step, setting, { source: "request", field: key })));
  }

  const unavailableFrame = setting.name === "first_frame" && model && !model.accepts_first_frame;
  return <fieldset className="wf-binding" data-focused={focused || undefined}>
    <legend>{setting.label}</legend>
    {unavailableFrame && <small>Choose a video model that accepts a first frame to wire this input.</small>}
    <select aria-label={`${setting.label} source`} value={binding?.source ?? ""} onChange={(event) => {
      const source = event.target.value as Binding["source"] | "";
      if (binding) remembered.current[binding.source] = binding;
      if (!source) {
        assigned();
        return;
      }
      const previous = remembered.current[source];
      const available = previous && (previous.source !== "step" && previous.source !== "request"
        || canWire(graph, previous, { step, setting }));
      assigned(available ? previous : defaultBinding(source, setting, graph, step));
    }}>
      <option value="" disabled={setting.required}>Unset</option>
      {bindingModes(setting).map(({ value, label }) =>
        <option key={value} value={value} disabled={!!unavailableFrame}>{label}</option>)}
    </select>
    {binding?.source === "fixed" && (choices?.length
      ? <select aria-label={`${setting.label} value`} value={binding.value} onChange={(event) =>
        assigned({ source: "fixed", value: setting.kind === "integer" ? Number(event.target.value) : event.target.value })}>
        {!choices.some((choice) => choice === binding.value) && <option value={binding.value}>{String(binding.value) || "Choose a value"}</option>}
        {choices.map((choice) => <option value={choice} key={choice}>{choice}</option>)}
      </select>
      : setting.kind === "integer"
        ? <input aria-label={`${setting.label} value`} type="number" min={1} max={32000} value={binding.value}
          onChange={(event) => assigned({ source: "fixed", value: Number(event.target.value) })} />
        : <textarea aria-label={`${setting.label} value`} rows={setting.name === "prompt" || setting.name === "system" ? 3 : 1}
          value={binding.value} onChange={(event) => assigned({ source: "fixed", value: event.target.value })} />)}
    {binding?.source === "request" && <div className="wf-stack">
      <select aria-label={`${setting.label} request input`} value={binding.field}
        onChange={(event) => assigned({ source: "request", field: event.target.value })}>
        <option value="">Choose a request field</option>
        {requestInputs.map(([name, input]) => <option key={name} value={name}>{input.title} · {name}</option>)}
      </select>
      <button type="button" className="wf-button wf-small" disabled={Object.keys(graph.inputs).length >= 24} onClick={addInput}><Plus size={14} /> Create request field</button>
    </div>}
    {binding?.source === "step" && <select aria-label={`${setting.label} connected step`} value={binding.step}
      onChange={(event) => assigned({ source: "step", step: event.target.value })}>
      <option value="">Choose a compatible step</option>
      {connected.map((node) => <option key={node.id} value={node.id}>{node.title}</option>)}
    </select>}
    {binding?.source === "template" && <>
      <textarea aria-label={`${setting.label} template`} rows={3} value={binding.text}
        onChange={(event) => assigned({ source: "template", text: event.target.value })} />
      <small>Insert a value with <code>{"{{input.brief}}"}</code> or <code>{"{{step.text_1}}"}</code>.</small>
    </>}
  </fieldset>;
}

export function StepInspector({ graph, step, capabilities, onChange, onRemove, focusedField }: {
  graph: Graph; step: Step; capabilities: Capabilities; onChange: (graph: Graph) => void; onRemove: () => void; focusedField?: string;
}) {
  const advanced = useRef<HTMLDetailsElement>(null);
  const model = capabilities.models.find((item) => item.id === step.model && item.kind === step.kind);
  const primitive = capabilities.primitives.find((item) => item.kind === step.kind);
  const settings = stepSettings(step, primitive?.settings);
  const required = settings.filter((setting) => setting.required);
  const optional = settings.filter((setting) => !setting.required);
  const binding = capabilities.tools?.find((item) => item.revision === step.tool?.binding_revision);
  const operation = binding?.manifest.tools.find((item) => item.id === step.model && item.revision === step.tool?.revision);
  const optionalConfigured = optional.filter(({ name }) => !!step.settings[name]).length;
  const focusIsOptional = optional.some(({ name }) => name === focusedField);

  useEffect(() => {
    if (focusIsOptional && advanced.current) advanced.current.open = true;
  }, [focusedField, focusIsOptional]);

  const field = (setting: Setting) => <BindingEditor key={setting.name} graph={graph} step={step}
    setting={setting} model={model} onChange={onChange} focused={focusedField === setting.name} />;
  function changeParameter(tool: ToolStep, name: string, value?: Scalar) {
    const parameters = { ...tool.parameters };
    if (value === undefined) delete parameters[name];
    else parameters[name] = value;
    onChange(updateStep(graph, { ...step, tool: { ...tool, parameters } }));
  }

  const tool = step.tool;
  return <aside className="wf-inspector" aria-label="Step inspector">
    <div className="wf-inspector-heading"><span>{tool ? "Media tool" : (primitive?.label ?? step.kind)}</span><h3>{step.title}</h3></div>
    <label className="wf-field">Step name<input value={step.title} maxLength={100}
      onChange={(event) => onChange(updateStep(graph, { ...step, title: event.target.value }))} /></label>
    {tool ? <div className="wf-model-selection"><div>
      <small>{binding?.name ?? "Binding unavailable"} · Revision {tool.revision}</small>
      <strong>{operation?.label ?? step.model}</strong>
    </div></div> : <ModelSearch step={step} models={capabilities.models}
      framesOnly={!!step.settings.first_frame || focusedField === "first_frame"}
      onSelect={(id) => onChange(updateStep(graph, { ...step, model: id }))} />}
    {required.length > 0 && <h4>Required inputs</h4>}
    {required.map(field)}
    {tool && operation && Object.entries(operation.parameters.properties).length > 0 && <div className="wf-stack">
      <h4>Parameters</h4>
      {Object.entries(operation.parameters.properties).map(([name, parameter]) => {
        const required = operation.parameters.required.includes(name);
        const value = tool.parameters[name] ?? "";
        return <label key={name} className="wf-field">{name.replaceAll("_", " ")}{required ? " (required)" : ""}
          {parameter.type === "string" ? <select value={value} required={required} onChange={(event) =>
            changeParameter(tool, name, event.target.value || undefined)}>
            <option value="">{parameter.default ?? "Choose a value"}</option>
            {parameter.enum.map((item) => <option key={item} value={item}>{item}</option>)}
          </select> : <input type="number" min={parameter.minimum} max={parameter.maximum}
            step={parameter.multipleOf ?? (parameter.type === "integer" ? 1 : "any")} required={required}
            value={value} placeholder={parameter.default?.toString()} onChange={(event) =>
              changeParameter(tool, name, event.target.value === "" ? undefined : event.target.valueAsNumber)} />}
        </label>;
      })}
    </div>}
    {!tool && <details className="wf-disclosure" ref={advanced}>
      <summary>Optional &amp; advanced {optionalConfigured > 0 && <span>{optionalConfigured} set</span>}</summary>
      <div className="wf-stack">
        {optional.map(field)}
        <label className="wf-field">Step spending cap (USD)<input type="number" min="0" max="100" step="0.000001"
          value={step.max_usd_nanos / 1e9}
          onChange={(event) => onChange(updateStep(graph, { ...step, max_usd_nanos: Math.round(Number(event.target.value) * 1e9) }))} /></label>
      </div>
    </details>}
    <button type="button" className="wf-button wf-danger wf-small" onClick={onRemove}><Trash2 size={14} /> Remove step</button>
  </aside>;
}
