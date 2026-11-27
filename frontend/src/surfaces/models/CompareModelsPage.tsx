import { useEffect, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { ArrowUpRight, Box, ChevronDown, Clapperboard, FileText, Image, Images } from "lucide-react";
import { ChoicePicker } from "@/components/ChoicePicker";
import { ModelIcon } from "@/components/ModelArt";
import { Badge, Button, Card, Container } from "@/components/ui";
import { MODALITY_LABEL, type Model, type Modality } from "@/lib/catalog";
import { demoEnabled } from "@/lib/gateway";
import {
  comparisonExamples, comparisonModalities, comparisonModelsFor, defaultComparisonModels,
  lowestCatalogEstimate, modelComparisonUrl, priceBasis, selectedComparisonModels,
  suggestedExample, type ComparisonExample,
} from "@/lib/model-comparison";
import { loadPublishedResults, resultForPrompt, type PublishedResults } from "@/lib/model-examples";
import { comparisonApi, type ModelQuote, type QuoteInput } from "@/surfaces/comparison/api";
import { useDeadlineClock } from "@/surfaces/comparison/QuoteExpiry";
import { lowestCurrentQuotePrice } from "./modelQuotePrice";

const modalityIcons = { image: Image, video: Clapperboard, "3d": Box, world: Box, chat: FileText };
const modalityLabels: Record<Modality, string> = { ...MODALITY_LABEL, "3d": "3D", world: "World", chat: "Text" };
const money = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 6 });
const priceReadTimeoutMs = 10_000;

export default function CompareModelsPage() {
  const [params, setParams] = useSearchParams();
  const selection = params.get("models") ?? "";
  const models = useMemo(() => {
    const selected = selectedComparisonModels(selection.split(","));
    return selected.length ? selected : defaultComparisonModels("image");
  }, [selection]);
  const modality = models[0].modality;
  const examples = useMemo(() => comparisonExamples(modality), [modality]);
  const requestedExample = params.get("example");
  const fallback = suggestedExample(models);
  const example = examples.find(({ id }) => id === requestedExample)
    ?? examples.find(({ id }) => id === fallback?.id) ?? examples[0];
  const choices = comparisonModelsFor(modality);
  const ExampleIcon = modalityIcons[modality];

  useEffect(() => {
    const title = document.title;
    document.title = "Compare models · TokenCraft";
    return () => { document.title = title; };
  }, []);

  function changeSelection(next: readonly Model[], nextExample?: ComparisonExample) {
    const search = modelComparisonUrl(next, nextExample).split("?")[1] ?? "";
    setParams(new URLSearchParams(search));
  }

  function toggleModel(model: Model) {
    const selected = models.some(({ slug }) => slug === model.slug);
    const next = selected ? models.filter(({ slug }) => slug !== model.slug) : [...models, model];
    if (!next.length || next.length > 3) return;
    changeSelection(next, example);
  }

  return (
    <Container className="space-y-6 pb-14 pt-7 sm:pt-10">
      <header className="space-y-2 border-b border-border pb-5">
        <h1 className="font-display text-4xl font-extrabold tracking-[-0.05em] sm:text-5xl">Compare models</h1>
        <p className="text-sm text-muted sm:text-base">Compare recorded outputs from the same input and see the lowest listed estimate for each model.</p>
      </header>

      <div role="group" aria-label="Comparison modality" className="flex flex-wrap gap-1.5">
        {comparisonModalities.map((item) => {
          const Icon = modalityIcons[item];
          return <button type="button" key={item} aria-pressed={item === modality}
            className={`inline-flex min-h-10 items-center gap-2 rounded-lg border px-3 text-sm font-semibold focus-visible:outline focus-visible:outline-2 focus-visible:outline-brand motion-safe:transition-colors ${item === modality ? "border-brand/30 bg-brand/10 text-brand" : "border-transparent text-muted hover:bg-surface2 hover:text-fg"}`}
            onClick={() => { if (item !== modality) changeSelection(defaultComparisonModels(item)); }}>
            <Icon className="h-4 w-4" aria-hidden /> {modalityLabels[item]}
          </button>;
        })}
      </div>

      <Card className="grid gap-4 p-4 sm:grid-cols-[minmax(0,1fr)_minmax(11rem,0.65fr)] sm:p-5">
        <ChoicePicker key={modality} label="Example" value={example.id}
          onChange={(id) => changeSelection(models, examples.find((item) => item.id === id))}
          options={examples.map(({ id, name, theme }) => ({
            value: id, label: name, keywords: [theme],
            icon: <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg border border-brand/15 bg-brand/10 text-brand" aria-hidden>
              <ExampleIcon className="h-4 w-4" />
            </span>,
            detail: <span className="rounded bg-brand/10 px-1.5 text-[11px] font-medium text-brand">{theme}</span>,
          }))} />
        <div className="min-w-0 space-y-1.5">
          <span className="block text-sm font-medium" id="compare-model-picker-label">Models</span>
          <details className="choice-picker-details relative min-w-0">
            <summary aria-labelledby="compare-model-picker-label compare-selection-count"
              className="choice-picker-trigger cursor-pointer list-none text-sm [&::-webkit-details-marker]:hidden">
              <span id="compare-selection-count" className="flex-1">{models.length} selected</span>
              <ChevronDown className="choice-picker-chevron h-4 w-4 shrink-0 text-muted" aria-hidden />
            </summary>
            <fieldset aria-label="Choose up to three models"
              className="choice-picker-list space-y-1">
              {choices.map((model) => {
                const selected = models.some(({ slug }) => slug === model.slug);
                return <label key={model.slug} className="flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 text-sm has-[:checked]:bg-brand/10 has-[:disabled]:cursor-default has-[:disabled]:opacity-60">
                  <input type="checkbox" checked={selected} disabled={selected ? models.length === 1 : models.length === 3}
                    className="h-4 w-4 shrink-0 accent-brand" onChange={() => toggleModel(model)} />
                  <ModelIcon model={model} className="h-7 w-7" />
                  <span className="min-w-0 break-words">{model.name}</span>
                </label>;
              })}
            </fieldset>
          </details>
        </div>
      </Card>

      <section aria-labelledby="compare-input-title">
        <Card className="space-y-3 p-4 sm:p-5">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 id="compare-input-title" className="text-base font-semibold">Shared input</h2>
            <span className="text-xs text-muted">{example.summary}</span>
          </div>
          <p className="rounded-lg bg-surface2/65 p-3.5 text-sm leading-6 sm:p-4">{example.prompt}</p>
          <p className="text-sm text-muted"><span className="font-semibold text-fg">What to look for</span> · {example.criteria}</p>
        </Card>
      </section>

      <ComparisonResults modelList={models} example={example} />
    </Container>
  );
}

type Records = { key: string; results: ReadonlyMap<string, PublishedResults | undefined> };

function ComparisonResults({ modelList, example }: { modelList: Model[]; example: ComparisonExample }) {
  const [records, setRecords] = useState<Records>();
  const [quotes, setQuotes] = useState<{ key: string; refresh: number; values: ReadonlyMap<string, ModelQuote | null> }>();
  const [refresh, setRefresh] = useState(0);
  const key = `${example.id}/${modelList.map(({ slug }) => slug).join(",")}`;
  const workload = useMemo(() => demoEnabled ? quoteWorkload(modelList[0].modality, example) : null, [modelList, example]);

  useEffect(() => {
    const suite = example.suite;
    if (!suite) return;
    const controller = new AbortController();
    Promise.all(modelList.map(async ({ slug }) => {
      const result = await loadPublishedResults(suite, slug, controller.signal).catch(() => undefined);
      return [slug, result] as const;
    })).then((results) => {
      if (!controller.signal.aborted) setRecords({ key, results: new Map(results) });
    });
    return () => controller.abort();
  }, [example, key, modelList]);

  useEffect(() => {
    if (!workload) return;
    const controller = new AbortController();
    const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(priceReadTimeoutMs)]);

    async function loadPrices(workload: QuoteInput["workload"]) {
      const catalog = await comparisonApi.models(signal);
      const offered = new Set(catalog.models.map(({ id }) => id));
      const entries = await Promise.all(modelList.filter(({ slug }) => offered.has(slug)).map(async ({ slug }) => {
        const quote = await comparisonApi.modelQuote({
          model: slug, provider: null, max_total_usd_nanos: null, workload,
        }, signal).catch(() => null);
        const valid = quote && Array.isArray(quote.candidates)
          && quote.candidates.every((candidate) => candidate && typeof candidate === "object");
        return [slug, valid ? quote : null] as const;
      }));
      if (!controller.signal.aborted) setQuotes({ key, refresh, values: new Map(entries) });
    }

    void loadPrices(workload).catch(() => {
      if (!controller.signal.aborted) {
        setQuotes({ key, refresh, values: new Map(modelList.map(({ slug }) => [slug, null])) });
      }
    });
    return () => controller.abort();
  }, [key, modelList, refresh, workload]);

  const published = records?.key === key ? records.results : undefined;
  const currentQuotes = quotes?.key === key && quotes.refresh === refresh ? quotes.values : undefined;
  const checkingPrices = workload !== null && !currentQuotes;

  return (
    <section className="space-y-3" aria-labelledby="compare-results-title">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="compare-results-title" className="font-display text-xl font-bold">Model outputs</h2>
        {workload && <Button type="button" size="sm" variant="outline" disabled={checkingPrices}
          onClick={() => setRefresh((value) => value + 1)}>Refresh prices</Button>}
      </div>
      <div className={`grid items-start gap-4 ${modelList.length > 1 ? "sm:grid-cols-2" : ""} ${modelList.length === 3 ? "lg:grid-cols-3" : ""}`}>
        {modelList.map((model) => {
          const asset = example.suite
            ? resultForPrompt(example.suite, example, model.slug, published?.get(model.slug))?.artifact
            : undefined;
          return <ModelResult key={model.slug} model={model} example={example} asset={asset}
            pending={!asset && Boolean(example.suite && !published)}
            checkingPrice={checkingPrices} gatewayQuote={currentQuotes?.get(model.slug)} />;
        })}
      </div>
    </section>
  );
}

function quoteWorkload(modality: Modality, example: ComparisonExample): QuoteInput["workload"] | null {
  switch (modality) {
    case "image": return { kind: "image", count: 1, aspect_ratio: "1:1", max_relative_error_bps: 0 };
    case "video": return {
      kind: "video", duration_seconds: Number(example.suite?.settings.duration_seconds ?? 6),
      aspect_ratio: "16:9", resolution: "720p", max_relative_error_bps: 0,
    };
    case "3d": return { kind: "mesh", format: "glb" };
    case "chat": return { kind: "text", max_input_tokens: 1000, max_output_tokens: 500 };
    case "world": return null;
  }
}

function ModelResult({ model, example, asset, pending, checkingPrice, gatewayQuote }: {
  model: Model; example: ComparisonExample; asset?: string; pending: boolean;
  checkingPrice: boolean; gatewayQuote?: ModelQuote | null;
}) {
  const now = useDeadlineClock([gatewayQuote?.expires_at, ...(gatewayQuote?.candidates.map(row => row.rate_valid_until) ?? [])]);
  const estimate = gatewayQuote ? lowestCurrentQuotePrice(gatewayQuote, now)
    : checkingPrice || gatewayQuote === null ? null : lowestCatalogEstimate(model, example);
  return (
    <Card className="min-w-0 overflow-hidden">
      <div className="flex items-center gap-3 px-4 py-3.5">
        <ModelIcon model={model} />
        <div className="min-w-0 flex-1">
          <h3 className="font-semibold leading-tight">
            <Link className="hover:text-brand hover:underline" to={`/models/${model.slug}`}>{model.name}</Link>
          </h3>
          <p className="mt-0.5 truncate text-xs text-muted">{model.provider}</p>
        </div>
        <ArrowUpRight className="h-4 w-4 shrink-0 text-muted" aria-hidden />
      </div>
      {asset ? <OutputAsset key={asset} model={model} asset={asset} /> : <UnavailableOutput pending={pending} />}
      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border p-4">
        <div aria-live="polite" aria-busy={checkingPrice}>
          <p className="text-xs text-muted">Lowest estimated cost</p>
          <p className="mt-0.5 font-mono text-lg font-semibold">{checkingPrice ? "Checking price…"
            : estimate === null ? "Unavailable" : money.format(estimate)}</p>
        </div>
        <div className="text-right">
          <p className="text-xs text-muted">{priceBasis(model, example)}</p>
          {estimate !== null && <Badge className="mt-1 text-[11px]">{gatewayQuote ? "Live quote" : "Catalog estimate"}</Badge>}
        </div>
      </div>
    </Card>
  );
}

function UnavailableOutput({ pending, failed = false }: { pending?: boolean; failed?: boolean }) {
  return <div className="flex min-h-52 items-center justify-center gap-2 border-y border-border bg-surface2/50 p-4 text-center text-sm text-muted" role={pending ? "status" : undefined}>
    <Images className="h-4 w-4 shrink-0" aria-hidden />
    {pending ? "Checking for a recorded output…" : failed ? "Recorded output could not load." : "No recorded output for this example"}
  </div>;
}

function OutputAsset({ model, asset }: { model: Model; asset: string }) {
  const [failed, setFailed] = useState(false);
  if (failed) return <UnavailableOutput failed />;
  if (model.modality === "image") {
    return (
      <a className="flex aspect-square w-full items-center justify-center overflow-hidden border-y border-border bg-bg" href={asset} target="_blank" rel="noreferrer">
        <img className="h-full w-full object-contain" src={asset} loading="lazy" decoding="async" referrerPolicy="no-referrer"
          alt={`Recorded output from ${model.name}`} onError={() => setFailed(true)} />
      </a>
    );
  }
  if (model.modality === "video") return <video className="aspect-video w-full bg-black" src={asset} controls playsInline preload="metadata"
    aria-label={`Recorded output from ${model.name}`} onError={() => setFailed(true)} />;
  return (
    <div className="grid min-h-52 place-items-center border-y border-border bg-surface2/50 p-4">
      <a className="inline-flex items-center gap-2 rounded-lg border border-border bg-surface px-3 py-2 text-sm font-semibold hover:border-brand" href={asset}
        download>Download recorded output <ArrowUpRight className="h-4 w-4" aria-hidden /></a>
    </div>
  );
}
