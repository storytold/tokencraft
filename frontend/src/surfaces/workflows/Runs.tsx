import { useEffect, useRef, useState } from "react";
import { Play, RefreshCw } from "lucide-react";
import { API_BASE_URL } from "@/lib/api";
import { errorMessage, type ProjectScope } from "@/lib/gateway";
import { workflowApi } from "./api";
import { usd } from "./editor";
import { Dialog } from "./IntentDialog";
import { ToolParameterInput } from "./ToolParameters";
import { isFileInput } from "./wiring";
import { statusLabel, TERMINAL, type Graph, type Run, type RunSummary, type InvocationInput, type UploadKind, type UploadReference, type WorkflowInput } from "./types";

const fileTypes: Record<Exclude<UploadKind, "media">, Record<string, string>> = {
  image: { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp", gif: "image/gif" },
  video: { mp4: "video/mp4", webm: "video/webm", mov: "video/quicktime" },
  audio: { wav: "audio/wav", mp3: "audio/mpeg", flac: "audio/flac", ogg: "audio/ogg", opus: "audio/ogg" },
  mesh: { glb: "model/gltf-binary" },
  document: { pdf: "application/pdf", txt: "text/plain", srt: "text/plain", vtt: "text/vtt", gltf: "model/gltf+json" },
};

function extensions(kind: UploadKind): Record<string, string> {
  return kind === "media" ? Object.assign({}, ...Object.values(fileTypes)) : fileTypes[kind];
}

async function fileDigest(file: File) {
  const hash = await crypto.subtle.digest("SHA-256", await file.arrayBuffer());
  return Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function TestDialog({ graph, onClose, onUpload, onRun }: {
  graph: Graph;
  onClose: () => void;
  onUpload: (file: File, type: string, digest: string) => Promise<UploadReference>;
  onRun: (inputs: Record<string, InvocationInput>, key: string) => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const intent = useRef<{ serialized: string; key: string } | undefined>(undefined);
  const inputs = Object.entries(graph.inputs);
  const optional = inputs.filter(([, input]) => input.default !== null);
  const field = ([name, input]: [string, WorkflowInput]) => <label className="wf-field" key={name}>
    {input.title} <code>{name}</code>
    {input.kind === "parameter"
      ? <ToolParameterInput parameter={input.parameter} name={name} defaultValue={input.default} required={input.default === null} />
      : input.kind === "integer"
      ? <input name={name} type="number" min={1} max={32000} defaultValue={input.default ?? ""} required={input.default === null} />
      : isFileInput(input.kind)
        ? <input name={name} type="file" required accept={Object.keys(extensions(input.kind)).map((ext) => "." + ext).join(",")} />
        : <textarea name={name} rows={2} maxLength={4000} defaultValue={input.default ?? ""} required={input.default === null} />}
  </label>;
  return <Dialog title="Test draft" onClose={onClose}>
    <form className="wf-stack" onSubmit={async (event) => {
      event.preventDefault();
      const form = new FormData(event.currentTarget);
      setBusy(true); setError("");
      try {
        const values: Record<string, InvocationInput> = {};
        for (const [name, definition] of inputs) {
          const raw = form.get(name);
          if (isFileInput(definition.kind)) {
            if (!(raw instanceof File) || !raw.size || raw.size > 32 * 1024 * 1024) {
              throw new Error(`Choose a file up to 32 MiB for ${definition.title}.`);
            }
            const types = extensions(definition.kind);
            const type = Object.values(types).find((mime) => mime === raw.type)
              ?? types[raw.name.toLowerCase().split(".").pop() ?? ""];
            if (!type) throw new Error(`Choose a supported file format for ${definition.title}.`);
            const { upload_id } = await onUpload(raw, type, await fileDigest(raw));
            values[name] = { upload_id };
          } else if (definition.kind === "parameter" && typeof raw === "string" && raw !== "") {
            values[name] = definition.parameter.type === "string" ? raw : Number(raw);
          } else if (typeof raw === "string" && raw.trim()) {
            values[name] = definition.kind === "integer" ? Number(raw) : raw.trim();
          }
        }
        const serialized = JSON.stringify(values);
        if (intent.current?.serialized !== serialized) intent.current = { serialized, key: crypto.randomUUID() };
        await onRun(values, intent.current.key);
      } catch (failure) {
        setError(failure instanceof Error ? failure.message : errorMessage(failure));
        setBusy(false);
      }
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
