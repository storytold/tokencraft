import { useMemo, useState } from "react";
import { ArrowUpRight, Loader2, Play, RotateCcw, Sparkles } from "lucide-react";
import { Link, useSearchParams } from "react-router-dom";
import { Badge, Button, Card, Input, Label, Select, Slider, Textarea } from "@/components/ui";
import { AccountModal } from "@/components/AccountModal";
import { useAuth } from "@/lib/auth";
import { mockGenerate, playgroundHistory, type GenerateResult, type PlayRun } from "@/lib/mock-api";
import { MODALITY_LABEL, priceLabel, type Control, type Model } from "@/lib/catalog";
import { cn } from "@/lib/cn";
import { demoEnabled } from "@/lib/gateway";
import { generationEndpoint, modelPrimitive } from "@/lib/generation-contract";

type Values = Record<string, unknown>;

function defaults(model: Model, initial?: Values): Values {
  const v: Values = {};
  for (const c of model.controls) {
    if (c.type === "slider") v[c.key] = c.default;
    else if (c.type === "toggle") v[c.key] = c.default ?? false;
    else if (c.type === "select") v[c.key] = c.default ?? c.options[0]?.value;
    else if (c.type === "text" || c.type === "textarea") v[c.key] = c.default ?? "";
    else if (c.type === "seed") v[c.key] = "";
    // remix: overlay any provided value for a control this model actually has
    if (initial && initial[c.key] !== undefined && initial[c.key] !== null) v[c.key] = initial[c.key];
  }
  return v;
}

export function Playground({ model, initialValues }: { model: Model; initialValues?: Values }) {
  const [current] = useSearchParams();
  const destination = new URLSearchParams({ model: model.slug });
  for (const key of ["account_id", "project_id"]) {
    const value = current.get(key);
    if (value) destination.set(key, value);
  }
  const runnable = demoEnabled && generationEndpoint(modelPrimitive(model.modality));
  return (
    <div className="space-y-5">
      {runnable && <Card className="flex flex-wrap items-center justify-between gap-3 p-4">
        <p className="text-sm text-muted">Run requests with the model aliases enabled for your project.</p>
        <Link to={`/playground?${destination}`}
          className="inline-flex items-center gap-1.5 text-sm font-medium text-brand hover:underline">
          Open project playground <ArrowUpRight className="h-4 w-4" aria-hidden />
        </Link>
      </Card>}
      <MockPlayground model={model} initialValues={initialValues} />
    </div>
  );
}

function MockPlayground({ model, initialValues }: { model: Model; initialValues?: Values }) {
  const { user } = useAuth();
  const [values, setValues] = useState<Values>(() => defaults(model, initialValues));
  const [status, setStatus] = useState<"idle" | "running" | "done">("idle");
  const [result, setResult] = useState<GenerateResult | null>(null);
  const [sessionRuns, setSessionRuns] = useState<PlayRun[]>([]);
  const [showAccountModal, setShowAccountModal] = useState(false);

  const set = (k: string, val: unknown) => setValues((v) => ({ ...v, [k]: val }));

  // The actual generation — assumes the auth gate already passed.
  const execute = async () => {
    setStatus("running");
    setResult(null);
    const r = await mockGenerate(model, values);
    setResult(r);
    setStatus("done");
    setSessionRuns((prev) => [
      {
        id: r.requestId,
        modelSlug: model.slug,
        status: "succeeded",
        createdAt: new Date(Date.parse("2026-07-19T14:32:00Z") + prev.length * 60_000).toISOString(),
        costUsd: r.costUsd,
        latencyMs: r.latencyMs,
        mine: true,
        endUser: null,
        hue: (prev.length * 47 + 200) % 360,
      },
      ...prev,
    ]);
  };

  // Playing (and paying) requires an account — anonymous visitors get the signup modal, which
  // resumes the run via `execute` directly (no stale auth-state check) once they're signed in.
  const run = async () => {
    if (!user) {
      setShowAccountModal(true);
      return;
    }
    await execute();
  };

  const reset = () => {
    setValues(defaults(model));
    setStatus("idle");
    setResult(null);
  };

  return (
    <div className="space-y-6">
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
      {/* ── Controls ── */}
      <Card className="p-5">
        <div className="mb-4 flex items-center justify-between">
          <h3 className="font-display font-semibold">Inputs</h3>
          <Badge tone="brand">{MODALITY_LABEL[model.modality]}</Badge>
        </div>
        <div className="space-y-4">
          {model.controls.map((c) => (
            <ControlField key={c.key} control={c} value={values[c.key]} onChange={(val) => set(c.key, val)} />
          ))}
        </div>
        <div className="mt-6 flex items-center gap-2">
          <Button onClick={run} disabled={status === "running"} className="flex-1">
            {status === "running" ? <Loader2 className="h-4 w-4 animate-spin" /> : <Play className="h-4 w-4" />}
            {status === "running" ? "Generating…" : "Preview"}
          </Button>
          <Button variant="outline" onClick={reset} disabled={status === "running"}>
            <RotateCcw className="h-4 w-4" />
          </Button>
        </div>
        <p className="mt-3 text-xs text-muted">
          Estimated {priceLabel(model.pricing[0])}. Mock run — no request leaves your browser.
        </p>
      </Card>

      {/* ── Output ── */}
      <Card className="flex flex-col overflow-hidden">
        <div className="flex items-center justify-between border-b border-border px-5 py-3">
          <h3 className="font-display font-semibold">Output</h3>
          {result && <Badge tone="green">succeeded</Badge>}
        </div>
        <div className="grid flex-1 place-items-center p-6">
          {status === "idle" && (
            <div className="text-center text-muted">
              <Sparkles className="mx-auto mb-3 h-8 w-8 text-brand/60" />
              <p className="text-sm">Set your inputs and select Preview.</p>
            </div>
          )}
          {status === "running" && (
            <div className="w-full">
              <div className="aspect-video w-full animate-pulse rounded-xl bg-surface2" />
              <p className="mt-4 text-center text-sm text-muted">Generating a mock {MODALITY_LABEL[model.modality].toLowerCase()}…</p>
            </div>
          )}
          {status === "done" && result && (
            <div className="w-full">
              <div className="relative grid aspect-video w-full place-items-center overflow-hidden rounded-xl brand-gradient">
                <div className="absolute inset-0 grid-bg opacity-30" />
                <div className="relative text-center text-white">
                  <p className="text-5xl">✦</p>
                  <p className="mt-2 text-sm opacity-80">Mock {MODALITY_LABEL[model.modality]} preview</p>
                </div>
              </div>
              <div className="mt-4 grid grid-cols-3 gap-3 text-sm">
                <Stat label="Request" value={result.requestId} mono />
                <Stat label="Latency" value={`${(result.latencyMs / 1000).toFixed(1)}s`} />
                <Stat label="Cost" value={`$${result.costUsd.toFixed(4)}`} />
              </div>
              <p className="mt-3 rounded-lg bg-surface2/60 px-3 py-2 text-xs text-muted">
                Cost breakdown: {result.costBreakdown}
              </p>
            </div>
          )}
        </div>
      </Card>
    </div>

    <PlaygroundHistory model={model} sessionRuns={sessionRuns} />

    <AccountModal
      open={showAccountModal}
      onClose={() => setShowAccountModal(false)}
      onAuthed={() => void execute()}
      reason={`Create a free account to preview ${model.name}.`}
    />
    </div>
  );
}

function PlaygroundHistory({ model, sessionRuns }: { model: Model; sessionRuns: PlayRun[] }) {
  const [scope, setScope] = useState<"mine" | "account">("mine");
  const seeded = useMemo(() => playgroundHistory(model.slug), [model.slug]);

  const all = [...sessionRuns, ...seeded];
  const rows = (scope === "mine" ? all.filter((r) => r.mine) : all).slice(0, 12);

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-col gap-3 border-b border-border px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h3 className="font-display font-semibold">Request history</h3>
          <p className="text-xs text-muted">Recent {model.name} runs, newest first.</p>
        </div>
        <div className="inline-flex rounded-lg border border-border bg-surface p-1">
          {(["mine", "account"] as const).map((s) => (
            <button
              key={s}
              onClick={() => setScope(s)}
              className={cn(
                "rounded-md px-3 py-1.5 text-xs font-medium transition",
                scope === s ? "brand-gradient text-white shadow-glow" : "text-muted hover:text-fg",
              )}
            >
              {s === "mine" ? "My runs" : "All account traffic"}
            </button>
          ))}
        </div>
      </div>
      {scope === "account" && (
        <p className="border-b border-border bg-brand/[0.05] px-5 py-2 text-xs text-muted">
          Everything that transited the account for this model — including your customers' end-users (via{" "}
          <code className="rounded bg-surface2 px-1 py-0.5">end_user</code>).
        </p>
      )}
      <div className="scrollbar-thin overflow-x-auto">
        <table className="w-full text-sm">
          <tbody>
            {rows.map((r) => (
              <tr key={r.id} className="border-b border-border/60 last:border-0 hover:bg-surface2/40">
                <td className="py-3 pl-5 pr-3">
                  <div
                    className="h-10 w-10 shrink-0 rounded-lg"
                    style={{ background: `linear-gradient(135deg, hsl(${r.hue} 80% 55%), hsl(${(r.hue + 60) % 360} 80% 55%))` }}
                  />
                </td>
                <td className="px-3 py-3">
                  <div className="font-mono text-[13px] text-fg">{r.id}</div>
                  <div className="text-xs text-muted">{shortTime(r.createdAt)}</div>
                </td>
                <td className="px-3 py-3">
                  {r.endUser ? (
                    <span className="font-mono text-xs text-muted">{r.endUser}</span>
                  ) : r.mine ? (
                    <span className="text-xs text-brand">you</span>
                  ) : (
                    <span className="text-xs text-muted">API</span>
                  )}
                </td>
                <td className="px-3 py-3">
                  {r.status === "succeeded" && <Badge tone="green">succeeded</Badge>}
                  {r.status === "running" && <Badge tone="amber">running</Badge>}
                  {r.status === "failed" && <Badge tone="red">failed</Badge>}
                </td>
                <td className="hidden px-3 py-3 text-right text-xs text-muted sm:table-cell">{(r.latencyMs / 1000).toFixed(1)}s</td>
                <td className="py-3 pl-3 pr-5 text-right font-medium text-fg">${r.costUsd.toFixed(3)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

function shortTime(iso: string): string {
  const [date, time] = iso.replace("Z", "").split("T");
  const [, m, d] = date.split("-");
  const months = ["", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  return `${months[Number(m)]} ${Number(d)}, ${time.slice(0, 5)}`;
}

function Stat({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="rounded-lg bg-surface2/60 px-3 py-2">
      <div className="text-xs text-muted">{label}</div>
      <div className={cn("truncate font-medium text-fg", mono && "font-mono text-[13px]")}>{value}</div>
    </div>
  );
}

function ControlField({ control, value, onChange }: { control: Control; value: unknown; onChange: (v: unknown) => void }) {
  switch (control.type) {
    case "text":
      return (
        <div>
          <Label hint={control.hint}>{control.label}</Label>
          <Input placeholder={control.placeholder} value={(value as string) ?? ""} onChange={(e) => onChange(e.target.value)} />
        </div>
      );
    case "textarea":
      return (
        <div>
          <Label hint={control.hint}>{control.label}</Label>
          <Textarea rows={3} placeholder={control.placeholder} value={(value as string) ?? ""} onChange={(e) => onChange(e.target.value)} />
        </div>
      );
    case "select":
      return (
        <div>
          <Label hint={control.hint}>{control.label}</Label>
          <Select value={(value as string) ?? ""} onChange={(e) => onChange(e.target.value)}>
            {control.options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </Select>
        </div>
      );
    case "slider":
      return (
        <div>
          <Label hint={String(value)}>{control.label}</Label>
          <Slider value={Number(value)} min={control.min} max={control.max} step={control.step} onChange={onChange} />
        </div>
      );
    case "toggle":
      return (
        <label className="flex items-center justify-between">
          <span className="text-sm font-medium text-fg">{control.label}</span>
          <button
            type="button"
            onClick={() => onChange(!value)}
            className={cn("relative h-6 w-11 rounded-full transition", value ? "brand-gradient" : "bg-surface2")}
          >
            <span className={cn("absolute top-0.5 h-5 w-5 rounded-full bg-white transition", value ? "left-[1.375rem]" : "left-0.5")} />
          </button>
        </label>
      );
    case "seed":
      return (
        <div>
          <Label hint={control.hint}>{control.label}</Label>
          <Input type="number" placeholder="random" value={(value as string) ?? ""} onChange={(e) => onChange(e.target.value)} />
        </div>
      );
  }
}
