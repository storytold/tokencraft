import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { Folder, Plus } from "lucide-react";
import { Badge, Button, Input } from "@/components/ui";
import { errorMessage } from "@/lib/gateway";
import { useOrg } from "@/lib/org";
import { projectsApi } from "./api";
import { ProjectStatus, useProject } from "./ProjectScope";
import { ProjectGrants } from "./ProjectGrants";
import { ProjectCredentials } from "./ProjectCredentials";
import { ProjectWebhooks } from "./ProjectWebhooks";
import { ProjectRouting } from "./ProjectRouting";
import { ProjectClientBudgets } from "./ProjectClientBudgets";
import { PolicyInfo, ProjectDisclosure } from "./ProjectDisclosure";
import { ProjectProviderAccess } from "./providers/ProviderAccess";

type ProjectView = "settings" | "keys" | "providers";

export default function Projects({ view = "settings" }: { view?: ProjectView }) {
  const { current } = useOrg();
  const { project, scope } = useProject();
  const [routingVersion, setRoutingVersion] = useState(0);
  const canManage = current?.role === "owner" || current?.role === "admin";
  const requestedSection = view === "keys" ? "project-keys" : view === "providers" ? "project-providers" : undefined;

  useEffect(() => {
    if (!requestedSection || !canManage || !project) return;
    document.getElementById(requestedSection)?.scrollIntoView({ block: "start" });
  }, [canManage, project, requestedSection]);

  return (
    <div className="mx-auto max-w-5xl space-y-5">
      <header className="flex flex-wrap items-end justify-between gap-3 border-b border-border pb-4">
        <div className="min-w-0">
          <p className="text-sm font-medium text-muted">Projects</p>
          <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-2">
            {project && <Folder className="h-6 w-6 shrink-0 text-brand sm:h-7 sm:w-7" aria-hidden />}
            <h1 className="min-w-0 break-words font-display text-3xl font-bold tracking-tight sm:text-4xl">{project?.name ?? "Project settings"}</h1>
            {project?.is_default && <Badge tone="brand">Default project</Badge>}
          </div>
        </div>
        {canManage && <CreateProject key={current.account_id} accountId={current.account_id} />}
      </header>

      {!project || !scope ? <ProjectStatus /> : !canManage ? (
        <p className="text-sm text-muted">Organization owners and admins can manage project settings.</p>
      ) : (
        <div className="space-y-4" key={`${scope.account_id}:${project.id}`}>
          <nav aria-label="Project settings" className="flex flex-wrap gap-2 text-sm">
            <a className="rounded-lg border border-border bg-surface px-3 py-1.5 font-medium hover:text-brand" href="#project-keys">API keys</a>
            <a className="rounded-lg border border-border bg-surface px-3 py-1.5 font-medium hover:text-brand" href="#project-controls">Routing and access</a>
            <a className="rounded-lg border border-border bg-surface px-3 py-1.5 font-medium hover:text-brand" href="#project-providers">Provider exclusions</a>
          </nav>

          <section id="project-keys" aria-labelledby="project-keys-title" className="min-w-0 scroll-mt-20 space-y-3 rounded-2xl border border-border bg-surface p-4 sm:p-5">
            <h2 id="project-keys-title" className="font-display text-xl font-semibold">API keys</h2>
            <ProjectCredentials scope={scope} isDefault={project.is_default} />
          </section>

          <section id="project-controls" aria-labelledby="project-controls-title" className="min-w-0 scroll-mt-20 space-y-3 rounded-2xl border border-border bg-surface p-4 sm:p-5">
            <h2 id="project-controls-title" className="font-display text-xl font-semibold">Routing and access</h2>
            <div className="space-y-2">
              <ProjectDisclosure title="Routing policy" description="Provider allowlist, route preferences, and limits" defer>
                <ProjectRouting key={routingVersion} scope={scope} />
              </ProjectDisclosure>
              <ProjectDisclosure title="Per-client spending" description="Monthly estimate caps by client" defer
                help={<PolicyInfo title="per-client spending" topic="policy-spending-capacity">
                  A matching client budget applies in addition to the project and organization monthly budgets.
                </PolicyInfo>}>
                <ProjectClientBudgets scope={scope} />
              </ProjectDisclosure>
              {!project.is_default && <ProjectDisclosure title="Project access" description="People with access to this project" defer>
                <ProjectGrants scope={scope} />
              </ProjectDisclosure>}
              <ProjectDisclosure title="Delivery" description="Webhooks for requests and workflows" defer>
                <ProjectWebhooks scope={scope} />
              </ProjectDisclosure>
            </div>
          </section>

          <section id="project-providers" aria-labelledby="project-providers-title" className="min-w-0 scroll-mt-20 rounded-2xl border border-border bg-surface p-4 sm:p-5">
            <ProjectProviderAccess scope={scope} onSaved={() => setRoutingVersion((value) => value + 1)} />
          </section>
        </div>
      )}
    </div>
  );
}

function CreateProject({ accountId }: { accountId: string }) {
  const { select, reload } = useProject();
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [name, setName] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  async function create(event: FormEvent) {
    event.preventDefault();
    if (saving || !name.trim()) return;
    setSaving(true);
    setError("");
    try {
      const project = await projectsApi.create(accountId, name.trim());
      if (!mounted.current) return;
      dialog.current?.close();
      reload();
      select(project.id);
    } catch (error) {
      if (mounted.current) setError(errorMessage(error));
    } finally {
      if (mounted.current) setSaving(false);
    }
  }

  return (
    <>
      <Button type="button" variant="outline" aria-haspopup="dialog" aria-label="New project"
        className="shrink-0 rounded-md px-3" onClick={() => dialog.current?.showModal()}>
        <Plus className="h-4 w-4 text-brand" aria-hidden />
        <span className="hidden min-[360px]:inline">New project</span>
      </Button>
      <dialog ref={dialog} aria-labelledby={titleId}
        className="fixed inset-0 m-auto max-h-[calc(100dvh_-_2rem)] w-[calc(100%_-_2rem)] max-w-sm overflow-y-auto rounded-xl border border-border bg-surface p-5 text-fg shadow-xl backdrop:bg-black/60 sm:p-6"
        onCancel={(event) => { if (saving) event.preventDefault(); }}
        onClose={() => { setName(""); setError(""); }}>
        <h2 id={titleId} className="font-display text-xl font-semibold">Create project</h2>
        <form className="mt-4 space-y-4" onSubmit={create}>
          <label className="block space-y-1.5 text-sm font-medium">Project name
            <Input value={name} maxLength={80} required disabled={saving} onChange={(event) => setName(event.target.value)} placeholder="e.g. Studio video" />
          </label>
          {error && <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="ghost" className="rounded-md" disabled={saving} onClick={() => dialog.current?.close()}>Cancel</Button>
            <Button type="submit" className="rounded-md" disabled={saving || !name.trim()}>{saving ? "Creating…" : "Create project"}</Button>
          </div>
        </form>
      </dialog>
    </>
  );
}
