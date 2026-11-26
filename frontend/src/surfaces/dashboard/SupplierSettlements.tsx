import { useRef, useState, type FormEvent } from "react";
import { Button, Card, Input } from "@/components/ui";
import { errorMessage, gatewayRequest, withProject, type ProjectScope } from "@/lib/gateway";
import { formatNanoUsd, providerLabel, type Provider } from "@/surfaces/comparison/api";

type Posting = {
  revision: number;
  amount_usd_nanos: string;
  delta_usd_nanos: string;
  evidence_ref: string;
  recorded_at: string;
};
type History = {
  request_id: string;
  eligible: boolean;
  provider: Provider;
  estimated_usd_nanos: string;
  entries: Posting[];
};

export function SupplierSettlements({ scope, onRecorded }: { scope: ProjectScope; onRecorded: () => void }) {
  const [history, setHistory] = useState<History>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const retry = useRef<{ intent: string; key: string } | undefined>(undefined);
  const latest = history?.entries[0];

  function path(id: string) {
    return withProject(
      `/v1/projects/${scope.project_id}/requests/${encodeURIComponent(id)}/settlements`,
      undefined, scope.account_id,
    );
  }

  async function openHistory(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    const id = String(new FormData(event.currentTarget).get("request_id")).trim();
    setHistory(undefined);
    setError("");
    setBusy(true);
    try {
      setHistory(await gatewayRequest<History>(path(id)));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }

  async function record(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!history?.eligible || busy) return;
    const form = event.currentTarget;
    const fields = new FormData(form);
    const amount = parseUsd(String(fields.get("amount")).trim());
    if (amount === null) {
      setError("Enter a nonnegative USD amount with up to nine decimal places.");
      return;
    }
    const body = JSON.stringify({
      expected_revision: latest?.revision ?? 0,
      amount_usd_nanos: amount,
      evidence_ref: String(fields.get("evidence")).trim(),
    });
    const intent = JSON.stringify([history.request_id, body]);
    if (retry.current?.intent !== intent) retry.current = { intent, key: crypto.randomUUID() };
    setBusy(true);
    setError("");
    try {
      const endpoint = path(history.request_id);
      await gatewayRequest<Posting>(endpoint, {
        method: "POST",
        headers: { "Content-Type": "application/json", "Idempotency-Key": retry.current.key },
        body,
      });
      onRecorded();
      setHistory(await gatewayRequest<History>(endpoint));
      retry.current = undefined;
      form.reset();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="space-y-4 p-4 sm:p-5">
      <div>
        <h2 className="font-display text-xl font-semibold">Supplier settlements</h2>
        <p className="mt-1 text-sm text-muted">Record an amount from a supplier statement and keep each correction in the request history.</p>
      </div>
      <form className="flex flex-wrap items-end gap-3" onSubmit={openHistory}>
        <label className="w-full max-w-xs space-y-1 text-sm font-medium">Request ID
          <Input name="request_id" placeholder="req_…" maxLength={64} required disabled={busy} />
        </label>
        <Button type="submit" variant="outline" disabled={busy}>Open settlement history</Button>
      </form>
      {history && <>
        <p className="break-words text-sm text-muted">
          <code>{history.request_id}</code> · {providerLabel(history.provider)} · {formatNanoUsd(history.estimated_usd_nanos)} estimated
        </p>
        {history.eligible ? <form className="flex flex-wrap items-end gap-3" onSubmit={record}>
          <label className="w-full max-w-44 space-y-1 text-sm font-medium">Supplier amount (USD)
            <Input name="amount" inputMode="decimal" placeholder="0.012345678" required maxLength={20} disabled={busy} />
          </label>
          <label className="w-full max-w-xs space-y-1 text-sm font-medium">Evidence reference
            <Input name="evidence" autoComplete="off" placeholder="statement:line-42" required maxLength={64} disabled={busy} />
          </label>
          <Button type="submit" disabled={busy}>{latest ? "Record correction" : "Record supplier charge"}</Button>
        </form> : <p className="text-sm text-muted">Supplier settlements require a dispatched request with a recorded provider outcome.</p>}
        {latest && <ol aria-label="Settlement revisions" className="divide-y divide-border">
          {history.entries.map((entry) => (
            <li key={entry.revision} className="flex flex-wrap justify-between gap-2 py-2 text-sm">
              <span className="min-w-0 break-all">Revision {entry.revision} · <code>{entry.evidence_ref}</code></span>
              <span className="font-mono">{formatNanoUsd(entry.amount_usd_nanos)}</span>
            </li>
          ))}
        </ol>}
      </>}
      {error && <p className="text-sm text-rose-700 dark:text-rose-300" role="alert">{error}</p>}
    </Card>
  );
}

function parseUsd(value: string) {
  const match = /^(\d{1,10})(?:\.(\d{1,9}))?$/.exec(value);
  if (!match) return null;
  const nanos = BigInt(match[1] + (match[2] ?? "").padEnd(9, "0"));
  return nanos <= 9_223_372_036_854_775_807n ? nanos.toString() : null;
}
