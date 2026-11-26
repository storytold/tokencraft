import { useEffect, useState } from "react";
import { Badge, Button, Card } from "@/components/ui";
import { errorMessage, gatewayRequest, type Provider } from "@/lib/gateway";
import { providerChoices, providerLabel } from "@/surfaces/comparison/api";
import { providerOnboarding } from "@/surfaces/projects/providers/provider-onboarding";

type Selection = { pause_all: boolean; paused_providers: Provider[] };
type DispatchState = Selection & { revision: number };

export function DispatchControls({ accountId }: { accountId: string }) {
  const path = `/v1/operations/dispatch?${new URLSearchParams({ account_id: accountId })}`;
  const [saved, setSaved] = useState<DispatchState>();
  const [draft, setDraft] = useState<Selection>();
  const [reload, setReload] = useState(0);
  const [pending, setPending] = useState(true);
  const [error, setError] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const [activeProviders, setActiveProviders] = useState<Provider[]>([]);

  useEffect(() => {
    const controller = new AbortController();
    providerOnboarding.inventory({ account_id: accountId }, controller.signal).then(({ offers }) => {
      if (!controller.signal.aborted) setActiveProviders(offers.map((offer) => offer.provider));
    }).catch(() => {
      // Saved pauses remain editable when provider inventory is unavailable.
    });
    gatewayRequest<DispatchState>(path, { signal: controller.signal }).then((state) => {
      if (controller.signal.aborted) return;
      setSaved(state);
      setDraft(state);
    }).catch((failure: unknown) => {
      if (!controller.signal.aborted) setError(errorMessage(failure));
    }).finally(() => {
      if (!controller.signal.aborted) setPending(false);
    });
    return () => controller.abort();
  }, [accountId, path, reload]);

  const providerOptions = providerChoices([
    ...activeProviders, ...(saved?.paused_providers ?? []), ...(draft?.paused_providers ?? []),
  ]);

  const dirty = saved && draft && (saved.pause_all !== draft.pause_all
    || saved.paused_providers.length !== draft.paused_providers.length
    || draft.paused_providers.some((provider) => !saved.paused_providers.includes(provider)));

  function edit(selection: Selection) {
    setDraft(selection);
    setConfirmed(false);
  }

  async function save() {
    if (!saved || !draft || pending || !dirty) return;
    setPending(true);
    setError("");
    setConfirmed(false);
    try {
      const state = await gatewayRequest<DispatchState>(path, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          expected_revision: saved.revision, pause_all: draft.pause_all, paused_providers: draft.paused_providers,
        }),
      });
      setSaved(state);
      setDraft(state);
      setConfirmed(true);
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setPending(false);
    }
  }

  return (
    <section aria-labelledby="dispatch-heading">
      <Card className="space-y-4 p-4 sm:p-6">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id="dispatch-heading" className="font-display text-2xl font-semibold">Dispatch controls</h2>
          {saved && (
            <Badge tone={saved.pause_all || saved.paused_providers.length ? "amber" : "green"}>
              {saved.pause_all ? "All providers paused" : saved.paused_providers.length
                ? `${saved.paused_providers.length} paused` : "Dispatch enabled"}
            </Badge>
          )}
        </div>
        <p className="text-sm text-muted">Pause new worker claims for this organization. Already-claimed work can continue, and queue deadlines still apply.</p>
        {draft && (
          <form className="space-y-3" onSubmit={(event) => { event.preventDefault(); void save(); }}>
            <fieldset disabled={pending} className="space-y-3">
              <label className="flex items-center gap-2 text-sm font-medium">
                <input type="checkbox" className="size-4 accent-current" checked={draft.pause_all}
                  onChange={(event) => edit({ ...draft, pause_all: event.target.checked })} />
                Pause all providers
              </label>
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                {providerOptions.map((provider) => (
                  <label key={provider} className="flex items-center gap-2 text-sm">
                    <input type="checkbox" className="size-4 accent-current" disabled={draft.pause_all}
                      checked={draft.paused_providers.includes(provider)} onChange={(event) => edit({
                        ...draft,
                        paused_providers: event.target.checked
                          ? [...draft.paused_providers, provider]
                          : draft.paused_providers.filter((value) => value !== provider),
                      })} />
                    Pause {providerLabel(provider)}
                  </label>
                ))}
              </div>
            </fieldset>
            <Button type="submit" size="sm" disabled={pending || !dirty}>Save dispatch controls</Button>
          </form>
        )}
        {pending && <p role="status" className="text-sm text-muted">{saved ? "Saving dispatch controls…" : "Loading dispatch controls…"}</p>}
        {confirmed && <p role="status" className="text-sm text-muted">Dispatch controls saved.</p>}
        {error && (
          <div className="space-y-2">
            <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{error}</p>
            <Button type="button" variant="outline" size="sm" disabled={pending} onClick={() => {
              setPending(true);
              setError("");
              setReload((value) => value + 1);
            }}>Reload controls</Button>
          </div>
        )}
      </Card>
    </section>
  );
}
