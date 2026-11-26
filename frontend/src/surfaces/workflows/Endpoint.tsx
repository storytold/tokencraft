import { useEffect, useState } from "react";
import { Copy, KeyRound, Plus, ShieldCheck } from "lucide-react";
import { API_BASE_URL } from "@/lib/api";
import { errorMessage, type ProjectScope } from "@/lib/gateway";
import { workflowApi, workflowEndpoint } from "./api";
import { Dialog } from "./IntentDialog";
import type { AppKey, IssuedKey, Permission, Release, Workflow } from "./types";

const permissions: Record<Permission, string> = {
  "runs:submit": "Invoke", "runs:read": "Read status", "outputs:read": "Read outputs", "runs:cancel": "Cancel runs",
};

function CopyAction({ value, label = "Copy" }: { value: string; label?: string }) {
  const [feedback, setFeedback] = useState("");
  return <button className="wf-button wf-small" type="button" onClick={async () => {
    try { await navigator.clipboard.writeText(value); setFeedback("Copied"); }
    catch { setFeedback("Select and copy the text"); }
    window.setTimeout(() => setFeedback(""), 2500);
  }}><Copy size={14} /> {feedback || label}</button>;
}

function NewApp({ onClose, onCreate }: {
  onClose: () => void; onCreate: (name: string, scopes: Permission[], days: number) => Promise<void>;
}) {
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  return <Dialog title="Add an application" onClose={onClose}>
    <form className="wf-stack" onSubmit={async (event) => {
      event.preventDefault();
      const form = new FormData(event.currentTarget);
      setError(""); setBusy(true);
      try { await onCreate(String(form.get("name")), form.getAll("scopes") as Permission[], Number(form.get("days"))); }
      catch (failure) { setError(errorMessage(failure)); }
      finally { setBusy(false); }
    }}>
      <label className="wf-field">Application name<input autoFocus name="name" placeholder="Commerce backend" maxLength={80} required /></label>
      <label className="wf-field">Key expiration (days)<input name="days" defaultValue={30} type="number" min={1} max={90} required /></label>
      <fieldset className="wf-binding"><legend>Endpoint permissions</legend>
        {Object.entries(permissions).map(([scope, label]) =>
          <label className="wf-checkbox" key={scope}><input type="checkbox" name="scopes" value={scope} defaultChecked={scope !== "runs:cancel"} /> {label}</label>)}
      </fieldset>
      {error && <p role="alert" className="wf-error">{error}</p>}
      <footer className="wf-actions"><button className="wf-button wf-primary" type="submit" disabled={busy}>Create key</button></footer>
    </form>
  </Dialog>;
}

function accessState(key: AppKey) {
  if (key.revoked_at) return "Revoked";
  return new Date(key.expires_at).getTime() <= Date.now() ? "Expired" : "Active";
}

export function Endpoint({ scope, workflow, admin, onUpdate }: {
  scope: ProjectScope; workflow: Workflow; admin: boolean; onUpdate: (workflow: Workflow) => void;
}) {
  const [keys, setKeys] = useState<AppKey[]>([]);
  const [releases, setReleases] = useState<Release[]>([]);
  const [refresh, setRefresh] = useState(0);
  const [newApp, setNewApp] = useState(false);
  const [issued, setIssued] = useState<IssuedKey>();
  const [revoke, setRevoke] = useState<AppKey>();
  const [version, setVersion] = useState<number>();
  const [contract, setContract] = useState<Awaited<ReturnType<typeof workflowApi.openApi>>>();
  const [showContract, setShowContract] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const active = workflow.active_version;
  const endpoint = API_BASE_URL + workflowEndpoint(scope, workflow.id);
  const example = `curl --request POST '${endpoint}/runs' \\\n  --header "Authorization: Bearer \${WORKFLOW_API_KEY}" \\\n  --header 'Idempotency-Key: product-run-0001' \\\n  --header 'Content-Type: application/json' \\\n  --data '${JSON.stringify({ inputs: contract?.["x-workflow-example-inputs"] ?? {} }).replaceAll("'", "'\\''")}'`;

  useEffect(() => {
    const controller = new AbortController();
    if (active !== null) workflowApi.openApi(scope, workflow.id).then((document) => {
      if (!controller.signal.aborted) setContract(document);
    }).catch((failure) => {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    });
    workflowApi.releases(scope, workflow.id, controller.signal).then(setReleases).catch((failure) => {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    });
    if (admin) workflowApi.keys(scope, workflow.id, controller.signal).then(setKeys).catch((failure) => {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    });
    return () => controller.abort();
  }, [scope, workflow.id, active, refresh, admin]);

  async function doAction(action: () => Promise<unknown>) {
    setBusy(true); setError("");
    try { await action(); setRefresh((value) => value + 1); }
    catch (failure) { setError(errorMessage(failure)); }
    finally { setBusy(false); }
  }

  return <div className="wf-grid">
    <div className="wf-stack">
      <section className="wf-panel wf-stack">
        <div className="wf-panel-heading"><h3>Production endpoint</h3>
          <span className={`wf-status ${active !== null && !workflow.paused ? "wf-status-succeeded" : ""}`}>
            {active === null ? "Unpublished" : workflow.paused ? "Paused" : `Live · version ${active}`}
          </span></div>
        <div className="wf-url"><strong>POST</strong> <code>{endpoint}/runs</code></div>
        <div className="wf-actions">
          <CopyAction value={`${endpoint}/runs`} label="Copy URL" />
          {admin && active !== null && <button className="wf-button wf-small" disabled={busy} type="button" onClick={() => void doAction(async () =>
            onUpdate(await workflowApi.activation(scope, workflow, active, !workflow.paused)))}>
            {workflow.paused ? "Resume endpoint" : "Pause endpoint"}
          </button>}
        </div>
        {active === null && <p className="wf-muted">Test and publish a draft to enable external invocations.</p>}
      </section>
      {active !== null && <section className="wf-panel wf-stack">
        <div className="wf-panel-heading"><h3>Request example</h3>{contract && <CopyAction value={example} label="Copy cURL" />}</div>
        {contract?.["x-workflow-upload-inputs"] && <p className="wf-muted">
          Upload source files to <code>{endpoint}/uploads</code> with the application key, file media type, and SHA-256 header.
          Use each returned upload ID in the request.
        </p>}
        {contract ? <pre>{example}</pre> : <p className="wf-muted">Loading the published contract…</p>}
        <p className="wf-muted">Store the key on your application server. Save the run ID and poll the URL in the receipt.</p>
        {contract && <div><button type="button" className="wf-button wf-small" aria-expanded={showContract}
          onClick={() => setShowContract(!showContract)}>{showContract ? "Hide OpenAPI" : "View OpenAPI"}</button></div>}
        {showContract && contract && <><pre>{JSON.stringify(contract, null, 2)}</pre>
          <CopyAction value={JSON.stringify(contract, null, 2)} label="Copy OpenAPI" /></>}
      </section>}
      {releases.length > 0 && <section className="wf-panel wf-stack">
        <div className="wf-panel-heading"><h3>Release history</h3></div>
        {releases.map((release) => <div className="wf-panel-heading" key={release.version}>
          <div><strong>Version {release.version}</strong><p className="wf-muted">Draft {release.draft_revision} · {new Date(release.published_at).toLocaleDateString()}</p></div>
          {active === release.version ? <span className="wf-status wf-status-succeeded">Selected</span>
            : admin && <button type="button" className="wf-button wf-small" onClick={() => setVersion(release.version)}>Activate</button>}
        </div>)}
      </section>}
    </div>
    <section className="wf-panel wf-stack">
      <div className="wf-panel-heading"><h3><ShieldCheck size={17} /> Application access</h3>
        {admin && <button className="wf-button wf-small" type="button" onClick={() => setNewApp(true)}><Plus size={14} /> Add app</button>}
      </div>
      {error && <p className="wf-error" role="alert">{error}</p>}
      {!admin && <p className="wf-muted">An organization admin manages endpoint keys.</p>}
      {admin && keys.length === 0 && <p className="wf-muted">Create a scoped key for each application that calls this workflow.</p>}
      {keys.map((key) => <article className="wf-key-row" key={key.key_id}>
        <div className="wf-panel-heading"><strong>{key.name}</strong><span className="wf-status">{accessState(key)}</span></div>
        <p className="wf-muted">{key.scopes.map((scope) => permissions[scope]).join(" · ")}</p>
        <p className="wf-muted">App ID <code>{key.app_id}</code></p>
        <p className="wf-muted"><code>{key.prefix}…</code> · Expires {new Date(key.expires_at).toLocaleDateString()}</p>
        {accessState(key) === "Active" && <div className="wf-actions">
          <button type="button" className="wf-button wf-small" disabled={busy} onClick={() => void doAction(async () => setIssued(
            await workflowApi.rotate(scope, workflow.id, key.app_id, 30),
          ))}><KeyRound size={14} /> Rotate key</button>
          <button type="button" className="wf-button wf-small wf-danger" disabled={busy} onClick={() => setRevoke(key)}>Revoke</button>
        </div>}
      </article>)}
    </section>
    {newApp && <NewApp onClose={() => setNewApp(false)} onCreate={async (name, scopes, days) => {
      const key = await workflowApi.createApp(scope, workflow.id, name, scopes, days);
      setNewApp(false); setIssued(key); setRefresh((value) => value + 1);
    }} />}
    {issued && <Dialog title="Application key" onClose={() => setIssued(undefined)}>
      <div className="wf-stack"><p>Save this key in the application’s server-side secret store. This is the only time you’ll see it.</p>
        <pre>{issued.secret}</pre><CopyAction value={issued.secret} label="Copy key" />
      </div>
    </Dialog>}
    {revoke && <Dialog title="Revoke key" onClose={() => setRevoke(undefined)}>
      <div className="wf-stack"><p>Requests using this key for {revoke.name} will be rejected.</p>
        <button className="wf-button wf-danger" type="button" disabled={busy} onClick={() => void doAction(async () => {
          await workflowApi.revoke(scope, workflow.id, revoke.key_id); setRevoke(undefined);
        })}>Revoke key</button>
      </div>
    </Dialog>}
    {version !== undefined && <Dialog title={`Activate version ${version}`} onClose={() => setVersion(undefined)}>
      <div className="wf-stack"><p>New requests will use this version of the workflow. Existing runs keep their pinned version.</p>
        <button className="wf-button wf-primary" type="button" disabled={busy} onClick={() => void doAction(async () => {
          onUpdate(await workflowApi.activation(scope, workflow, version, workflow.paused)); setVersion(undefined); setContract(undefined);
        })}>Activate this version</button>
      </div>
    </Dialog>}
  </div>;
}
