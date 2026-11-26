import { useState, type FormEvent } from "react";
import { Plus } from "lucide-react";
import { Checkbox, Button, Card, Input, Select } from "@/components/ui";
import { isProviderId } from "@/lib/gateway";
import type { Project } from "../api";
import { ProjectSharingFields } from "./ProviderAccess";
import { DiscoveryPanel } from "./DiscoveryPanel";
import { Field } from "./fields";
import { ModelEditor } from "./ModelEditor";
import {
  buildModels, defaultEnv, kindLabel, newModel, slug,
  type Binding, type Change, type Connection, type DiscoveredModel, type Kind,
  type ModelDraft, type Offer, type ProviderScope,
} from "./provider-onboarding";

const MAX_MODELS = 24;
type PluginTemplate = "fal" | "kinovi" | "openrouter";
const pluginTemplates: Record<PluginTemplate, {
  name: string;
  connection: Connection;
  model: Pick<ModelDraft, "kind" | "upstream" | "alias"> & Partial<ModelDraft>;
}> = {
  fal: { name: "fal", connection: "connector", model: { kind: "image", upstream: "fal-ai/flux/dev", alias: "flux-dev" } },
  kinovi: { name: "Kinovi", connection: "connector", model: { kind: "video", upstream: "seedance-20", alias: "seedance-2" } },
  openrouter: { name: "OpenRouter", connection: "chat", model: { kind: "text", upstream: "", alias: "" } },
};

type Props = {
  scope: ProviderScope;
  projects: Project[];
  discoveryEnabled: boolean;
  existing?: Offer;
  onPreview: (change: Change, evidence: unknown[], projects: string[] | null) => void;
  clearPreview: () => void;
  onCancel: () => void;
  busy: boolean;
};

export function ProviderEditor({ scope, projects, discoveryEnabled, existing, onPreview, clearPreview, onCancel, busy }: Props) {
  const [connection, setConnection] = useState<Connection>(existing?.adapter === "openai_compatible_chat" ? "chat" : "connector");
  const [name, setName] = useState("");
  const [providerId, setProviderId] = useState("");
  const [bindingOverride, setBindingOverride] = useState("");
  const [endpoint, setEndpoint] = useState("");
  const [source, setSource] = useState("");
  const [revision, setRevision] = useState("v1");
  const [envOverride, setEnvOverride] = useState("");
  const [useChatKey, setUseChatKey] = useState(false);
  const [defaultKind, setDefaultKind] = useState<Kind>(existing?.pricing.kind ?? "image");
  const [template, setTemplate] = useState<PluginTemplate>();
  const [sharing, setSharing] = useState<string[] | null>(null);
  const [models, setModels] = useState<ModelDraft[]>([]);
  const [error, setError] = useState("");
  const bindingName = existing?.binding ?? (bindingOverride.trim() || slug(name).slice(0, 34));
  const env = envOverride.trim() || defaultEnv(bindingName, connection);

  function changeConnection(value: Connection, kind: Kind) {
    setConnection(value);
    setDefaultKind(kind);
    setModels([]);
    setEnvOverride("");
  }

  function useTemplate(provider: PluginTemplate | undefined) {
    clearPreview();
    setError("");
    setTemplate(provider);
    setName(provider ? pluginTemplates[provider]?.name ?? "" : "");
    setProviderId("");
    setBindingOverride("");
    setEndpoint("");
    setRevision("v1");
    setUseChatKey(provider === "openrouter");
    const preset = provider && pluginTemplates[provider];
    changeConnection(preset?.connection ?? "connector", preset?.model.kind ?? "image");
    if (preset) setModels([{ ...newModel(preset.model.kind), ...preset.model }]);
  }

  function addDiscovered(suggestions: DiscoveredModel[], checkedAt: string) {
    clearPreview();
    setModels((rows) => {
      const added = new Set(rows.map((row) => row.upstream.trim()));
      const incoming = suggestions.filter((model) => !added.has(model.model_id))
        .slice(0, MAX_MODELS - rows.length).map((model) => ({
          ...newModel(model.kind), upstream: model.model_id,
          amount: model.price?.amount_usd ?? "", output: model.price?.output_usd ?? "",
          evidence: { ...model, checked_at: checkedAt },
        }));
      return [...rows, ...incoming];
    });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    setError("");
    if (busy) return;
    try {
      if (!models.length) throw new Error("Add at least one model.");
      const reviewed = buildModels(models, bindingName);
      const evidence = models.flatMap((row, index) => row.evidence
        ? [{ offer_id: reviewed[index].id, upstream_model: reviewed[index].upstream_model, suggestion: row.evidence }]
        : []);
      if (existing) {
        onPreview({ kind: "add_models", binding_name: bindingName, models: reviewed }, evidence, null);
        return;
      }
      const binding: Binding = connection === "connector" ? {
        kind: connection, name: bindingName, origin: endpoint.trim(), revision: revision.trim(), token_env: env,
      } : {
        kind: connection, name: bindingName, endpoint: endpoint.trim(), ...(useChatKey ? { key_env: env } : {}),
      };
      if (sharing?.length === 0) throw new Error("Select at least one project.");
      const identity = template ?? (providerId.trim() || undefined);
      if (identity && !isProviderId(identity)) {
        throw new Error("Enter a lowercase provider ID of up to 64 letters, digits, underscores, or hyphens.");
      }
      onPreview({ kind: "attach", name: name.trim(), binding, models: reviewed, provider_id: identity }, evidence, sharing);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "Check the provider fields.");
    }
  }

  return (
    <Card className="p-4 sm:p-6">
      <form className="space-y-5" onSubmit={submit} onChange={() => { clearPreview(); setError(""); }}>
        <header className="space-y-1">
          <h3 className="text-xl font-semibold">{existing ? `Add models to ${bindingName}` : "Connect a provider"}</h3>
          <p className="text-sm text-muted">Use an installed connector or a compatible chat service. The gateway operator deploys the reviewed catalog change.</p>
        </header>
        <fieldset disabled={busy} className="min-w-0 space-y-5">
          {!existing && (
            <>
              <Field label="Provider plugin">
                <Select value={template ?? ""} onChange={(event) => useTemplate((event.target.value || undefined) as PluginTemplate | undefined)}>
                  <option value="">Custom connection</option>
                  <option value="fal">fal plugin</option>
                  <option value="kinovi">Kinovi plugin</option>
                  <option value="openrouter">OpenRouter plugin</option>
                </Select>
              </Field>
              <div className="grid gap-3 sm:grid-cols-2">
                <Field label="Provider name"><Input required maxLength={80} value={name} onChange={(event) => setName(event.target.value)} /></Field>
                <Field label="Connection type"><Select value={connection} disabled={models.length > 0} onChange={(event) => {
                  const kind = event.target.value as Connection;
                  changeConnection(kind, kind === "chat" ? "text" : "image");
                }}>
                  <option value="connector">TokenCraft media connector</option>
                  <option value="chat">OpenAI-compatible chat</option>
                </Select></Field>
              </div>
              {!template && <Field label="Provider ID">
                <Input maxLength={64} value={providerId} placeholder="self_hosted" autoCapitalize="none" spellCheck={false}
                  onChange={(event) => setProviderId(event.target.value)} />
              </Field>}
              <Field label={connection === "connector" ? "Connector origin" : "Chat completions endpoint"}>
                <Input required maxLength={2048} value={endpoint}
                  placeholder={connection === "connector" ? "https://connector.example.com/" : "http://127.0.0.1:11434/v1/chat/completions"}
                  onChange={(event) => setEndpoint(event.target.value)} />
              </Field>
              <p className="-mt-3 text-sm text-muted">{connection === "connector"
                ? "Use the URL of a deployed TokenCraft connector."
                : "Use a gateway-local address ending in /v1/chat/completions."}</p>
              <details className="space-y-3">
                <summary className="cursor-pointer text-sm font-semibold">Binding and authentication</summary>
                <div className="grid gap-3 sm:grid-cols-2">
                  <Field label="Binding identifier">
                    <Input maxLength={64} value={bindingOverride} placeholder={bindingName || "Generated from name"}
                      onChange={(event) => setBindingOverride(event.target.value)} />
                  </Field>
                  {connection === "connector" && (
                    <Field label="Binding revision">
                      <Input maxLength={64} value={revision} onChange={(event) => setRevision(event.target.value)} />
                    </Field>
                  )}
                </div>
                {connection === "chat" && (
                  <label className="flex items-center gap-2 text-sm">
                    <Checkbox  checked={useChatKey}
                      onChange={(event) => setUseChatKey(event.target.checked)} />
                    Read a bearer key from the gateway environment
                  </label>
                )}
                {(connection === "connector" || useChatKey) && (
                  <>
                    <Field label="Environment variable name"><Input maxLength={96} value={envOverride} placeholder={env}
                      onChange={(event) => setEnvOverride(event.target.value)} /></Field>
                    <p className="text-sm text-muted">Set the actual key in the deployment environment.</p>
                  </>
                )}
              </details>
            </>
          )}
          {!existing && <ProjectSharingFields projects={projects} value={sharing} onChange={(next) => { clearPreview(); setSharing(next); }} />}
          <DiscoveryPanel key={connection} scope={scope} enabled={discoveryEnabled} connection={connection} source={source}
            setSource={setSource} added={new Set(models.map((row) => row.upstream.trim()))}
            capacity={MAX_MODELS - models.length} onAdd={addDiscovered} />
          <div className="space-y-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h4 className="font-semibold">Models and rates</h4>
              <Button type="button" size="sm" variant="outline" disabled={models.length >= MAX_MODELS} onClick={() => {
                clearPreview();
                setModels((rows) => [...rows, newModel(defaultKind)]);
              }}><Plus size={15} aria-hidden="true" /> Add {existing ? kindLabel[defaultKind].toLowerCase() : "model"} manually</Button>
            </div>
            {models.map((model, index) => (
              <ModelEditor key={model.key} model={model} connection={connection} position={index + 1}
                onChange={(patch) => setModels((rows) => rows.map((row) => row.key === model.key ? { ...row, ...patch } : row))}
                onRemove={() => {
                  clearPreview();
                  setModels((rows) => rows.filter((row) => row.key !== model.key));
                }} />
            ))}
          </div>
        </fieldset>
        {error && <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
        <div className="flex flex-wrap gap-2">
          <Button type="submit" disabled={busy || models.length === 0}>{busy ? "Validating…" : "Validate change"}</Button>
          <Button type="button" variant="outline" onClick={onCancel}>Close editor</Button>
        </div>
      </form>
    </Card>
  );
}
