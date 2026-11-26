import { useEffect, useRef, useState } from "react";
import { Play, RefreshCw } from "lucide-react";
import { API_BASE_URL } from "@/lib/api";
import { errorMessage, type ProjectScope } from "@/lib/gateway";
import { workflowApi } from "./api";
import { usd } from "./editor";
import { Dialog } from "./IntentDialog";
import { statusLabel, TERMINAL, type Graph, type Run, type RunSummary, type Scalar, type WorkflowInput } from "./types";

export function TestDialog({ graph, onClose, onRun }: {
  graph: Graph; onClose: () => void; onRun: (inputs: Record<string, Scalar>, key: string) => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const intent = useRef<{ serialized: string; key: string } | undefined>(undefined);
  const inputs = Object.entries(graph.inputs);
  const optional = inputs.filter(([, input]) => input.default !== null);
  const field = ([name, input]: [string, WorkflowInput]) => <label className="wf-field" key={name}>
    {input.title} <code>{name}</code>
    {input.kind === "integer"
      ? <input name={name} type="number" min={1} max={32000} defaultValue={input.default ?? ""} required={input.default === null} />
      : <textarea name={name} rows={2} maxLength={4000} defaultValue={input.default ?? ""} required={input.default === null} />}
  </label>;
  return <Dialog title="Test draft" onClose={onClose}>
    <form className="wf-stack" onSubmit={async (event) => {
      event.preventDefault();
      const form = new FormData(event.currentTarget);
      const inputs: Record<string, Scalar> = {};
      for (const [name, definition] of Object.entries(graph.inputs)) {
        const raw = String(form.get(name) ?? "").trim();
        if (raw !== "") inputs[name] = definition.kind === "integer" ? Number(raw) : raw;
      }
      const serialized = JSON.stringify(inputs);
      if (intent.current?.serialized !== serialized) intent.current = { serialized, key: crypto.randomUUID() };
      setBusy(true); setError("");
      try { await onRun(inputs, intent.current.key); }
      catch (failure) { setError(errorMessage(failure)); setBusy(false); }
    }}>
      <p className="wf-muted">Runs the saved draft with the selected steps. The maximum spend is {usd(graph.max_usd_nanos)}.</p>
      {inputs.filter(([, input]) => input.default === null).map(field)}
      {optional.length > 0 && <details className="wf-disclosure">
        <summary>Optional inputs <span>{optional.length}</span></summary>
        <div className="wf-stack">{optional.map(field)}</div>
      </details>}
      {error && <p role="alert" className="wf-error">{error}</p>}
      <footer className="wf-actions"><button className="wf-button wf-primary" type="submit" disabled={busy}><Play size={15} />{busy ? "Starting…" : "Start test run"}</button></footer>
    </form>
  </Dialog>;
}

export function Status({ status }: { status: RunSummary["status"] }) {
  return <span className={`wf-status wf-status-${status}`}>{statusLabel[status]}</span>;
}

function RunDetails({ run, graph, onCancel }: { run: Run; graph: Graph; onCancel: () => Promise<void> }) {
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const steps = [
    ...run.steps.map((step) => ({ node: step.node, status: step.status, detail: step.request })),
    ...Object.entries(run.tools ?? {}).map(([node, step]) => ({
      node, status: step.status, detail: step.request_id,
    })),
  ];
  return <div className="wf-panel wf-stack">
    <div className="wf-panel-heading"><h3>Run timeline</h3><Status status={run.status} /></div>
    <div className="wf-muted">{run.workflow_version === null ? `Draft ${run.draft_revision}` : `Version ${run.workflow_version}`}</div>
    {steps.length > 0 && <ol className="wf-timeline">{steps.map((step) =>
      <li key={step.node}><span>{graph.nodes.find((item) => item.id === step.node)?.title ?? step.node}<small>{step.detail}</small></span>
        <span>{step.status.replaceAll("_", " ")}</span></li>)}</ol>}
    {run.error && <p className="wf-error" role="alert">{run.error.replaceAll("_", " ")}</p>}
    {run.outputs && <div className="wf-stack"><h4>Outputs</h4>
      {Object.entries(run.outputs).map(([name, output]) => <div className="wf-run-output" key={name}>
        <code>{name}</code>
        {output.type === "text" ? <p>{output.text}</p>
          : output.type === "media" ? <a href={API_BASE_URL + output.download_url}>Download {output.kind}</a>
          : <span className="wf-muted">Synthetic {output.kind} result</span>}
      </div>)}
    </div>}
    {!TERMINAL.has(run.status) && <div>
      <button className="wf-button wf-danger" disabled={busy} type="button" onClick={async () => {
        setBusy(true); setError("");
        try { await onCancel(); }
        catch (failure) { setError(errorMessage(failure)); }
        finally { setBusy(false); }
      }}>Cancel run</button>
      {error && <p role="alert" className="wf-error">{error}</p>}
    </div>}
  </div>;
}

export function Runs({ scope, id, graph, selected, onSelect }: {
  scope: ProjectScope; id: string; graph: Graph; selected: string | undefined; onSelect: (id: string) => void;
}) {
  const [list, setList] = useState<RunSummary[]>();
  const [run, setRun] = useState<Run>();
  const [error, setError] = useState("");
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    workflowApi.runs(scope, id, controller.signal).then(setList).catch((failure) => {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    });
    return () => controller.abort();
  }, [scope, id, refresh]);

  useEffect(() => {
    if (!selected) return;
    const controller = new AbortController();
    let handle: number | undefined;
    async function observe() {
      try {
        const current = await workflowApi.run(scope, id, selected!, controller.signal);
        if (controller.signal.aborted) return;
        setRun(current);
        setList((previous) => previous?.map((item) => item.id === current.id ? { ...item, status: current.status } : item));
        if (!TERMINAL.has(current.status)) handle = window.setTimeout(() => void observe(), 2000);
      } catch {
        if (!controller.signal.aborted) handle = window.setTimeout(() => void observe(), 5000);
      }
    }
    void observe();
    return () => { controller.abort(); if (handle !== undefined) window.clearTimeout(handle); };
  }, [scope, id, selected, refresh]);

  return <div className="wf-grid">
    <section className="wf-panel">
      <div className="wf-panel-heading"><h3>Recent runs</h3><button type="button" className="wf-button wf-small"
        onClick={() => { setError(""); setRefresh((value) => value + 1); }}><RefreshCw size={14} /> Refresh</button></div>
      {error && <p className="wf-error" role="alert">{error}</p>}
      {list?.map((item) => <button key={item.id} type="button" className="wf-run-row"
        aria-pressed={item.id === selected} onClick={() => onSelect(item.id)}>
        <span><code>{item.id}</code><small>{item.version === null ? `Draft ${item.draft_revision}` : `Version ${item.version}`}</small></span>
        <Status status={item.status} />
      </button>)}
      {list?.length === 0 && <p className="wf-muted">Run a draft or call a published endpoint to see results here.</p>}
    </section>
    {run && run.id === selected && <RunDetails key={run.id} run={run} graph={graph} onCancel={async () => {
      await workflowApi.cancel(scope, id, run.id); setRefresh((value) => value + 1);
    }} />}
  </div>;
}
