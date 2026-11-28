import { useEffect, useRef, useState, type FormEvent } from "react";
import { ArrowRight, ArrowUpRight, Compass, LoaderCircle, X } from "lucide-react";
import { Link, useLocation } from "react-router-dom";
import { Button, Textarea } from "@/components/ui";
import { ModelIcon } from "@/components/ModelArt";
import { useAuth } from "@/lib/auth";
import { MODEL_BY_SLUG } from "@/lib/catalog";
import { modelComparisonUrl, selectedComparisonModels, suggestedExample } from "@/lib/model-comparison";
import { entryPath } from "@/lib/onboarding";
import { advisorApi, catalogAdvice, type Advice } from "./model-advisor";

type Result = { advice: Advice; job: string; researched: boolean; notice?: string };

export function ModelAdvisor({ onClose }: { onClose: () => void }) {
  const { user, loading: authLoading } = useAuth();
  const location = useLocation();
  const [available, setAvailable] = useState(false);
  const [web, setWeb] = useState(true);
  const [job, setJob] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Result>();
  const dialog = useRef<HTMLDialogElement>(null);
  const request = useRef<AbortController | null>(null);

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    const controller = new AbortController();
    advisorApi.status(controller.signal).then(({ web_research }) => {
      if (!controller.signal.aborted) setAvailable(web_research);
    }).catch(() => {});
    return () => {
      controller.abort();
      request.current?.abort();
      element?.close();
    };
  }, []);

  function dismiss() {
    request.current?.abort();
    dialog.current?.close();
    onClose();
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const description = job.trim();
    if (description.length < 8 || request.current) return;
    if (!available || !user || !web) {
      setResult({ advice: catalogAdvice(description), job: description, researched: false });
      return;
    }

    const controller = new AbortController();
    request.current = controller;
    setResult(undefined);
    setBusy(true);
    try {
      const advice = await advisorApi.research(description, controller.signal);
      if (!controller.signal.aborted) setResult({ advice, job: description, researched: true });
    } catch {
      if (!controller.signal.aborted) {
        setResult({ advice: catalogAdvice(description), job: description, researched: false, notice: "Web research was unavailable. These matches use the catalog." });
      }
    } finally {
      request.current = null;
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return (
    <dialog
      ref={dialog}
      aria-labelledby="model-advisor-title"
      aria-describedby="model-advisor-description"
      onCancel={(event) => { event.preventDefault(); dismiss(); }}
      onClick={(event) => { if (event.target === event.currentTarget) dismiss(); }}
      className="model-advisor-dialog scrollbar-thin m-auto max-h-[calc(100dvh-1.5rem)] w-[calc(100%-1.5rem)] max-w-xl overflow-y-auto overscroll-contain rounded-2xl border border-border bg-surface text-fg shadow-2xl"
    >
      <header className="sticky top-0 z-10 flex items-center gap-3 border-b border-border bg-surface p-4 sm:px-6">
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-brand/10 text-brand">
          <Compass className="h-[1.125rem] w-[1.125rem]" aria-hidden="true" />
        </span>
        <h2 id="model-advisor-title" className="font-display text-xl font-bold tracking-tight">Model Advisor</h2>
        <Button type="button" variant="ghost" aria-label="Close Model Advisor" onClick={dismiss} className="ml-auto h-10 w-10 shrink-0 rounded-lg p-0 motion-reduce:transition-none">
          <X className="h-5 w-5" aria-hidden="true" />
        </Button>
      </header>

      <form className="space-y-4 p-4 sm:px-6 sm:py-5" onSubmit={submit}>
        <p id="model-advisor-description" className="text-sm leading-6 text-muted">Describe what you want to build to get a shortlist and reasons to choose each model.</p>
        <div>
          <label htmlFor="model-advisor-job" className="mb-1.5 block text-sm font-semibold">What are you trying to do?</label>
          <Textarea
            id="model-advisor-job"
            value={job}
            onChange={(event) => setJob(event.target.value)}
            required minLength={8} maxLength={1000} rows={3} readOnly={busy}
            placeholder="Turn a product photo into a six-second video with dialogue."
            className="block resize-y bg-bg/75 text-base motion-reduce:transition-none sm:text-sm"
          />
        </div>
        {available && user && (
          <div>
            <label className="inline-flex items-center gap-2 text-sm font-medium">
              <input type="checkbox" className="h-4 w-4 accent-brand" checked={web} onChange={(event) => setWeb(event.target.checked)} disabled={busy} />
              Include AI web research
            </label>
            {web && <p className="mt-0.5 text-xs text-muted">Web research sends your description to OpenRouter.</p>}
          </div>
        )}
        <div className="flex flex-wrap items-center justify-end gap-x-4 gap-y-2">
          {available && !authLoading && !user && (
            <Link className="mr-auto py-1 text-sm font-medium text-brand hover:underline" to={entryPath("/login", `${location.pathname}${location.search}#advisor`)}>
              Sign in for AI web research
            </Link>
          )}
          <Button type="submit" disabled={busy || job.trim().length < 8} className="min-w-48 rounded-lg motion-reduce:transition-none">
            {busy ? "Researching…" : "Recommend models"}
            {busy ? <LoaderCircle className="h-4 w-4 motion-safe:animate-spin" aria-hidden="true" /> : <ArrowRight className="h-4 w-4" aria-hidden="true" />}
          </Button>
        </div>
      </form>

      <p role="status" className="sr-only">
        {busy ? "Researching models…" : result ? `${result.researched ? "Web-researched" : "Catalog"} shortlist ready.` : ""}
      </p>
      {result && <AdviceResults result={result} />}
    </dialog>
  );
}

function AdviceResults({ result }: { result: Result }) {
  const recommendations = result.advice.recommendations.flatMap((recommendation) => {
    return Object.hasOwn(MODEL_BY_SLUG, recommendation.slug)
      ? [{ model: MODEL_BY_SLUG[recommendation.slug], recommendation }]
      : [];
  });
  const peers = result.advice.comparison_slugs;
  const comparison = selectedComparisonModels(recommendations
    .map(({ model }) => model.slug)
    .filter((slug) => !peers || peers.includes(slug)));

  return (
    <section className="space-y-3 border-t border-border bg-surface2/40 p-4 motion-safe:animate-fade-up sm:p-6" aria-labelledby="advisor-result-title">
      <h3 id="advisor-result-title" className="font-display text-lg font-bold">{result.researched ? "Web-researched shortlist" : "Catalog shortlist"}</h3>
      {comparison.length > 1 && (
        <Link to={modelComparisonUrl(comparison, suggestedExample(comparison, result.job))}
          className="inline-flex min-h-10 items-center gap-2 rounded-lg border border-brand/30 bg-brand/10 px-3 text-sm font-semibold text-brand hover:bg-brand/15 focus-visible:outline focus-visible:outline-2 focus-visible:outline-brand">
          Compare {comparison.length} models <ArrowRight className="h-4 w-4" aria-hidden />
        </Link>
      )}
      {result.notice && <p className="text-sm text-muted">{result.notice}</p>}
      {!recommendations.length && <p className="text-sm text-muted">No clear match. Try adding a specific deliverable or requirement.</p>}
      {recommendations.length > 0 && (
        <ol className="grid gap-2.5">
          {recommendations.map(({ model, recommendation }) => (
            <li key={model.slug} className="min-w-0 space-y-2 rounded-xl border border-border bg-surface p-3.5">
              <Link to={`/models/${model.slug}`} className="group flex items-center gap-2.5 rounded-md font-semibold hover:text-brand focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand">
                <ModelIcon model={model} />
                <span className="min-w-0 break-words">{model.name}</span>
                <ArrowUpRight className="ml-auto h-4 w-4 shrink-0 text-muted group-hover:text-brand motion-safe:transition-transform motion-safe:group-hover:-translate-y-0.5 motion-safe:group-hover:translate-x-0.5" aria-hidden="true" />
              </Link>
              <p className="text-sm leading-5 text-fg">{recommendation.reason}</p>
              {recommendation.tradeoff && <p className="text-sm leading-5 text-muted">{recommendation.tradeoff}</p>}
              {recommendation.community && (
                <div className="space-y-1 border-t border-border pt-2 text-xs leading-5 text-muted">
                  <p className="font-medium text-fg">Community feedback</p>
                  {recommendation.community.average_rating !== null && <p>
                    {recommendation.community.average_rating.toFixed(2)} / 10 · {recommendation.community.rating_count} ratings
                  </p>}
                  {recommendation.community.strengths.map((theme) => <p key={theme}><span className="font-medium">Reported strength</span> · {theme}</p>)}
                  {recommendation.community.weaknesses.map((theme) => <p key={theme}><span className="font-medium">Reported weakness</span> · {theme}</p>)}
                </div>
              )}
            </li>
          ))}
        </ol>
      )}
      {result.advice.sources.length > 0 && (
        <div>
          <h4 className="text-sm font-semibold">Sources</h4>
          <ul className="mt-1 space-y-1">
            {result.advice.sources.map(({ url, title }) => (
              <li key={url}><a href={url} target="_blank" rel="noopener noreferrer" className="break-words text-sm text-brand hover:underline">{title || new URL(url).hostname}</a></li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
