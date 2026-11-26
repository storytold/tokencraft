import { useEffect, useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { Checkbox, Badge, Button, Select } from "@/components/ui";
import { errorMessage, gatewayRequest, isGatewayEndpointUnavailable, withProject, type ProjectScope } from "@/lib/gateway";
import { useOrg } from "@/lib/org";
import { providerLabel } from "@/surfaces/comparison/api";
import { ConnectorProviderKeys, ProjectProviderKeys } from "../ProjectProviderKeys";
import { ProjectDisclosure } from "../ProjectDisclosure";
import type { Project } from "../api";
import { kindLabel, providerOnboarding, type Inventory, type Offer, type ProviderScope } from "./provider-onboarding";

export function providerTitle(offer: Pick<Offer, "provider" | "adapter">) {
  return offer.adapter === "mock" ? "Synthetic fixture" : providerLabel(offer.provider);
}

export function groupConnections(offers: Offer[]) {
  const groups = new Map<string, Offer[]>();
  for (const offer of offers) {
    const id = offer.connection_id ?? (offer.binding ? `${offer.provider}/${offer.adapter}/${offer.binding}` : offer.provider);
    const group = groups.get(id) ?? [];
    group.push(offer);
    groups.set(id, group);
  }
  return groups;
}

export function ProjectSharingFields({ projects, value, onChange }: {
  projects: Project[];
  value: string[] | null;
  onChange: (value: string[] | null) => void;
}) {
  return (
    <fieldset className="min-w-0 space-y-2">
      <legend className="mb-1 text-sm font-semibold">Available to</legend>
      <Select aria-label="Available to" value={value === null ? "all" : "selected"}
        onChange={(event) => onChange(event.target.value === "all" ? null : [])}>
        <option value="all">All projects</option>
        <option value="selected">Selected projects</option>
      </Select>
      {value === null ? <p className="text-xs text-muted">Includes future projects that inherit shared providers.</p> :
        <ul className="max-h-48 space-y-2 overflow-y-auto py-1">
          {projects.map((project) => (
            <li key={project.id}>
              <label className="flex items-center gap-2 text-sm">
                <Checkbox  checked={value.includes(project.id)}
                  onChange={(event) => onChange(event.target.checked
                    ? [...value, project.id] : value.filter((id) => id !== project.id))} />
                {project.name}
              </label>
            </li>
          ))}
          {!projects.length && <li className="text-sm text-muted">No projects are available.</li>}
        </ul>}
    </fieldset>
  );
}

export function ConnectionSharing({ accountId, revision, offer, projects, onSaved, onCancel }: {
  accountId: string;
  revision: number;
  offer: Offer;
  projects: Project[];
  onSaved: () => void;
  onCancel: () => void;
}) {
  const [selected, setSelected] = useState<string[] | null>(offer.selected_projects ?? null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  async function save(event: FormEvent) {
    event.preventDefault();
    setSaving(true);
    setError("");
    try {
      await providerOnboarding.access({ account_id: accountId }, revision, offer.connection_id, {
        kind: "share", projects: selected,
      });
      onSaved();
    } catch (failure) {
      setError(errorMessage(failure));
      setSaving(false);
    }
  }

  return (
    <form className="space-y-3 rounded-lg border border-border p-3" onSubmit={(event) => void save(event)}>
      <h3 className="text-sm font-semibold">Sharing for {offer.binding ?? providerTitle(offer)}</h3>
      <fieldset className="space-y-3" disabled={saving}>
        <ProjectSharingFields projects={projects} value={selected} onChange={setSelected} />
        <div className="flex flex-wrap gap-2">
          <Button size="sm" type="submit" disabled={selected?.length === 0}>{saving ? "Saving…" : "Save sharing"}</Button>
          <Button size="sm" type="button" variant="outline" onClick={onCancel}>Cancel</Button>
        </div>
      </fieldset>
      {error && <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
    </form>
  );
}

export function ProjectProviderAccess({ scope, onSaved }: { scope: ProjectScope; onSaved: () => void }) {
  const { withScope } = useOrg();
  const { account_id, project_id } = scope;
  const [inventory, setInventory] = useState<Inventory>();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState<string>();
  const [error, setError] = useState("");
  const [refresh, setRefresh] = useState(0);

  function reload() {
    setError("");
    setLoading(true);
    setRefresh((value) => value + 1);
  }

  useEffect(() => {
    const controller = new AbortController();
    providerOnboarding.inventory({ account_id, project_id }, controller.signal)
      .then((result) => { if (!controller.signal.aborted) setInventory(result); })
      .catch((failure) => { if (!controller.signal.aborted) setError(errorMessage(failure)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [account_id, project_id, refresh]);

  async function toggle(offer: Offer) {
    if (!inventory || saving) return;
    const excluded = !offer.excluded_in_project;
    const next = {
      ...inventory,
      offers: inventory.offers.map((item) => item.connection_id === offer.connection_id
        ? { ...item, excluded_in_project: excluded } : item),
    };
    setSaving(offer.connection_id);
    setError("");
    setInventory(next);
    try {
      const result = await providerOnboarding.access(scope, inventory.policy_revision, offer.connection_id, {
        kind: "exclude", excluded,
      });
      setInventory({ ...next, policy_revision: result.revision });
      reload();
      onSaved();
    } catch (failure) {
      setInventory(inventory);
      setError(errorMessage(failure));
    } finally {
      setSaving(undefined);
    }
  }

  return (
    <section className="min-w-0 space-y-3" aria-labelledby="project-providers-title">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h2 id="project-providers-title" className="font-display text-xl font-semibold">Provider exclusions</h2>
          <p className="mt-1 text-sm text-muted">This project inherits organization providers. Exclude individual connections here.</p>
        </div>
        <Link to={withScope("/providers")} className="text-sm font-semibold text-brand hover:underline">Organization providers</Link>
      </div>
      {loading && <p className="text-sm text-muted" role="status">Reading provider access…</p>}
      {error && <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}{" "}
        <button type="button" className="underline" onClick={reload}>Refresh provider access</button></p>}
      {inventory && <ul className="divide-y divide-border rounded-xl border border-border">
        {[...groupConnections(inventory.offers)].map(([id, offers]) => {
          const offer = offers[0];
          const status = offer.excluded_in_project ? "Excluded from project"
            : offer.available_in_project === false ? "Outside sharing scope"
            : !offer.allowed_by_policy ? "Blocked by allowlist" : "Inherited";
          return (
            <li key={id} className="space-y-2 p-3.5 sm:p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="min-w-0">
                  <p className="break-words text-sm font-semibold">{providerTitle(offer)}{offer.binding && ` · ${offer.binding}`}</p>
                  <Badge tone={status === "Inherited" ? "green" : "amber"}>{status}</Badge>
                </div>
                <label className="flex cursor-pointer items-center gap-2.5 rounded-lg border border-border px-2.5 py-1.5 text-sm has-[:checked]:bg-brand/5">
                  <Checkbox  checked={offer.excluded_in_project ?? false}
                    disabled={Boolean(saving) || loading || inventory.policy_revision === undefined}
                    aria-label={`Exclude ${offer.binding ?? providerTitle(offer)} from this project`}
                    onChange={() => void toggle(offer)} />
                  Exclude
                </label>
              </div>
              <p className="break-words text-xs text-muted">{offers.map((model) => `${model.model} (${kindLabel[model.pricing.kind]})`).join(", ")}</p>
            </li>
          );
        })}
        {!inventory.offers.length && <li className="p-3 text-sm text-muted">No provider connections are available.</li>}
      </ul>}
      {!!inventory?.credential_bindings?.length && (
        <ProjectDisclosure title="Project provider credentials" unmountOnClose>
          <ConnectorProviderKeys scope={scope} bindings={inventory.credential_bindings} />
        </ProjectDisclosure>
      )}
      {inventory?.provider_key_management && !inventory.credential_bindings?.length && (
        <ProjectDisclosure title="Project provider credentials" unmountOnClose>
          <ProjectProviderKeys scope={scope} />
        </ProjectDisclosure>
      )}
      <ProjectDisclosure title="Routing change history" unmountOnClose>
        <RoutingHistory scope={scope} refresh={inventory?.policy_revision} />
      </ProjectDisclosure>
    </section>
  );
}

type HistoryEntry = { revision: number; created_at: string; created_by: string; policy: Record<string, unknown> };

export function RoutingHistory({ scope, refresh }: { scope: ProviderScope; refresh?: number }) {
  const { account_id, project_id } = scope;
  const [entries, setEntries] = useState<HistoryEntry[]>();
  const [error, setError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    gatewayRequest<HistoryEntry[]>(withProject("/v1/routing-policy/history", project_id, account_id), { signal: controller.signal })
      .then((result) => { if (!controller.signal.aborted) { setEntries(result); setError(""); } })
      .catch((failure) => {
        if (!controller.signal.aborted) {
          setError(isGatewayEndpointUnavailable(failure) ? "Routing history requires an updated gateway." : errorMessage(failure));
        }
      });
    return () => controller.abort();
  }, [account_id, project_id, refresh]);

  if (error) return <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>;
  if (!entries) return <p className="text-sm text-muted" role="status">Reading change history…</p>;
  if (!entries.length) return <p className="text-sm text-muted">No routing changes yet.</p>;
  return (
    <ol className="space-y-2 text-sm">
      {entries.map((entry, index) => {
        const before = entries[index + 1]?.policy;
        const fields = Object.keys({ ...before, ...entry.policy }).filter((key) =>
          JSON.stringify(before?.[key]) !== JSON.stringify(entry.policy[key]));
        const sharingChanged = fields.includes("connection_projects");
        const exclusionsChanged = fields.includes("excluded_connections");
        const time = new Date(entry.created_at).toLocaleString();
        return (
          <li key={entry.revision} className="min-w-0 rounded-md border border-border p-2">
            <details>
              <summary className="cursor-pointer break-words">
                Revision {entry.revision} · {!before && entry.revision > 1 ? "Routing settings" : sharingChanged ? "Provider sharing" : exclusionsChanged ? "Provider exclusions" : "Routing settings"} · {time}
              </summary>
              <p className="mt-2 break-all text-xs text-muted">Changed by {entry.created_by}</p>
              <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(entry.policy, null, 2)}</pre>
            </details>
          </li>
        );
      })}
    </ol>
  );
}
