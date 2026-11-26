import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { ArrowLeft, Braces, Download, ListChecks, Play, Plus, Save, Sparkles, Upload, Workflow as WorkflowIcon } from "lucide-react";
import { ApiError } from "@/lib/api";
import { errorMessage, type ProjectScope } from "@/lib/gateway";
import { ProjectStatus, useProject } from "@/surfaces/projects/ProjectScope";
import { workflowApi } from "./api";
import { Canvas } from "./Canvas";
import { EMPTY } from "./editor";
import { Endpoint } from "./Endpoint";
import { Dialog, IntentDialog } from "./IntentDialog";
import { Runs, TestDialog } from "./Runs";
import type { Capabilities, Graph, InvocationInput, Workflow, WorkflowSummary } from "./types";
import "./workflows.css";

const tabs = [["build", "Build", WorkflowIcon], ["runs", "Runs", ListChecks], ["endpoint", "Endpoint", Braces]] as const;
type View = typeof tabs[number][0];

function Editor({ scope, initial, capabilities, admin, onBack }: {
  scope: ProjectScope; initial: Workflow; capabilities: Capabilities; admin: boolean; onBack: (updated: Workflow) => void;
}) {
  const [workflow, setWorkflow] = useState(initial);
  const [graph, setGraph] = useState(initial.draft);
  const [params, setParams] = useSearchParams();
  const run = params.get("run_id") ?? undefined;
  const requestedView = params.get("workflow_view");
  const view = tabs.find(([id]) => id === requestedView)?.[0] ?? (run ? "runs" : "build");
  const [error, setError] = useState("");
  const [saved, setSaved] = useState("");
  const [busy, setBusy] = useState(false);
  const [modal, setModal] = useState<"test" | "reuse" | "publish">();
  const [lastTest, setLastTest] = useState<{ id: string; revision: number; inputs: Record<string, InvocationInput> }>();
  const dirty = graph !== workflow.draft;
  const reuseSource = !dirty && lastTest?.id === run && lastTest?.revision === workflow.revision ? lastTest : undefined;

  function openView(nextView: View, nextRun?: string) {
    setParams((previous) => {
      const next = new URLSearchParams(previous);
      next.set("workflow_view", nextView);
      if (nextRun) next.set("run_id", nextRun);
      return next;
    });
  }

  async function save() {
    if (!dirty) return workflow;
    const next = await workflowApi.save(scope, workflow.id, workflow.revision, graph);
    setWorkflow(next); setGraph(next.draft); setSaved("Draft saved");
    return next;
  }

  async function perform(action: () => Promise<unknown>) {
    setBusy(true); setError(""); setSaved("");
    try { await action(); }
    catch (failure) { setError(errorMessage(failure)); }
    finally { setBusy(false); }
  }

  function exportGraph() {
    const url = URL.createObjectURL(new Blob([JSON.stringify(graph, null, 2)], { type: "application/json" }));
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = "tokencraft-workflow.json";
    anchor.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  return <div className="wf-studio">
    <header className="wf-header">
      <div className="wf-stack">
        <button type="button" className="wf-back" onClick={() => {
          if (!dirty || window.confirm("Discard unsaved draft changes?")) onBack(workflow);
        }}><ArrowLeft size={14} /> Workflows</button>
        <label className="wf-title-field"><span className="wf-sr-only">Workflow name</span><input value={graph.title}
          maxLength={100} onChange={(event) => { setGraph({ ...graph, title: event.target.value }); setSaved(""); }} /></label>
      </div>
      <div className="wf-actions">
        <span className="wf-status">{dirty ? "Unsaved draft" : `Draft ${workflow.revision}`}</span>
        <button className="wf-button" type="button" onClick={exportGraph}><Download size={15} /> Export</button>
        <button className="wf-button" disabled={busy || !dirty} type="button" onClick={() => void perform(save)}><Save size={15} /> Save draft</button>
        <button className="wf-button" disabled={busy || !graph.nodes.length} type="button" onClick={() => void perform(async () => {
          await save(); setModal("test");
        })}><Play size={15} /> Test run</button>
        {admin && <button className="wf-button wf-primary" disabled={busy || !graph.nodes.length} type="button"
          onClick={() => void perform(async () => { await save(); setModal("publish"); })}><Upload size={15} /> Publish</button>}
      </div>
    </header>
    <nav className="wf-tabs" aria-label="Workflow views">
      {tabs.map(([id, label, Icon]) => <button type="button" key={id} aria-current={view === id ? "page" : undefined}
        onClick={() => { openView(id); setError(""); }}><Icon size={15} /> {label}</button>)}
      {workflow.active_version !== null && <span className="wf-live"><span />{workflow.paused ? "Production paused" : `Version ${workflow.active_version} live`}</span>}
    </nav>
    {error && <p className="wf-banner wf-error" role="alert">{error}</p>}
    {saved && <p className="wf-banner wf-muted" role="status">{saved}</p>}
    {view === "build" && <Canvas workflowId={workflow.id} graph={graph} capabilities={capabilities} onChange={(next) => { setGraph(next); setSaved(""); }} />}
    {view === "runs" && <Runs scope={scope} id={workflow.id} graph={workflow.draft} selected={run} onSelect={(id) => openView("runs", id)}
      onReuse={reuseSource ? () => setModal("reuse") : undefined} />}
    {view === "endpoint" && <Endpoint scope={scope} workflow={workflow} admin={admin} onUpdate={(next) => {
      setWorkflow(next);
      if (!dirty) setGraph(next.draft);
    }} />}
    {(modal === "test" || modal === "reuse" && reuseSource) && <TestDialog graph={workflow.draft}
      priorInputs={modal === "reuse" ? reuseSource?.inputs : undefined}
      onUpload={(file, type, digest) => workflowApi.upload(scope, workflow.id, file, type, digest)}
      onClose={() => setModal(undefined)} onRun={async (inputs, key) => {
      const launched = await workflowApi.test(scope, workflow, inputs, key, modal === "reuse" ? reuseSource?.id : undefined);
      setLastTest({ id: launched.id, revision: workflow.revision, inputs });
      openView("runs", launched.id); setModal(undefined);
    }} />}
    {modal === "publish" && <Dialog title="Publish workflow" onClose={() => setModal(undefined)}>
      <div className="wf-stack">
        <p>Draft {workflow.revision} must have a successful test. New external requests will use the published version.</p>
        {error && <p role="alert" className="wf-error">{error}</p>}
        <div className="wf-actions"><button type="button" className="wf-button wf-primary" disabled={busy} onClick={() => void perform(async () => {
          const next = await workflowApi.publish(scope, workflow);
          setWorkflow(next); setGraph(next.draft); setModal(undefined); openView("endpoint");
        })}>Publish endpoint</button></div>
      </div>
    </Dialog>}
  </div>;
}

function Library({ scope, admin }: { scope: ProjectScope; admin: boolean }) {
  const [capabilities, setCapabilities] = useState<Capabilities>();
  const [workflows, setWorkflows] = useState<WorkflowSummary[]>();
  const [params, setParams] = useSearchParams();
  const workflowId = params.get("workflow_id");
  const [opened, setOpened] = useState<{ id: string; workflow?: Workflow; error?: string }>();
  const [openAttempt, setOpenAttempt] = useState(0);
  const [intent, setIntent] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [disabled, setDisabled] = useState(false);
  const [importing, setImporting] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    const tools = workflowApi.tools(scope, controller.signal).catch((failure: unknown) => {
      if (failure instanceof ApiError && failure.status === 404) return [];
      throw failure;
    });
    Promise.all([workflowApi.capabilities(scope, controller.signal), workflowApi.list(scope, controller.signal), tools])
      .then(([available, existing, tools]) => {
        if (controller.signal.aborted) return;
        setCapabilities({ ...available, tools }); setWorkflows(existing);
      }).catch((failure) => {
        if (controller.signal.aborted) return;
        if (failure instanceof ApiError && failure.status === 404) setDisabled(true);
        else setError(errorMessage(failure));
      });
    return () => controller.abort();
  }, [scope]);

  const selection = opened?.id === workflowId ? opened : undefined;
  const hasWorkflow = Boolean(selection?.workflow);
  useEffect(() => {
    if (!workflowId || hasWorkflow) return;
    const controller = new AbortController();
    workflowApi.read(scope, workflowId, controller.signal).then((workflow) => {
      if (!controller.signal.aborted) setOpened({ id: workflowId, workflow });
    }).catch((failure) => {
      if (!controller.signal.aborted) setOpened({ id: workflowId, error: errorMessage(failure) });
    });
    return () => controller.abort();
  }, [scope, workflowId, hasWorkflow, openAttempt]);

  function open(id?: string) {
    setError("");
    if (id && id === workflowId && selection?.error) {
      setOpened(undefined);
      setOpenAttempt((attempt) => attempt + 1);
      return;
    }
    setParams((previous) => {
      const next = new URLSearchParams(previous);
      if (id) next.set("workflow_id", id);
      else next.delete("workflow_id");
      next.delete("run_id");
      next.delete("workflow_view");
      return next;
    });
  }

  async function create(graph: Graph) {
    const created = await workflowApi.create(scope, graph);
    setWorkflows((existing) => [created, ...(existing ?? [])]);
    setOpened({ id: created.id, workflow: created }); open(created.id); setIntent(false);
  }

  if (selection?.workflow && capabilities) {
    return <Editor key={selection.workflow.id} scope={scope} initial={selection.workflow} capabilities={capabilities} admin={admin}
      onBack={(updated) => {
        setWorkflows((list) => list?.map((item) => item.id === updated.id ? updated : item));
        setOpened(undefined);
        open();
      }} />;
  }

  return <div className="wf-studio">
    <header className="wf-header wf-library-header">
      <div><h1>Workflows</h1><p className="wf-muted">Connect models and approved tools, then publish a private endpoint for your application.</p></div>
      {capabilities && <div className="wf-actions">
        <button type="button" className="wf-button wf-primary" disabled={busy} onClick={async () => {
          setBusy(true); setError("");
          try { await create({ ...EMPTY }); }
          catch (failure) { setError(errorMessage(failure)); }
          finally { setBusy(false); }
        }}><Plus size={15} /> Create manually</button>
        <button type="button" className="wf-button" disabled={busy} onClick={() => setImporting(true)}>Import JSON</button>
        <button type="button" className="wf-button" onClick={() => setIntent(true)}><Sparkles size={15} /> Start with intent</button>
      </div>}
    </header>
    <section className="wf-library-list">
      {disabled && <p role="status" className="wf-muted">Workflows is disabled on this gateway. Set TOKENCRAFT_WORKFLOW_KEY to enable it.</p>}
      {!workflows && !disabled && !error && <p role="status" className="wf-muted">Loading workflows…</p>}
      {workflowId && !selection && <p role="status" className="wf-muted">Opening workflow…</p>}
      {selection?.error && <p role="alert" className="wf-error">{selection.error}</p>}
      {error && <p role="alert" className="wf-error">{error}</p>}
      {workflows?.length === 0 && <p className="wf-muted">Create your first workflow to get started.</p>}
      {workflows?.map((item) => <button type="button" className="wf-workflow-row" key={item.id} disabled={busy} onClick={() => open(item.id)}>
        <span className="wf-node-icon"><WorkflowIcon size={17} /></span><strong>{item.title}</strong>
        <span className={`wf-status ${item.active_version && !item.paused ? "wf-status-succeeded" : ""}`}>
          {item.active_version ? item.paused ? "Paused" : `Version ${item.active_version} live` : "Draft"}
        </span>
      </button>)}
    </section>
    {intent && <IntentDialog scope={scope} onCreate={create} onClose={() => setIntent(false)} />}
    {importing && <Dialog title="Import workflow definition" onClose={() => setImporting(false)}>
      <form className="wf-stack" onSubmit={async (event) => {
        event.preventDefault();
        const file = new FormData(event.currentTarget).get("definition");
        if (!(file instanceof File) || !file.size || file.size > 64 * 1024) {
          setError("Choose a workflow JSON file up to 64 KiB.");
          return;
        }
        setBusy(true); setError("");
        try {
          const value: unknown = JSON.parse(await file.text());
          if (!value || typeof value !== "object" || Array.isArray(value)) throw new SyntaxError("Invalid workflow definition");
          await create(value as Graph);
          setImporting(false);
        } catch (failure) { setError(failure instanceof SyntaxError ? "The file is not a valid workflow JSON object." : errorMessage(failure)); }
        finally { setBusy(false); }
      }}>
        <label className="wf-field">Workflow JSON<input name="definition" type="file" accept="application/json,.json" required /></label>
        {error && <p role="alert" className="wf-error">{error}</p>}
        <footer className="wf-actions"><button className="wf-button wf-primary" disabled={busy} type="submit">Import draft</button></footer>
      </form>
    </Dialog>}
  </div>;
}

export default function WorkflowsPage() {
  const { scope, project } = useProject();
  return scope ? <Library key={`${scope.account_id}/${scope.project_id}`} scope={scope} admin={project?.role === "admin"} /> : <ProjectStatus />;
}
