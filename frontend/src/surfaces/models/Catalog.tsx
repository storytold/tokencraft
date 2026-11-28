import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate, useSearchParams } from "react-router-dom";
import { Compass, LayoutGrid, Rows3, Search, TrendingDown, TrendingUp, X } from "lucide-react";
import { Badge, Button, Container, Input, Select } from "@/components/ui";
import { Sparkline } from "@/components/Sparkline";
import { ModelCard } from "@/components/ModelCard";
import { ModelIcon } from "@/components/ModelArt";
import { MODELS, MODALITY_LABEL, PROVIDERS, type Modality } from "@/lib/catalog";
import { SORTS, rankModels, sortMetric, type RankedModel, type SortKey } from "@/lib/model-stats";
import { cn } from "@/lib/cn";
import { ModelAdvisor } from "./ModelAdvisor";
import "./catalog.css";

const MODALITIES = ["all", "image", "video", "3d", "world", "chat"] as const;
const focusRing = "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand";

function normalizeSearch(value: string) {
  return value.toLowerCase().replace(/[_\p{Pd}]+/gu, " ").trim();
}

export default function Catalog() {
  const location = useLocation();
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const activeModality = MODALITIES.find((modality) => modality === params.get("modality")) ?? "all";
  const [query, setQuery] = useState("");
  const [provider, setProvider] = useState<string>("all");
  const [sort, setSort] = useState<SortKey>("generations");
  const [view, setView] = useState<"grid" | "table">("grid");
  const searchInput = useRef<HTMLInputElement>(null);
  const advisorButton = useRef<HTMLButtonElement>(null);
  const advisorOpen = location.hash === "#advisor";

  const ranked = useMemo(() => {
    const terms = normalizeSearch(query).split(/\s+/).filter(Boolean);
    const filtered = MODELS.filter((model) => {
      if (activeModality !== "all" && model.modality !== activeModality) return false;
      if (provider !== "all" && model.provider !== provider) return false;
      if (!terms.length) return true;
      const searchText = normalizeSearch([
        model.name, model.slug, model.provider, MODALITY_LABEL[model.modality], model.tagline, ...model.tags,
      ].join(" "));
      return terms.every((term) => searchText.includes(term));
    });
    return rankModels(filtered, sort);
  }, [activeModality, provider, query, sort]);

  useEffect(() => {
    if (advisorOpen) return;
    function focusSearch(event: KeyboardEvent) {
      if (event.defaultPrevented || event.isComposing || event.altKey || event.shiftKey) return;
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "k") return;
      event.preventDefault();
      searchInput.current?.focus();
      searchInput.current?.select();
    }
    window.addEventListener("keydown", focusSearch);
    return () => window.removeEventListener("keydown", focusSearch);
  }, [advisorOpen]);

  function setModality(modality: Modality | "all") {
    const next = new URLSearchParams(params);
    if (modality === "all") next.delete("modality");
    else next.set("modality", modality);
    setParams(next, { replace: true });
  }

  function setAdvisorOpen(open: boolean) {
    navigate({ pathname: location.pathname, search: location.search, hash: open ? "#advisor" : "" }, { replace: true });
    if (!open) advisorButton.current?.focus({ preventScroll: true });
  }

  function clearSearch() {
    setQuery("");
    searchInput.current?.focus();
  }

  function resetSearchAndFilters() {
    setModality("all");
    setProvider("all");
    clearSearch();
  }

  const shortcut = /Macintosh/.test(navigator.userAgent) ? "⌘ K" : "Ctrl K";

  return (
    <>
      <section className="catalog-banner border-b border-border" aria-labelledby="catalog-title">
        <Container className="pb-6 pt-7 sm:py-11">
          <div className="motion-safe:animate-fade-up">
            <h1 id="catalog-title" className="font-display text-4xl font-extrabold tracking-tight sm:text-5xl">Model catalog</h1>
            <p className="mt-2 text-sm leading-6 text-muted sm:text-base">Explore {MODELS.length} models and compare their capabilities and creators.</p>
            <div className="mt-6 flex flex-col items-end gap-2.5 sm:flex-row sm:items-center sm:gap-3">
              <div role="search" className="group relative w-full min-w-0 flex-1">
                <Search className="pointer-events-none absolute left-4 top-1/2 h-5 w-5 -translate-y-1/2 text-muted group-focus-within:text-brand motion-safe:transition-colors" aria-hidden="true" />
                <Input
                  ref={searchInput}
                  type="search"
                  aria-label="Search models"
                  aria-keyshortcuts="Control+K Meta+K"
                  placeholder="Search name, creator, or capability"
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  className={cn("catalog-search-input h-14 rounded-xl bg-surface pl-12 text-base shadow-sm focus:border-brand/70 motion-reduce:transition-none sm:pr-20", query && "pr-12")}
                />
                {query ? (
                  <Button type="button" variant="ghost" aria-label="Clear search" onClick={clearSearch} className="absolute right-1.5 top-1/2 h-11 w-11 -translate-y-1/2 rounded-lg p-0 motion-reduce:transition-none">
                    <X className="h-4 w-4" aria-hidden="true" />
                  </Button>
                ) : (
                  <kbd aria-hidden="true" className="pointer-events-none absolute right-3 top-1/2 hidden -translate-y-1/2 rounded-md border border-border bg-bg px-2 py-0.5 font-mono text-xs text-muted sm:block">
                    {shortcut}
                  </kbd>
                )}
              </div>
              <Button
                ref={advisorButton}
                type="button"
                variant="outline"
                aria-haspopup="dialog"
                onClick={() => setAdvisorOpen(true)}
                className="group h-11 shrink-0 rounded-xl px-4 hover:bg-surface2/70 motion-reduce:transition-none"
              >
                <Compass className="h-4 w-4 text-brand motion-safe:transition-transform motion-safe:group-hover:rotate-12" aria-hidden="true" />
                Model Advisor
              </Button>
            </div>
          </div>
        </Container>
      </section>

      <Container className="pb-12 pt-5 sm:pt-6">
        <div className="flex flex-col gap-3 border-b border-border pb-4 lg:flex-row lg:items-center lg:justify-between">
          <div role="group" aria-label="Model categories" className="flex flex-wrap gap-1.5">
            {MODALITIES.map((modality) => (
              <button
                key={modality}
                type="button"
                aria-pressed={activeModality === modality}
                onClick={() => setModality(modality)}
                className={cn(
                  "min-h-9 rounded-lg border px-3 text-sm font-medium motion-safe:transition-colors", focusRing,
                  activeModality === modality ? "border-brand/25 bg-brand/10 text-brand" : "border-transparent text-muted hover:bg-surface2 hover:text-fg",
                )}
              >
                {modality === "all" ? "All models" : MODALITY_LABEL[modality]}
              </button>
            ))}
          </div>
          <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)] gap-2 sm:flex">
            <Select aria-label="Model creator" value={provider} onChange={(event) => setProvider(event.target.value)} className="h-9 truncate rounded-lg bg-surface motion-reduce:transition-none">
              <option value="all">All creators</option>
              {PROVIDERS.map((creator) => <option key={creator} value={creator}>{creator}</option>)}
            </Select>
            <Select aria-label="Rank by" value={sort} onChange={(event) => setSort(event.target.value as SortKey)} className="h-9 truncate rounded-lg bg-surface motion-reduce:transition-none">
              {SORTS.map(({ key, label }) => <option key={key} value={key}>{label}</option>)}
            </Select>
          </div>
        </div>

        <div className="flex min-h-16 items-center justify-between gap-3">
          <p role="status" className="text-sm text-muted">
            <span className="font-semibold tabular-nums text-fg">{ranked.length}</span> {ranked.length === 1 ? "model" : "models"}
          </p>
          <div role="group" aria-label="Catalog view" className="inline-flex gap-0.5 rounded-lg border border-border bg-surface p-0.5">
            <ViewButton active={view === "grid"} onClick={() => setView("grid")} icon={LayoutGrid} label="Grid" />
            <ViewButton active={view === "table"} onClick={() => setView("table")} icon={Rows3} label="Table" />
          </div>
        </div>

        {!ranked.length ? (
          <div className="grid justify-items-center rounded-xl border border-dashed border-border bg-surface/60 px-5 py-12 text-center motion-safe:animate-fade-up">
            <Search className="h-5 w-5 text-brand" aria-hidden="true" />
            <h2 className="mt-3 font-display text-lg font-bold">No models found</h2>
            <p className="mt-1 text-sm text-muted">Try a different search or reset your filters.</p>
            <Button type="button" variant="outline" onClick={resetSearchAndFilters} className="mt-4 rounded-lg motion-reduce:transition-none">
              Reset search and filters
            </Button>
          </div>
        ) : view === "grid" ? (
          <div className="grid gap-4 motion-safe:animate-fade-up sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
            {ranked.map((entry) => (
              <ModelCard key={entry.model.slug} model={entry.model} rank={entry.rank} stats={entry.stats} metric={sortMetric(entry, sort)} />
            ))}
          </div>
        ) : (
          <TableView ranked={ranked} sort={sort} />
        )}

        <p className="mt-6 text-xs text-muted">Catalog usage and rankings are based on sample data.</p>
      </Container>
      {advisorOpen && <ModelAdvisor onClose={() => setAdvisorOpen(false)} />}
    </>
  );
}

function ViewButton({ active, onClick, icon: Icon, label }: { active: boolean; onClick: () => void; icon: typeof LayoutGrid; label: string }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-pressed={active}
      className={cn("flex min-h-8 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium motion-safe:transition-colors", focusRing, active ? "bg-surface2 text-fg" : "text-muted hover:bg-surface2/60 hover:text-fg")}
    >
      <Icon className="h-4 w-4" aria-hidden="true" />
      <span className="hidden sm:inline">{label}</span>
    </button>
  );
}

function TableView({ ranked, sort }: { ranked: RankedModel[]; sort: SortKey }) {
  const navigate = useNavigate();
  const metricLabel = sortMetric(ranked[0], sort).label;
  return (
    <div className="scrollbar-thin overflow-x-auto rounded-2xl border border-border bg-surface motion-safe:animate-fade-up">
      <table className="w-full text-sm">
        <thead className="border-b border-border bg-surface2/40 text-left text-muted">
          <tr>
            <th className="px-4 py-3 font-medium">#</th>
            <th className="px-4 py-3 font-medium">Model</th>
            <th className="hidden px-4 py-3 font-medium sm:table-cell">Modality</th>
            <th className="px-4 py-3 text-right font-medium">{metricLabel}</th>
            <th className="hidden px-4 py-3 text-right font-medium sm:table-cell">Weekly</th>
            <th className="hidden px-4 py-3 lg:table-cell">14-day</th>
          </tr>
        </thead>
        <tbody>
          {ranked.map((rm) => {
            const m = rm.model;
            const met = sortMetric(rm, sort);
            const up = rm.stats.wow >= 0;
            return (
              <tr
                key={m.slug}
                onClick={(event) => {
                  if (event.target instanceof Element && !event.target.closest("a")) navigate(`/models/${m.slug}`);
                }}
                className="cursor-pointer border-b border-border/60 last:border-0 hover:bg-surface2/40 motion-safe:transition-colors"
              >
                <td className="px-4 py-3 font-mono text-muted">{rm.rank}</td>
                <td className="px-4 py-3">
                  <div className="flex items-center gap-2">
                    <ModelIcon model={m} />
                    <div className="min-w-0">
                      <div className="flex items-center gap-1.5">
                        <Link to={`/models/${m.slug}`} className={cn("rounded-sm font-medium text-fg hover:text-brand", focusRing)}>{m.name}</Link>
                        {m.status === "new" && <Badge tone="green">New</Badge>}
                      </div>
                      <div className="text-xs text-muted">{m.provider}</div>
                    </div>
                  </div>
                </td>
                <td className="hidden px-4 py-3 sm:table-cell">
                  <Badge>{MODALITY_LABEL[m.modality]}</Badge>
                </td>
                <td className="px-4 py-3 text-right font-semibold text-fg">{met.value}</td>
                <td className="hidden px-4 py-3 text-right sm:table-cell">
                  <span className={cn("inline-flex items-center gap-0.5 text-xs font-semibold", up ? "text-emerald-500" : "text-rose-500")}>
                    {up ? <TrendingUp className="h-3.5 w-3.5" /> : <TrendingDown className="h-3.5 w-3.5" />}
                    {up ? "+" : ""}
                    {(rm.stats.wow * 100).toFixed(0)}%
                  </span>
                </td>
                <td className="hidden px-4 py-3 lg:table-cell">
                  <Sparkline data={rm.stats.sparkline} />
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
