import { nextId, pruneUnusedToolInputs, updateStep } from "./editor";
import type { Graph, Scalar, Step, ToolOperation, ToolParameter, ToolStep } from "./types";

function numericMinimum(parameter: Exclude<ToolParameter, { type: "string" }>) {
  const stride = parameter.multipleOf ?? 1;
  return parameter.multipleOf ? Math.ceil(parameter.minimum / stride) * stride : parameter.minimum;
}

export function toolParameterExample(parameter: ToolParameter): Scalar {
  return parameter.default ?? (parameter.type === "string" ? parameter.enum[0] : numericMinimum(parameter));
}

export function ToolParameterInput({ parameter, value, defaultValue, onChange, name, label, required }: {
  parameter: ToolParameter;
  value?: Scalar | null;
  defaultValue?: Scalar | null;
  onChange?: (value?: Scalar) => void;
  name?: string;
  label?: string;
  required?: boolean;
}) {
  const selection = onChange ? { value: value ?? "" } : { defaultValue: defaultValue ?? "" };
  if (parameter.type === "string") {
    return <select {...selection} name={name} aria-label={label} required={required} onChange={(event) =>
      onChange?.(event.target.value || undefined)}>
      <option value="">{parameter.default ?? "Choose a value"}</option>
      {parameter.enum.map((item) => <option key={item} value={item}>{item}</option>)}
    </select>;
  }
  return <input {...selection} name={name} aria-label={label} required={required} type="number"
    min={numericMinimum(parameter)} max={parameter.maximum}
    step={parameter.multipleOf ?? (parameter.type === "integer" ? 1 : "any")}
    placeholder={parameter.default?.toString()} onChange={(event) => {
      const number = event.target.valueAsNumber;
      onChange?.(Number.isFinite(number) ? number : undefined);
    }} />;
}

export function ToolParameters({ graph, step, tool, operation, onChange }: {
  graph: Graph; step: Step; tool: ToolStep; operation: ToolOperation; onChange: (graph: Graph) => void;
}) {
  const properties = Object.entries(operation.parameters.properties);
  if (!properties.length) return null;

  function fixedParameter(name: string, value?: Scalar) {
    const parameters = { ...tool.parameters };
    if (value === undefined) delete parameters[name];
    else parameters[name] = value;
    const parameter_inputs = { ...tool.parameter_inputs };
    delete parameter_inputs[name];
    const updated = updateStep(graph, { ...step, tool: { ...tool, parameters, parameter_inputs } });
    onChange(tool.parameter_inputs?.[name] ? pruneUnusedToolInputs(updated) : updated);
  }

  function exposeParameter(name: string, parameter: ToolParameter) {
    const requestKey = nextId(name, Object.keys(graph.inputs));
    const parameters = { ...tool.parameters };
    const value = parameters[name] ?? parameter.default ?? null;
    delete parameters[name];
    const parameter_inputs = { ...tool.parameter_inputs, [name]: requestKey };
    const inputs: Graph["inputs"] = { ...graph.inputs, [requestKey]: {
      title: `${step.title} · ${name.replaceAll("_", " ")}`.slice(0, 100),
      kind: "parameter", default: value, parameter,
    } };
    onChange(updateStep({ ...graph, inputs }, { ...step, tool: { ...tool, parameters, parameter_inputs } }));
  }

  return <div className="wf-stack">
    <h4>Parameters</h4>
    {properties.map(([name, parameter]) => {
      const input = tool.parameter_inputs?.[name];
      const label = name.replaceAll("_", " ");
      const required = operation.parameters.required.includes(name);
      return <fieldset className="wf-binding" key={name}>
        <legend>{label}{required ? " (required)" : ""}</legend>
        {input ? <>
          <small>Request input <code>{input}</code></small>
          <button type="button" className="wf-button wf-small" onClick={() =>
            fixedParameter(name, graph.inputs[input]?.default ?? undefined)}>Use fixed value</button>
        </> : <>
          <ToolParameterInput parameter={parameter} value={tool.parameters[name]} label={label}
            onChange={(value) => fixedParameter(name, value)} required={required} />
          <button type="button" className="wf-button wf-small" disabled={Object.keys(graph.inputs).length >= 24}
            onClick={() => exposeParameter(name, parameter)}>Expose as request input</button>
        </>}
      </fieldset>;
    })}
  </div>;
}
