import { useState } from "react";
import { Link, useParams, Navigate, useLocation, useNavigate } from "react-router-dom";
import { ArrowLeft, BarChart3, Bot, FileText, LayoutGrid, Play } from "lucide-react";
import { Badge, Button, Card, Container } from "@/components/ui";
import { ModelCard } from "@/components/ModelCard";
import { ModelIcon } from "@/components/ModelArt";
import { hasSampleShowcase, SampleShowcase } from "@/components/SampleShowcase";
import { CodeTabs } from "@/components/CodeBlock";
import { Playground } from "./Playground";
import { Gallery, type RemixValues } from "./Gallery";
import { ModelReviews } from "./ModelReviews";
import { SharedExamples } from "./SharedExamples";
import { MODEL_BY_SLUG, MODALITY_LABEL, type Model } from "@/lib/catalog";
import { generationEndpoint, modelContract, modelPrimitive, modelRequestExample } from "@/lib/generation-contract";
import { examplesForModel } from "@/lib/gallery";
import { suiteForModel } from "@/lib/model-examples";
import { cn } from "@/lib/cn";
import { apiReferencePath } from "@/surfaces/marketing/api-reference-path";

const TABS = ["Overview", "Examples", "Playground", "API", "Changelog"] as const;
type Tab = (typeof TABS)[number];

/** Tabs sync to the URL hash (linkable, back/forward history) — matches the design's #anchors. */
const TAB_HASH: Record<Tab, string> = {
  Overview: "overview",
  Playground: "playground",
  API: "api",
  Examples: "examples",
  Changelog: "changelog",
};
function tabFromHash(hash: string): Tab {
  const h = hash.replace(/^#/, "").toLowerCase();
  return (TABS.find((t) => TAB_HASH[t] === h) as Tab) ?? "Playground";
}

export default function ModelPage() {
  const { slug } = useParams();
  const location = useLocation();
  const navigate = useNavigate();
  const model = slug ? MODEL_BY_SLUG[slug] : undefined;
  const tab = tabFromHash(location.hash);
  const setTab = (t: Tab) => navigate({ search: location.search, hash: TAB_HASH[t] });
  const [remix, setRemix] = useState<{ values: RemixValues; nonce: number } | null>(null);

  if (!model) return <Navigate to="/models" replace />;

  const applyRemix = (values: RemixValues) => {
    setRemix((r) => ({ values, nonce: (r?.nonce ?? 0) + 1 }));
    setTab("Playground");
  };

  const related = model.related.map((s) => MODEL_BY_SLUG[s]).filter(Boolean);
  const examplesAction = tab === "Playground" && Boolean(suiteForModel(model.slug));
  const ActionIcon = examplesAction ? LayoutGrid : Play;

  return (
    <Container className="px-3.5 pb-10 pt-3 sm:pt-4">
      <header className="space-y-2.5 sm:space-y-3">
        <div className="flex items-center justify-between gap-3 text-xs text-muted">
          <Link to="/models" className="inline-flex items-center gap-1.5 py-1 hover:text-brand">
            <ArrowLeft className="h-3.5 w-3.5" aria-hidden="true" /> All models
          </Link>
          <Link to={`/rankings/${rankingsMediaFor(model.modality)}`} className="inline-flex items-center gap-1.5 py-1 hover:text-brand">
            <BarChart3 className="h-3.5 w-3.5" aria-hidden="true" /> {MODALITY_LABEL[model.modality]} rankings
          </Link>
        </div>
        <div className="flex flex-wrap items-center justify-between gap-x-5 gap-y-2.5">
          <div className="flex min-w-0 items-center gap-2.5 sm:gap-3">
            <ModelIcon model={model} className="h-10 w-10 sm:h-12 sm:w-12" />
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <h1 className="break-words font-display text-[clamp(1.4rem,5vw,2.25rem)] font-extrabold leading-none tracking-tight">{model.name}</h1>
                {model.status === "new" && <Badge tone="green" className="text-[0.65rem]">New</Badge>}
                {model.status === "preview" && <Badge tone="amber" className="text-[0.65rem]">Preview</Badge>}
              </div>
              <p className="mt-1.5 text-xs text-muted">{model.provider} · {MODALITY_LABEL[model.modality]}</p>
            </div>
          </div>
          {(tab !== "Playground" || examplesAction) && (
            <Button size="sm" className="shrink-0 px-2.5 text-xs" onClick={() => setTab(examplesAction ? "Examples" : "Playground")}>
              <ActionIcon className="h-3.5 w-3.5" aria-hidden="true" />
              {examplesAction ? "Compare examples" : "Open playground"}
            </Button>
          )}
        </div>
      </header>

      <nav aria-label="Model sections" className="sticky top-0 z-30 -mx-3.5 mt-2.5 border-b border-border bg-bg/95 px-3.5 backdrop-blur sm:-mx-8 sm:mt-3 sm:px-8">
        <div className="grid grid-cols-3 gap-x-1 sm:flex">
          {TABS.map((t) => (
            <Link
              key={t}
              to={{ search: location.search, hash: TAB_HASH[t] }}
              aria-current={tab === t ? "page" : undefined}
              className={cn(
                "relative rounded-t-sm px-2.5 py-1.5 text-center text-xs font-medium transition focus-visible:outline focus-visible:outline-2 focus-visible:outline-brand sm:px-3 sm:py-2",
                tab === t ? "bg-brand/10 text-brand" : "text-muted hover:bg-surface2 hover:text-fg",
              )}
            >
              {t}
              {tab === t && <span aria-hidden="true" className="absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-brand" />}
            </Link>
          ))}
        </div>
      </nav>

      {/* ── Tab content ── */}
      <div className={tab === "Examples" ? "pb-6 pt-3.5" : "py-6 sm:py-8"}>
        {tab === "Overview" && (
          <div className="space-y-10">
            <div className="grid gap-8 lg:grid-cols-[minmax(0,1fr)_18rem]">
              <div className="prose-invert max-w-none">
                <h2 className="font-display text-xl font-bold tracking-tight">{model.tagline}</h2>
                <p className="mt-3 leading-relaxed text-muted">{model.description}</p>
                <h3 className="mt-8 font-display text-xl font-semibold">Good for</h3>
                <div className="mt-3 flex flex-wrap gap-1.5">
                  {model.tags.map((t) => (
                    <Badge key={t} tone="brand">
                      {t}
                    </Badge>
                  ))}
                </div>
              </div>
              <Card className="h-fit p-5">
                <h3 className="font-display font-semibold">At a glance</h3>
                <dl className="mt-3 space-y-2 text-sm">
                  <Row k="Provider" v={model.provider} />
                  <Row k="Modality" v={MODALITY_LABEL[model.modality]} />
                  <Row k="Status" v={model.status.toUpperCase()} />
                  <Row k="Released" v={model.released} />
                  {model.contextTokens && <Row k="Context" v={`${(model.contextTokens / 1000).toLocaleString()}K tokens`} />}
                  <Row k="Slug" v={model.slug} mono />
                </dl>
              </Card>
            </div>
          </div>
        )}

        {tab === "Playground" && <Playground key={remix?.nonce ?? 0} model={model} initialValues={remix?.values} />}

        {tab === "API" && <ApiTab model={model} />}

        {tab === "Examples" && <ModelExamples key={model.slug} model={model} onRemix={applyRemix} />}

        {tab === "Changelog" && (
          <div className="space-y-4">
            {model.changelog.map((c) => (
              <Card key={c.version} className="flex gap-4 p-5">
                <Badge tone="brand" className="h-fit">
                  v{c.version}
                </Badge>
                <div>
                  <div className="text-sm text-muted">{c.date}</div>
                  <p className="mt-1 text-fg">{c.note}</p>
                </div>
              </Card>
            ))}
          </div>
        )}
      </div>

      <ModelReviews key={model.slug} slug={model.slug} />

      {/* ── Related ── */}
      {related.length > 0 && (
        <div className="border-t border-border pt-10">
          <h2 className="mb-5 font-display text-2xl font-bold tracking-tight">Related models</h2>
          <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-3">
            {related.map((m) => (
              <ModelCard key={m.slug} model={m} />
            ))}
          </div>
        </div>
      )}
    </Container>
  );
}

function ModelExamples({ model, onRemix }: { model: Model; onRemix: (values: RemixValues) => void }) {
  const suite = suiteForModel(model.slug);
  const gallery = examplesForModel(model.slug).length > 0;
  const [showAdditional, setShowAdditional] = useState(false);
  const hasAdditional = gallery || hasSampleShowcase(model.slug);

  return (
    <div className="space-y-6">
      {suite && <SharedExamples model={model} suite={suite} />}
      {!suite && !hasAdditional && <p className="text-sm text-muted">No example outputs yet.</p>}
      {hasAdditional && (
        <details className="rounded-2xl border border-border bg-surface p-4 sm:p-5" onToggle={(event) => setShowAdditional(event.currentTarget.open)}>
          <summary className="cursor-pointer font-display font-semibold">Additional example prompts</summary>
          {showAdditional && (
            <div className="mt-5">
              {gallery ? <Gallery slug={model.slug} onRemix={onRemix} /> : <SampleShowcase slug={model.slug} modelName={model.name} />}
            </div>
          )}
        </details>
      )}
    </div>
  );
}

function Row({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="flex justify-between gap-4">
      <dt className="text-muted">{k}</dt>
      <dd className={cn("text-right text-fg", mono && "font-mono text-[13px]")}>{v}</dd>
    </div>
  );
}

/** Map a catalog modality to a rankings media key. */
function rankingsMediaFor(modality: Model["modality"]): string {
  return modality === "chat" ? "llm" : modality;
}

function ApiTab({ model }: { model: Model }) {
  const schema = modelContract(model.slug);
  const reference = `/docs/models/${model.slug}`;
  const primitive = schema?.profile.primitive ?? modelPrimitive(model.modality);
  const endpoint = generationEndpoint(primitive);
  const example = modelRequestExample(primitive, model.slug);
  return (
    <div className="space-y-6">
      <div>
        <h2 className="font-display text-xl font-bold">API schema</h2>
        <p className="mt-2 max-w-2xl text-muted">
          {schema?.profile.detail ?? (endpoint
            ? "Use the model alias enabled for your project."
            : "See this model’s generation capabilities. The gateway does not expose a generation endpoint for this modality.")}
        </p>
        <Link className="mt-3 inline-flex items-center gap-2 font-semibold text-brand hover:underline" to={reference}>
          <FileText size={16} aria-hidden /> Read the model API schema
        </Link>
        {endpoint && example && (
          <>
            <p className="mt-5 text-sm">
              <Link className="font-mono font-medium text-brand hover:underline" to={apiReferencePath("POST", endpoint)}>
                POST {endpoint}
              </Link>
            </p>
            <p className="mt-2 text-sm text-muted">Set <code>model</code> to an alias from your{" "}
              <Link className="text-brand hover:underline" to={apiReferencePath("GET", "/v1/catalog/models/scoped")}>project catalog</Link>.
              {primitive !== "text" && <> This example previews a quote with <code>dry_run</code>.</>}
            </p>
            <CodeTabs className="mt-3" tabs={[{ label: "JSON", lang: "json", code: JSON.stringify(example, null, 2) }]} />
          </>
        )}
      </div>
      <AgentsCTA />
    </div>
  );
}

function AgentsCTA() {
  return (
    <Card className="space-y-3 p-6">
      <h3 className="flex items-center gap-2 font-display text-xl font-bold">
        <Bot size={20} aria-hidden /> Use TokenCraft from an agent
      </h3>
      <div className="flex flex-wrap gap-x-5 gap-y-2">
        <Link className="font-semibold text-brand hover:underline" to="/connect/agents">
          Connect ChatGPT or Claude
        </Link>
        <Link className="font-semibold text-brand hover:underline" to="/docs#agent-setup">
          Coding agent quickstart
        </Link>
      </div>
    </Card>
  );
}
