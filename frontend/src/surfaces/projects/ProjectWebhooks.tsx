import { useEffect, useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { Button, Card, Input, Select } from "@/components/ui";
import { ApiError } from "@/lib/api";
import { useAuth } from "@/lib/auth";
import { errorMessage, isGatewayEndpointUnavailable, withProject, type ProjectScope } from "@/lib/gateway";
import { OneTimeSecret } from "./OneTimeSecret";
import {
  webhooksApi, type IssuedWebhook, type WebhookDelivery, type WebhookDetail, type WebhookPage, type WebhookSubscription,
} from "./api";
import { clearWebhookRotation, pendingWebhookRotation, rotationStorageKey, stageWebhookRotation } from "./webhookRotation";

type Webhooks = { destinations: string[]; subscriptions: WebhookPage<WebhookSubscription>; deliveries: WebhookPage<WebhookDelivery> };
type Collection = "subscriptions" | "deliveries";
const date = (value: string) => new Date(value).toLocaleString();
const stateLabels: Record<WebhookDelivery["state"], string> = {
  pending: "Pending", sending: "Sending", delivered: "Delivered", dead: "Failed",
};

export function ProjectWebhooks({ scope }: { scope: ProjectScope }) {
  const { user } = useAuth();
  return <ProjectWebhookPanel key={`${scope.account_id}/${scope.project_id}/${user?.id ?? ""}`}
    scope={scope} actorId={user?.id} />;
}

function ProjectWebhookPanel({ scope, actorId }: { scope: ProjectScope; actorId?: string }) {
  const [data, setData] = useState<Webhooks | null>();
  const [before, setBefore] = useState<Partial<Record<Collection, string>>>({});
  const [revision, setRevision] = useState(0);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [issued, setIssued] = useState<IssuedWebhook>();
  const [selected, setSelected] = useState<string>();
  const refresh = () => setRevision((value) => value + 1);

  useEffect(() => {
    const controller = new AbortController();
    const signal = controller.signal;
    Promise.all([
      webhooksApi.destinations(scope, signal),
      webhooksApi.subscriptions(scope, before.subscriptions, signal),
      webhooksApi.deliveries(scope, before.deliveries, signal),
    ]).then(([destinations, subscriptions, deliveries]) => {
      if (!signal.aborted) setData({ destinations, subscriptions, deliveries });
    }).catch((error) => {
      if (signal.aborted) return;
      if (isGatewayEndpointUnavailable(error)) setData(null);
      else setError(errorMessage(error));
    });
    return () => controller.abort();
  }, [scope, revision, before]);

  async function change(action: () => Promise<void | IssuedWebhook>, notice = "") {
    if (busy || issued) return false;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const next = await action();
      if (next) setIssued(next);
      setNotice(notice);
      refresh();
      return true;
    } catch (error) {
      setError(errorMessage(error));
      return false;
    } finally {
      setBusy(false);
    }
  }

  function rotationKey(id: string) {
    return actorId && rotationStorageKey(actorId, scope.account_id, scope.project_id, id);
  }

  function rotate(subscription: WebhookSubscription) {
    const storageKey = rotationKey(subscription.id);
    if (!storageKey || busy || issued) return;
    const attempt = stageWebhookRotation(storageKey, subscription.signing_version);
    void change(async () => {
      try {
        const result = await webhooksApi.rotate(scope, subscription.id, attempt.key, attempt.expectedVersion);
        if (!result || result.id !== subscription.id || result.signing_version !== attempt.expectedVersion + 1
            || typeof result.signing_secret !== "string" || !result.signing_secret.startsWith("whsec_")) {
          throw new ApiError(0, "rotation_receipt", "The rotation response was incomplete. Retry to recover the signing key.");
        }
        return result;
      } catch (error) {
        if (error instanceof ApiError && error.status === 409) {
          clearWebhookRotation(storageKey);
          refresh();
        }
        if (error instanceof ApiError && error.status === 404) {
          throw new ApiError(404, "rotation_unavailable", "This gateway version does not support signing key recovery.");
        }
        if (error instanceof SyntaxError || (error instanceof ApiError && error.status === 0)
            || (error instanceof DOMException && error.name === "TimeoutError")) {
          throw new ApiError(0, "rotation_receipt", "The rotation response was interrupted. Retry to recover the signing key.");
        }
        throw error;
      }
    }, "Update the receiver before the signing key overlap ends.");
  }

  function dismissSecret() {
    if (issued) {
      const storageKey = rotationKey(issued.id);
      if (storageKey) clearWebhookRotation(storageKey);
    }
    setIssued(undefined);
  }

  const disabled = busy || Boolean(issued);
  return (
    <Card className="min-w-0 space-y-4 p-4 sm:p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h3 className="font-display text-xl font-semibold">Webhooks</h3>
          <p className="mt-1 text-sm text-muted">Send signed request or workflow outcomes to a registered receiver.</p>
        </div>
        <Button variant="outline" size="sm" onClick={() => { setError(""); refresh(); }}>Refresh webhooks</Button>
      </div>
      {data === undefined && !error && <p className="text-sm text-muted" role="status">Loading webhooks…</p>}
      {data === null && <p className="text-sm text-muted" role="status">Webhook delivery requires a receiver configured on this gateway.</p>}
      {error && <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
      {notice && <p className="text-sm text-muted" role="status">{notice}</p>}
      {data && <AddSubscription destinations={data.destinations} disabled={disabled} create={async (input) => {
        if (!await change(() => webhooksApi.create(scope, input))) return false;
        if (before.subscriptions) setBefore((previous) => ({ ...previous, subscriptions: undefined }));
        return true;
      }} />}
      {issued && <OneTimeSecret key={`${issued.id}:${issued.signing_version}`} label="Webhook signing secret" noun="secret"
        value={issued.signing_secret} dismiss={dismissSecret} />}
      {data && <>
        <section aria-label="Webhook subscriptions" className="space-y-2">
          <h4 className="font-semibold">Subscriptions</h4>
          {data.subscriptions.items.length === 0 && <p className="text-sm text-muted">No subscriptions in this project.</p>}
          <ul className="divide-y divide-border">
            {data.subscriptions.items.map((subscription) => {
              const storageKey = rotationKey(subscription.id);
              const recovery = storageKey && pendingWebhookRotation(storageKey, subscription.signing_version);
              return <li key={subscription.id} className="flex flex-wrap items-start justify-between gap-2 border-t border-border py-3 text-sm">
                <div className="min-w-0 break-all">
                  <p className="font-medium">{subscription.name}{subscription.revoked_at && " · Revoked"}</p>
                  <p className="text-muted">
                    {subscription.destination_id}
                    {subscription.client_ref && ` · Client ${subscription.client_ref}`}
                    {subscription.workflow_app_id && ` · Workflow app ${subscription.workflow_app_id}`}
                  </p>
                  <code className="text-xs text-muted">{subscription.id}</code>
                </div>
                {!subscription.revoked_at && <div className="flex flex-wrap items-center gap-2">
                  {!subscription.workflow_app_id && <Link className="font-medium text-brand hover:underline"
                    to={`${withProject("/routing", scope.project_id, scope.account_id)}&webhook_subscription_id=${encodeURIComponent(subscription.id)}`}>Try in comparison</Link>}
                  <Button variant="outline" size="sm" disabled={disabled || !actorId} onClick={() => rotate(subscription)}>
                    {recovery ? "Retry signing key rotation" : "Rotate signing key"}
                  </Button>
                  <Button variant="ghost" size="sm" disabled={disabled} onClick={() => {
                    if (window.confirm(`Disable ${subscription.name}?`)) void change(() => webhooksApi.revoke(scope, subscription.id), "Subscription disabled.");
                  }}>Disable subscription</Button>
                </div>}
              </li>;
            })}
          </ul>
          <PageControls label="subscriptions" before={before.subscriptions} next={data.subscriptions.next_before}
            select={(cursor) => setBefore((previous) => ({ ...previous, subscriptions: cursor }))} />
        </section>
        <section aria-label="Webhook deliveries" className="space-y-2">
          <h4 className="font-semibold">Deliveries</h4>
          {data.deliveries.items.length === 0 && <p className="text-sm text-muted">No webhook deliveries yet.</p>}
          <ul className="divide-y divide-border">
            {data.deliveries.items.map((delivery) => (
              <li key={delivery.id} className="border-t border-border py-3 text-sm">
                <button type="button" className="flex w-full flex-wrap justify-between gap-2 text-left hover:text-brand"
                  aria-expanded={selected === delivery.id} onClick={() => setSelected(selected === delivery.id ? undefined : delivery.id)}>
                  <span className="min-w-0 break-all"><span className="font-medium">{delivery.event_type}</span>{" "}
                    <span className="font-mono text-xs text-muted">{delivery.request_id ?? delivery.workflow_run_id}</span>
                  </span>
                  <span>{stateLabels[delivery.state]} · {delivery.attempt_count} {delivery.attempt_count === 1 ? "attempt" : "attempts"}</span>
                </button>
                {selected === delivery.id && <DeliveryAttempts scope={scope} id={delivery.id} revision={revision} disabled={disabled}
                  redrive={() => { void change(() => webhooksApi.redrive(scope, delivery.id), "Delivery queued for another attempt."); }} />}
              </li>
            ))}
          </ul>
          <PageControls label="deliveries" before={before.deliveries} next={data.deliveries.next_before}
            select={(cursor) => { setSelected(undefined); setBefore((previous) => ({ ...previous, deliveries: cursor })); }} />
        </section>
      </>}
    </Card>
  );
}

type CreateInput = Parameters<typeof webhooksApi.create>[1];

function AddSubscription({ destinations, disabled, create }: {
  destinations: string[]; disabled: boolean; create: (input: CreateInput) => Promise<boolean>;
}) {
  const [name, setName] = useState("");
  const [target, setTarget] = useState(destinations[0] ?? "");
  const [client, setClient] = useState("");
  const [eventSource, setEventSource] = useState("request");
  const [app, setApp] = useState("");
  const workflow = eventSource === "workflow";
  const canSubmit = !disabled && Boolean(name.trim() && target && (!workflow || app.trim()));
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!canSubmit) return;
    if (await create({
      name: name.trim(), destination_id: target, client_ref: workflow ? null : client.trim() || null,
      workflow_app_id: workflow ? app.trim() : undefined,
    })) {
      setName("");
      setClient("");
      setApp("");
    }
  }
  return (
    <form onSubmit={submit} className="flex flex-wrap items-end gap-3">
      <label className="min-w-0 w-full max-w-52 space-y-1 text-sm font-medium">Subscription name
        <Input value={name} required maxLength={80} disabled={disabled} onChange={(event) => setName(event.target.value)} />
      </label>
      <label className="min-w-0 w-full max-w-52 space-y-1 text-sm font-medium">Webhook destination
        <Select value={target} required disabled={disabled} onChange={(event) => setTarget(event.target.value)}>
          {destinations.map((id) => <option key={id} value={id}>{id}</option>)}
        </Select>
      </label>
      <label className="min-w-0 w-full max-w-52 space-y-1 text-sm font-medium">Event source
        <Select value={eventSource} disabled={disabled} onChange={(event) => setEventSource(event.target.value)}>
          <option value="request">Requests</option>
          <option value="workflow">Workflow app</option>
        </Select>
      </label>
      {workflow ? <label className="min-w-0 w-full max-w-52 space-y-1 text-sm font-medium">Workflow app ID
        <Input value={app} required maxLength={64} placeholder="wfa_…" disabled={disabled} onChange={(event) => setApp(event.target.value)} />
      </label> : <label className="min-w-0 w-full max-w-52 space-y-1 text-sm font-medium">Webhook client reference
        <Input value={client} maxLength={64} placeholder="Optional" disabled={disabled} onChange={(event) => setClient(event.target.value)} />
      </label>}
      <Button type="submit" disabled={!canSubmit}>Create subscription</Button>
    </form>
  );
}

function DeliveryAttempts({ scope, id, revision, disabled, redrive }: {
  scope: ProjectScope; id: string; revision: number; disabled: boolean; redrive: () => void;
}) {
  const [result, setResult] = useState<{ detail?: WebhookDetail; error?: string }>();
  useEffect(() => {
    const controller = new AbortController();
    webhooksApi.detail(scope, id, controller.signal).then((detail) => {
      if (!controller.signal.aborted) setResult({ detail });
    }).catch((error) => {
      if (!controller.signal.aborted) setResult({ error: errorMessage(error) });
    });
    return () => controller.abort();
  }, [scope, id, revision]);
  if (!result) return <p className="mt-2 text-muted" role="status">Loading delivery attempts…</p>;
  if (result.error) return <p className="mt-2 text-rose-600 dark:text-rose-300" role="alert">{result.error}</p>;
  const { delivery, attempts } = result.detail!;
  return (
    <div className="mt-2 space-y-2 bg-surface2/45 p-3 text-xs">
      <div className="flex flex-wrap items-center gap-3">
        <code className="break-all">{delivery.id}</code>
        {delivery.request_id && <Link className="font-medium text-brand hover:underline"
          to={`${withProject(`/dashboard/${scope.account_id}/requests`, scope.project_id)}&request_id=${encodeURIComponent(delivery.request_id)}`}>Open request trace</Link>}
        {delivery.workflow_run_id && <Link className="font-medium text-brand hover:underline"
          to={withProject(`/dashboard/${scope.account_id}/workflows`, scope.project_id)}>Open Workflows</Link>}
      </div>
      {delivery.state === "pending" && <p className="text-muted">Next attempt {date(delivery.available_at)}</p>}
      {delivery.last_error && <p className="text-muted">{delivery.last_error.replaceAll("_", " ")}</p>}
      <ol className="space-y-1">
        {attempts.map((attempt) => <li key={attempt.number}>
          Attempt {attempt.number} · {date(attempt.started_at)} · {attempt.outcome?.replaceAll("_", " ") ?? "sending"}
          {attempt.http_status && ` (HTTP ${attempt.http_status})`}
        </li>)}
      </ol>
      {delivery.state === "dead" && <Button variant="outline" size="sm" disabled={disabled} onClick={redrive}>Retry delivery</Button>}
    </div>
  );
}

function PageControls({ label, before, next, select }: { label: string; before?: string; next: string | null; select: (cursor?: string) => void }) {
  if (!before && !next) return null;
  return <div className="flex flex-wrap gap-2">
    {before && <Button variant="outline" size="sm" onClick={() => select()}>Newest {label}</Button>}
    {next && <Button variant="outline" size="sm" onClick={() => select(next)}>Older {label}</Button>}
  </div>;
}
