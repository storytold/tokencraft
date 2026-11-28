import { useEffect, useRef, useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { Button, Card, Input, Select } from "@/components/ui";
import { ApiError, isDefiniteClientRejection } from "@/lib/api";
import { useAuth } from "@/lib/auth";
import { errorMessage, isGatewayEndpointUnavailable, withProject, type ProjectScope } from "@/lib/gateway";
import { OneTimeSecret } from "./OneTimeSecret";
import {
  webhooksApi, type IssuedWebhook, type WebhookDelivery, type WebhookDetail, type WebhookPage, type WebhookSubscription,
} from "./api";
import {
  clearWebhookReceipt, creationStorageKey, pendingWebhookCreation, pendingWebhookRotation,
  rotationStorageKey, stageWebhookCreation, stageWebhookRotation, type WebhookCreation, type WebhookCreationInput,
} from "./webhookReceipts";

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
  const creationKey = actorId && creationStorageKey(actorId, scope.account_id, scope.project_id);
  const [creation, setCreation] = useState(() => creationKey ? pendingWebhookCreation(creationKey) : undefined);
  const [data, setData] = useState<Webhooks | null>();
  const [before, setBefore] = useState<Partial<Record<Collection, string>>>({});
  const [revision, setRevision] = useState(0);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [issued, setIssued] = useState<IssuedWebhook>();
  const [selected, setSelected] = useState<string>();
  const activeWrite = useRef<AbortController | null>(null);
  const refresh = () => setRevision((value) => value + 1);

  useEffect(() => () => activeWrite.current?.abort(), []);

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

  async function change(action: (signal: AbortSignal) => Promise<void | IssuedWebhook>, notice = "") {
    if (activeWrite.current || issued) return false;
    const controller = new AbortController();
    activeWrite.current = controller;
    const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(15_000)]);
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const next = await action(signal);
      signal.throwIfAborted();
      if (next) setIssued(next);
      setNotice(notice);
      refresh();
      return true;
    } catch (error) {
      if (!controller.signal.aborted) setError(errorMessage(error));
      return false;
    } finally {
      activeWrite.current = null;
      setBusy(false);
    }
  }

  function clearCreation() {
    if (creationKey) clearWebhookReceipt(creationKey);
    setCreation(undefined);
  }

  async function submitCreation(attempt: WebhookCreation, allowLegacy: boolean) {
    const created = await change(async (signal) => {
      let legacy = false;
      let missingReceiptSupport = false;
      try {
        let result: IssuedWebhook;
        try {
          result = await webhooksApi.createWithReceipt(scope, attempt.input, attempt.key, signal);
        } catch (error) {
          missingReceiptSupport = isGatewayEndpointUnavailable(error) || (error instanceof ApiError && error.status === 405);
          if (!allowLegacy || !missingReceiptSupport || signal.aborted) throw error;
          legacy = true;
          clearCreation();
          result = await webhooksApi.create(scope, attempt.input, signal);
        }
        if (!result?.id || result.signing_version !== 1
            || typeof result.signing_secret !== "string" || !result.signing_secret.startsWith("whsec_")) {
          throw new Error("Incomplete subscription issuance response");
        }
        return result;
      } catch (error) {
        if (missingReceiptSupport && !legacy && !signal.aborted) {
          throw new ApiError(0, "creation_unavailable", "This gateway instance can’t recover the signing secret yet. Retry when creation recovery is available.");
        }
        if (isDefiniteClientRejection(error, signal)) {
          clearCreation();
          refresh();
          if (error.status === 409) {
            throw new ApiError(409, error.code, `${error.message}. Check Subscriptions before creating another.`);
          }
          throw error;
        }
        refresh();
        throw new ApiError(0, "creation_receipt", legacy
          ? "Couldn’t confirm creation. Check Subscriptions before creating another."
          : "The creation response was interrupted. Retry creation to recover the signing secret.");
      }
    });
    if (created && before.subscriptions) {
      setBefore((previous) => ({ ...previous, subscriptions: undefined }));
    }
    return created;
  }

  function rotationKey(id: string) {
    return actorId && rotationStorageKey(actorId, scope.account_id, scope.project_id, id);
  }

  function rotate(subscription: WebhookSubscription) {
    const storageKey = rotationKey(subscription.id);
    if (!storageKey || activeWrite.current || issued || creation) return;
    const attempt = stageWebhookRotation(storageKey, subscription.signing_version);
    void change(async (signal) => {
      try {
        const result = await webhooksApi.rotate(scope, subscription.id, attempt.key, attempt.expectedVersion, signal);
        if (!result || result.id !== subscription.id || result.signing_version !== attempt.expectedVersion + 1
            || typeof result.signing_secret !== "string" || !result.signing_secret.startsWith("whsec_")) {
          throw new ApiError(0, "rotation_receipt", "The rotation response was incomplete. Retry to recover the signing key.");
        }
        return result;
      } catch (error) {
        if (error instanceof ApiError && error.status === 409) {
          clearWebhookReceipt(storageKey);
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
      if (issued.signing_version === 1) clearCreation();
      else {
        const storageKey = rotationKey(issued.id);
        if (storageKey) clearWebhookReceipt(storageKey);
      }
    }
    setIssued(undefined);
  }

  const disabled = busy || Boolean(issued || creation);
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
      {creation && !issued && <div className="space-y-2 rounded-lg border border-border p-3">
        <p className="break-words text-sm">Pending creation for {creation.input.name}.</p>
        <div className="flex flex-wrap gap-2">
          <Button size="sm" disabled={busy} onClick={() => void submitCreation(creation, false)}>
            {busy ? "Checking creation…" : "Retry creation"}
          </Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => {
            clearCreation();
            setError("Check Subscriptions before creating another.");
            refresh();
          }}>Discard saved attempt</Button>
        </div>
      </div>}
      {data && <div hidden={Boolean(creation)}>
        <AddSubscription destinations={data.destinations} disabled={disabled || !creationKey}
          create={async (input) => {
            if (!creationKey || activeWrite.current) return false;
            const attempt = stageWebhookCreation(creationKey, input);
            setCreation(attempt);
            return submitCreation(attempt, true);
          }} />
      </div>}
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
                    if (window.confirm(`Disable ${subscription.name}?`)) void change((signal) => webhooksApi.revoke(scope, subscription.id, signal), "Subscription disabled.");
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
                  <span>{stateLabels[delivery.state]} · {delivery.attempt_count} {delivery.attempt_count === 1 ? "attempt" : "attempts"}
                    {delivery.state === "pending" && delivery.receiver_available_after && " · Receiver delay"}
                  </span>
                </button>
                {selected === delivery.id && <DeliveryAttempts scope={scope} id={delivery.id} revision={revision} disabled={disabled}
                  redrive={() => { void change((signal) => webhooksApi.redrive(scope, delivery.id, signal), "Delivery queued for another attempt."); }} />}
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

function AddSubscription({ destinations, disabled, create }: {
  destinations: string[]; disabled: boolean; create: (input: WebhookCreationInput) => Promise<boolean>;
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
  const nextAttempt = delivery.next_attempt_at === undefined ? delivery.available_at : delivery.next_attempt_at;
  return (
    <div className="mt-2 space-y-2 bg-surface2/45 p-3 text-xs">
      <div className="flex flex-wrap items-center gap-3">
        <code className="break-all">{delivery.id}</code>
        {delivery.request_id && <Link className="font-medium text-brand hover:underline"
          to={`${withProject(`/dashboard/${scope.account_id}/requests`, scope.project_id)}&request_id=${encodeURIComponent(delivery.request_id)}`}>Open request trace</Link>}
        {delivery.workflow_run_id && <Link className="font-medium text-brand hover:underline"
          to={withProject(`/dashboard/${scope.account_id}/workflows`, scope.project_id)}>Open Workflows</Link>}
      </div>
      {delivery.state === "pending" && <>
        <p className="text-muted">{nextAttempt ? `Eligible from ${date(nextAttempt)}` : "No further attempt scheduled"}</p>
        {delivery.receiver_available_after && <p className="text-muted">
          Delivery to this receiver is paused until {date(delivery.receiver_available_after)}
        </p>}
      </>}
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
