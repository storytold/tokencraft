import { Link } from "react-router-dom";
import { ArrowUpRight, TrendingDown, TrendingUp } from "lucide-react";
import { Badge, Card } from "@/components/ui";
import { Sparkline } from "@/components/Sparkline";
import { ModelIcon } from "@/components/ModelArt";
import { MODALITY_LABEL, type Model } from "@/lib/catalog";
import type { ModelStats } from "@/lib/model-stats";

/** Optional ranking/usage overlay for the catalog index (design §3.3). */
export function ModelCard({
  model,
  rank,
  stats,
  metric,
}: {
  model: Model;
  rank?: number;
  stats?: ModelStats;
  metric?: { label: string; value: string };
}) {
  return (
    <Link to={`/models/${model.slug}`} className="group block rounded-2xl focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand">
      <Card className="flex h-full flex-col gap-3 p-4 group-hover:border-brand/55 group-hover:shadow-md motion-safe:transition motion-safe:duration-200 motion-safe:group-hover:-translate-y-0.5">
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2.5">
            <ModelIcon model={model} />
            <span className="truncate text-sm font-medium text-muted">{model.provider}</span>
          </div>
          <div className="flex shrink-0 items-center gap-2 text-muted">
            {rank !== undefined && <span className="font-mono text-xs tabular-nums">#{rank}</span>}
            <ArrowUpRight className="h-4 w-4 group-hover:text-brand motion-safe:transition-transform motion-safe:group-hover:-translate-y-0.5 motion-safe:group-hover:translate-x-0.5" aria-hidden="true" />
          </div>
        </div>
        <div className="grow space-y-1.5">
          <h3 className="font-display text-lg font-bold leading-tight group-hover:text-brand">{model.name}</h3>
          <p className="line-clamp-2 text-sm leading-5 text-muted">{model.tagline}</p>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge className="text-xs">{MODALITY_LABEL[model.modality]}</Badge>
          {model.status === "new" && <Badge className="text-xs" tone="green">New</Badge>}
          {model.status === "preview" && <Badge className="text-xs" tone="amber">Preview</Badge>}
        </div>
        {stats && metric && (
          <div className="flex items-end justify-between gap-2 border-t border-border pt-4">
            <div>
              <div className="text-xs text-muted">{metric.label}</div>
              <div className="flex items-center gap-1.5 text-sm font-semibold text-fg">
                {metric.value}
                <WoW ratio={stats.wow} />
              </div>
            </div>
            <Sparkline data={stats.sparkline} />
          </div>
        )}
      </Card>
    </Link>
  );
}

function WoW({ ratio }: { ratio: number }) {
  const up = ratio >= 0;
  return (
    <span className={up ? "inline-flex items-center text-xs font-medium text-emerald-500" : "inline-flex items-center text-xs font-medium text-rose-500"}>
      {up ? <TrendingUp className="h-3 w-3" /> : <TrendingDown className="h-3 w-3" />}
    </span>
  );
}
