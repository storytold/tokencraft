import { useEffect, useRef, useState } from "react";
import { Sparkles, X } from "lucide-react";
import { errorMessage, type ProjectScope } from "@/lib/gateway";
import { advisorApi } from "@/surfaces/models/model-advisor";
import { workflowApi } from "./api";
import type { Graph } from "./types";
import { sourceName, wireSources } from "./wiring";

export function Dialog({ title, children, onClose }: { title: string; children: React.ReactNode; onClose: () => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    dialog?.showModal();
    return () => dialog?.close();
  }, []);
  return <dialog ref={ref} className="wf-dialog" onCancel={(event) => { event.preventDefault(); onClose(); }} aria-label={title}>
    <header><h2>{title}</h2><button className="wf-icon-button" type="button" aria-label="Close" onClick={onClose}><X size={18} /></button></header>
    <div className="wf-dialog-body">{children}</div>
  </dialog>;
}

export function IntentDialog({ scope, onClose, onCreate }: {
  scope: ProjectScope; onClose: () => void; onCreate: (graph: Graph) => Promise<void>;
}) {
  const [intent, setIntent] = useState("");
  const [advisor, setAdvisor] = useState(false);
  const [online, setOnline] = useState(false);
  const [proposal, setProposal] = useState<Graph>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const request = useRef<AbortController | undefined>(undefined);

  useEffect(() => {
    const controller = new AbortController();
    advisorApi.status(controller.signal).then((status) => setOnline(status.web_research)).catch(() => {});
    return () => { controller.abort(); request.current?.abort(); };
  }, []);

  async function generate() {
    const controller = new AbortController();
    request.current?.abort();
    request.current = controller;
    setBusy(true);
    setError("");
    setProposal(undefined);
    try {
      const graph = await workflowApi.plan(scope, intent.trim(), advisor, controller.signal);
      if (!controller.signal.aborted) setProposal(graph);
    } catch (failure) {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  async function create() {
    if (!proposal) return;
    setBusy(true);
    setError("");
    try { await onCreate(proposal); }
    catch (failure) { setError(errorMessage(failure)); setBusy(false); }
  }

  return <Dialog title="Describe your workflow" onClose={onClose}>
    <form className="wf-stack" onSubmit={(event) => { event.preventDefault(); void generate(); }}>
      <label className="wf-field">
        What should your application send and receive?
        <textarea autoFocus rows={4} maxLength={1800} value={intent} required disabled={busy}
          placeholder="Turn a product brief into a short caption, a hero image, and an animated launch clip."
          onChange={(event) => { setIntent(event.target.value); setProposal(undefined); }} />
      </label>
      {online && <details className="wf-disclosure">
        <summary>Planning options</summary>
        <label className="wf-checkbox">
          <input type="checkbox" checked={advisor} disabled={busy}
            onChange={(event) => { setAdvisor(event.target.checked); setProposal(undefined); }} />
          <span>Use Model Advisor to plan<span className="wf-muted wf-block">Sends this description to the configured Advisor provider.</span></span>
        </label>
      </details>}
      {error && <p role="alert" className="wf-error">{error}</p>}
      {proposal && <section className="wf-proposal" aria-label="Proposed steps">
        <strong>Proposed wiring</strong>
        <ol>{proposal.nodes.map((step) => {
          const sources = [...new Set(Object.values(step.settings).flatMap(wireSources)
            .map((source) => sourceName(proposal, source)).filter(Boolean))];
          return <li key={step.id}>
            <span>{step.title}</span>
            {sources.length > 0 && <small>Uses {sources.join(", ")}</small>}
          </li>;
        })}</ol>
        <small>{Object.keys(proposal.outputs).length} response outputs</small>
      </section>}
      <footer className="wf-actions">
        <button className={proposal ? "wf-button" : "wf-button wf-primary"} disabled={busy || !intent.trim()} type="submit">
          <Sparkles size={15} /> {busy && !proposal ? "Planning…" : "Plan workflow"}
        </button>
        {proposal && <button className="wf-button wf-primary" type="button" disabled={busy} onClick={() => void create()}>
          {busy ? "Creating…" : "Use this draft"}
        </button>}
      </footer>
    </form>
  </Dialog>;
}
