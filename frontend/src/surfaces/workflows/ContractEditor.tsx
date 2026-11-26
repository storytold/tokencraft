import { useState } from "react";
import { Plus, Trash2 } from "lucide-react";

import { nextId } from "./editor";
import { ToolParameterInput, toolParameterExample } from "./ToolParameters";
import { isFileInput } from "./wiring";
import type { Graph, UploadKind, WorkflowInput } from "./types";

export function ContractEditor({ graph, onChange }: { graph: Graph; onChange: (graph: Graph) => void }) {
  const [outputName, setOutputName] = useState("");
  const [revealOptional, setRevealOptional] = useState(false);
  const [fileKind, setFileKind] = useState<UploadKind>("image");
  function changeInput(name: string, input: WorkflowInput) {
    onChange({ ...graph, inputs: { ...graph.inputs, [name]: input } });
  }
  function addInput(kind: Exclude<WorkflowInput["kind"], "parameter">) {
    const name = nextId(kind === "text" ? "input" : kind === "integer" ? "number" : kind, Object.keys(graph.inputs));
    const title = kind === "text" ? "Text input" : kind === "integer" ? "Numeric input" : `${kind} file`;
    changeInput(name, { title, kind, default: null });
  }
  function removeInput(name: string) {
    const inputs = { ...graph.inputs };
    delete inputs[name];
    onChange({ ...graph, inputs });
  }
  const inputs = Object.entries(graph.inputs);
  const optional = inputs.filter(([, input]) => input.default !== null);
  function field([name, input]: [string, WorkflowInput]) {
    const parameterInUse = input.kind === "parameter" && graph.nodes.some((node) =>
      Object.values(node.tool?.parameter_inputs ?? {}).includes(name));
    return <fieldset className="wf-binding" key={name}>
        <legend><code>{name}</code> · {input.kind}</legend>
        <label className="wf-field">Display name
          <input value={input.title} maxLength={100} onChange={(event) => changeInput(name, { ...input, title: event.target.value })} />
        </label>
        {!isFileInput(input.kind) && <label className="wf-checkbox">
          <input type="checkbox" checked={input.default !== null} onChange={(event) => {
            const checked = event.target.checked;
            if (checked) setRevealOptional(true);
            const initial = input.kind === "parameter" ? toolParameterExample(input.parameter) : input.kind === "integer" ? 1 : "";
            changeInput(name, { ...input, default: checked ? initial : null });
          }} /> Provide a default
        </label>}
        {input.default !== null && (input.kind === "parameter"
          ? <ToolParameterInput parameter={input.parameter} value={input.default} label={`${name} default value`}
            onChange={(value) => changeInput(name, { ...input, default: value ?? null })} />
          : <input aria-label={`${name} default value`} type={input.kind === "integer" ? "number" : "text"}
            value={input.default} onChange={(event) => changeInput(name, {
              ...input, default: input.kind === "integer" ? Number(event.target.value) : event.target.value,
            })} />)}
        {parameterInUse
          ? <small>Managed by the tool step.</small>
          : <button className="wf-button wf-small wf-danger" type="button" onClick={() => removeInput(name)}>Remove field</button>}
      </fieldset>;
  }
  return <aside className="wf-inspector" aria-label="Request and response">
    <div className="wf-inspector-heading"><span>Endpoint contract</span><h3>Inputs and outputs</h3></div>

    <h4>Request inputs</h4>
    {inputs.filter(([, input]) => input.default === null).map(field)}
    {optional.length > 0 && <details className="wf-disclosure" open={revealOptional}
      onToggle={(event) => setRevealOptional(event.currentTarget.open)}>
      <summary>Optional request fields <span>{optional.length}</span></summary>
      <div className="wf-stack">{optional.map(field)}</div>
    </details>}
    <div className="wf-actions">
      <button className="wf-button wf-small" type="button" disabled={inputs.length >= 24} onClick={() => addInput("text")}><Plus size={13} /> Text field</button>
      <button className="wf-button wf-small" type="button" disabled={inputs.length >= 24} onClick={() => addInput("integer")}><Plus size={13} /> Numeric field</button>
    </div>
    <div className="wf-inline-form">
      <select aria-label="File input type" value={fileKind} onChange={(event) => setFileKind(event.target.value as UploadKind)}>
        <option value="image">Image</option><option value="video">Video</option><option value="audio">Audio</option>
        <option value="document">Document</option><option value="mesh">3D mesh</option><option value="media">Any supported file</option>
      </select>
      <button className="wf-button wf-small" type="button" disabled={inputs.length >= 24} onClick={() => addInput(fileKind)}>
        <Plus size={13} /> File field
      </button>
    </div>
    <h4>Response outputs</h4>
    {Object.entries(graph.outputs).map(([name, step]) =>
      <div className="wf-output-edit" key={name}><code>{name}</code>
        <select aria-label={`${name} source`} value={step} onChange={(event) => onChange({
          ...graph, outputs: { ...graph.outputs, [name]: event.target.value },
        })}>
          <option value="" disabled>Choose a step</option>
          {graph.nodes.map((node) => <option key={node.id} value={node.id}>{node.title}</option>)}
        </select>
        <button type="button" className="wf-icon-button" aria-label={`Remove ${name} output`} onClick={() => {
          const outputs = { ...graph.outputs };
          delete outputs[name];
          onChange({ ...graph, outputs });
        }}><Trash2 size={14} /></button>
      </div>)}
    <form className="wf-inline-form" onSubmit={(event) => {
      event.preventDefault();
      if (!graph.nodes[0] || Object.hasOwn(graph.outputs, outputName)) return;
      onChange({ ...graph, outputs: { ...graph.outputs, [outputName]: graph.nodes[0].id } });
      setOutputName("");
    }}>
      <input aria-label="Output name" placeholder="output_name" value={outputName} pattern="[a-z][a-z0-9_]{0,47}"
        onChange={(event) => setOutputName(event.target.value)} required />
      <button type="submit" className="wf-button wf-small" disabled={!graph.nodes.length}>Add</button>
    </form>
  </aside>;
}
