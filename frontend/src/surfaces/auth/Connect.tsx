import { useRef, useState } from "react";
import { ArrowRight, Bot, ChevronDown, Copy, Eye, EyeOff, KeyRound, Terminal } from "lucide-react";
import { Link, useSearchParams } from "react-router-dom";
import { Button, Card, Input, Select } from "@/components/ui";
import { accountDisplayName, api, API_BASE_URL, ApiError } from "@/lib/api";
import { cn } from "@/lib/cn";
import { demoEnabled, errorMessage, withProject } from "@/lib/gateway";
import { connectionCheckPath } from "@/lib/onboarding";
import { OrgProvider, useOrg } from "@/lib/org";
import { projectsApi } from "@/surfaces/projects/api";
import { ProjectPicker, ProjectProvider, ProjectStatus, useProject } from "@/surfaces/projects/ProjectScope";
import { agentSetup, secureGatewayUrl, shellSetup } from "./connect-setup";

const keyLifetimeDays = 30;
const keyCreationTimeoutMs = 15_000;
const checkPath = connectionCheckPath(demoEnabled);
const linkClass = "inline-flex items-center gap-1.5 font-medium text-brand hover:underline";

export default function Connect() {
  return <OrgProvider explicitScope={demoEnabled}><ConnectProjectScope /></OrgProvider>;
}

function ConnectProjectScope() {
  const { current } = useOrg();
  return <ProjectProvider accountId={demoEnabled ? current?.account_id : undefined}><ConnectWorkspace /></ProjectProvider>;
}

function ConnectWorkspace() {
  const { accounts, current, loading, select, reload } = useOrg();
  const { project, projects } = useProject();
  const [creatingKey, setCreatingKey] = useState(false);

  return (
    <div className="w-full max-w-5xl space-y-5 sm:space-y-7">
      <header className="flex flex-wrap items-end justify-between gap-x-6 gap-y-4">
        <div>
          <h1 className="font-display text-4xl font-extrabold tracking-[-0.055em] sm:text-5xl">Get your API key</h1>
          {demoEnabled && <p className="mt-2 text-sm text-muted">Local fixture requests incur no charges.</p>}
        </div>
        {current && (accounts.length > 1 || projects.length > 1) && (
          <fieldset aria-label="API key scope" className="flex min-w-0 flex-wrap items-end gap-3" disabled={creatingKey}>
            {accounts.length > 1 && (
              <label className="block min-w-0 max-w-xs space-y-1 text-xs font-medium text-muted">
                Organization
                <Select value={current.account_id} onChange={(event) => select(event.target.value)}>
                  {accounts.map((account) => <option key={account.account_id} value={account.account_id}>{accountDisplayName(account)}</option>)}
                </Select>
              </label>
            )}
            {demoEnabled && projects.length > 1 && <ProjectPicker />}
          </fieldset>
        )}
      </header>
      {loading ? (
        <p role="status" className="text-sm text-muted">Loading your workspace…</p>
      ) : current ? (
        <ConnectFlow key={`${current.account_id}/${project?.id ?? ""}`}
          accountId={current.account_id} organization={accountDisplayName(current)}
          busy={creatingKey} setBusy={setCreatingKey} />
      ) : (
        <p role="alert" className="text-sm text-muted">
          Couldn’t load your workspace.{" "}
          <button type="button" className={linkClass} onClick={() => void reload()}>Try again</button>
        </p>
      )}
    </div>
  );
}

function ConnectFlow({ accountId, organization, busy, setBusy }: {
  accountId: string; organization: string; busy: boolean; setBusy: (busy: boolean) => void;
}) {
  const { withScope } = useOrg();
  const { project, scope } = useProject();
  const [params, setParams] = useSearchParams();
  const agent = params.get("mode") === "agent";
  const [secret, setSecret] = useState<string>();
  const [error, setError] = useState("");
  const issuing = useRef(false);
  const origin = window.location.origin;
  const baseUrl = secureGatewayUrl(API_BASE_URL, origin);
  const manageLink = demoEnabled ? withProject(withScope("/projects"), project?.id) : withScope("/keys");
  const nextLink = demoEnabled ? withProject("/playground", project?.id, accountId) : "/models";

  function selectMode(useAgent: boolean) {
    const next = new URLSearchParams(params);
    if (useAgent) next.set("mode", "agent");
    else next.delete("mode");
    setParams(next, { replace: true });
  }

  async function createKey() {
    if (busy || issuing.current || secret) return;
    issuing.current = true;
    setBusy(true);
    setError("");
    const signal = AbortSignal.timeout(keyCreationTimeoutMs);
    try {
      if (scope) {
        const key = await projectsApi.createPrincipal(scope, {
          name: "Quickstart",
          scopes: ["requests:submit", "requests:read"],
          client_ref: null,
          expires_in_days: keyLifetimeDays,
        }, signal);
        setSecret(key.token);
      } else {
        const key = await api.createApiKey(accountId, "Quickstart", undefined, signal);
        setSecret(key.secret);
      }
    } catch (error) {
      const rejected = !signal.aborted && error instanceof ApiError && error.status >= 400 && error.status < 500;
      setError(rejected ? errorMessage(error) : "We couldn’t confirm the key was created. Check Manage keys before trying again.");
    } finally {
      setBusy(false);
      issuing.current = false;
    }
  }

  return (
    <>
      <div className="grid gap-4 lg:grid-cols-2">
        <Card className="min-w-0">
          <section aria-labelledby="key-step" className="space-y-4 p-4 sm:p-6">
            <StepTitle id="key-step" icon={KeyRound}>{secret ? "Your API key" : "Create an API key"}</StepTitle>
            {demoEnabled && !scope ? <ProjectStatus /> : project && project.role !== "admin" ? (
              <p className="text-sm text-muted">Ask a project admin for a key, or select a project you manage.</p>
            ) : (
              <>
                <dl className="grid grid-cols-2 gap-3 rounded-xl border border-border bg-bg/70 px-3.5 py-3 text-sm">
                  <div className="min-w-0">
                    <dt className="text-xs text-muted">{project ? "Project" : "Organization"}</dt>
                    <dd className="mt-0.5 break-words font-medium">{project?.name ?? organization}</dd>
                  </div>
                  <div className="min-w-0">
                    <dt className="text-xs text-muted">{project ? "Expires in" : "Key type"}</dt>
                    <dd className="mt-0.5 font-medium">{project ? `${keyLifetimeDays} days` : "Organization"}</dd>
                  </div>
                </dl>
                {project && <p className="text-sm text-muted">Permission to submit and read requests in this project.</p>}
                {secret ? <KeyReveal value={secret} /> : (
                  <Button type="button" className="w-full sm:w-auto" disabled={busy} onClick={() => void createKey()}>
                    {busy ? "Creating…" : "Create API key"} <ArrowRight className="h-4 w-4" aria-hidden />
                  </Button>
                )}
                {error && <p role="alert" className="text-sm text-rose-700 dark:text-rose-300">{error}</p>}
              </>
            )}
          </section>
        </Card>

        <Card className="min-w-0 overflow-hidden">
          <section aria-labelledby="connect-step" className="space-y-4 p-4 sm:p-6">
            <StepTitle id="connect-step" icon={agent ? Bot : Terminal}>Connect your app</StepTitle>
            <div className="inline-flex max-w-full gap-1 rounded-lg border border-border bg-bg p-1" role="group" aria-label="Connection method">
              <Button type="button" size="sm" className="rounded-md px-2.5 sm:px-3" variant={agent ? "ghost" : "secondary"} aria-pressed={!agent} onClick={() => selectMode(false)}>Terminal</Button>
              <Button type="button" size="sm" className="rounded-md px-2.5 sm:px-3" variant={agent ? "secondary" : "ghost"} aria-pressed={agent} onClick={() => selectMode(true)}>Coding agent</Button>
            </div>
            {baseUrl ? (
              agent ? (
                <div className="space-y-4">
                  <p className="text-sm leading-6 text-muted">
                    {demoEnabled ? "Use a coding agent running on this machine. " : ""}
                    Start the agent from a terminal where you’ve set the key, or use its secret manager.
                  </p>
                  <p className="text-sm">Need to set the key first?{" "}
                    <button type="button" onClick={() => selectMode(false)} className={linkClass}>Open terminal setup</button>
                  </p>
                  <CopySetup key="agent" label="Copy agent prompt" previewLabel="View prompt" content={agentSetup(baseUrl, checkPath, origin + "/docs")} primary={Boolean(secret)} />
                </div>
              ) : (
                <div className="space-y-4">
                  <ol className="list-decimal space-y-1 pl-5 text-sm leading-6 text-muted">
                    <li>Copy and run the setup command in Bash or Zsh.</li>
                    <li>When prompted, return here to copy your key and paste it into the terminal.</li>
                  </ol>
                  <p className="text-sm leading-6 text-muted">The prompt hides what you paste. Once connected, start your app from this terminal so it can use the key.</p>
                  <CopySetup key="terminal" label="Copy setup command" previewLabel="View command" content={shellSetup(baseUrl, checkPath)} primary={Boolean(secret)} code />
                </div>
              )
            ) : (
              <p role="status" className="text-sm text-muted">Set up your key with an HTTPS gateway or a localhost URL.</p>
            )}
          </section>
        </Card>
      </div>

      <nav aria-label="After setup" className="flex flex-wrap items-center justify-between gap-4 border-t border-border pt-4 text-sm">
        <div className="flex flex-wrap items-center gap-x-5 gap-y-3">
          <Link to={manageLink} className={linkClass}>Manage keys</Link>
          <Link to="/docs" className={linkClass}>Read API docs</Link>
        </div>
        <Link to={nextLink} className="inline-flex h-9 items-center gap-2 rounded-md border border-border bg-surface px-3 font-semibold hover:border-brand/60">
          {demoEnabled ? "Open playground" : "Browse models"} <ArrowRight className="h-4 w-4" aria-hidden />
        </Link>
      </nav>
    </>
  );
}

function StepTitle({ id, icon: Icon, children }: { id: string; icon: typeof KeyRound; children: string }) {
  return (
    <div className="flex items-center gap-3">
      <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand/10 text-brand">
        <Icon className="h-4 w-4" aria-hidden />
      </span>
      <h2 id={id} className="font-display text-xl font-bold tracking-tight">{children}</h2>
    </div>
  );
}

function KeyReveal({ value }: { value: string }) {
  const [visible, setVisible] = useState(false);
  const [feedback, setFeedback] = useState("");

  return (
    <div className="space-y-3 border-t border-border pt-4">
      <p className="text-sm font-medium">Save your key before leaving this page.</p>
      <div className="flex gap-2">
        <Input type={visible ? "text" : "password"} aria-label="Issued API key" className="min-w-0 font-mono" readOnly autoComplete="off" spellCheck={false} value={value} />
        <Button type="button" variant="outline" className="shrink-0 px-3" aria-label={visible ? "Hide key" : "Show key"} onClick={() => setVisible((current) => !current)}>
          {visible ? <EyeOff className="h-4 w-4" aria-hidden /> : <Eye className="h-4 w-4" aria-hidden />}
        </Button>
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Button type="button" variant="outline" onClick={async () => {
          try {
            await navigator.clipboard.writeText(value);
            setFeedback("API key copied.");
          } catch {
            setFeedback("Use Show key, then select and copy it.");
          }
        }}><Copy className="h-4 w-4" aria-hidden /> Copy API key</Button>
        <span role="status" className="text-sm text-muted">{feedback}</span>
      </div>
      <a href="#connect-step" className="inline-flex items-center gap-1.5 text-sm font-medium text-brand hover:underline lg:hidden">
        Go to connection setup <ArrowRight className="h-3.5 w-3.5" aria-hidden />
      </a>
    </div>
  );
}

function CopySetup({ label, previewLabel, content, primary, code }: { label: string; previewLabel: string; content: string; primary: boolean; code?: boolean }) {
  const [feedback, setFeedback] = useState("");
  const details = useRef<HTMLDetailsElement>(null);

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Button type="button" variant={primary ? "primary" : "outline"} onClick={async () => {
          try {
            await navigator.clipboard.writeText(content);
            setFeedback("Copied.");
          } catch {
            if (details.current) details.current.open = true;
            setFeedback("Select the text below to copy it.");
          }
        }}><Copy className="h-4 w-4" aria-hidden /> {label}</Button>
        <span role="status" className="text-sm text-muted">{feedback}</span>
      </div>
      <details ref={details} className="group rounded-lg border border-border bg-bg/70">
        <summary className="flex cursor-pointer list-none items-center gap-2 px-3.5 py-2.5 text-sm font-medium [&::-webkit-details-marker]:hidden">
          <ChevronDown className="h-4 w-4 text-muted transition group-open:rotate-180 motion-reduce:transition-none" aria-hidden />
          {previewLabel}
        </summary>
        <pre tabIndex={0} className={cn(
          "max-w-full border-t border-border px-3.5 py-3 text-xs leading-6",
          code ? "overflow-x-auto" : "whitespace-pre-wrap break-words font-sans text-sm",
        )}>{code ? <code>{content}</code> : content}</pre>
      </details>
    </div>
  );
}
