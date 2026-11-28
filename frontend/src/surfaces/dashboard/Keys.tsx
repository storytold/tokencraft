import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { Check, Copy, Plus, Trash2 } from "lucide-react";
import { Button, Card, Input, Label } from "@/components/ui";
import { UserAvatar } from "@/components/UserAvatar";
import { api, ApiError, isDefiniteClientRejection, type ApiAccountUser, type ApiKeyItem } from "@/lib/api";
import { useAuth } from "@/lib/auth";
import { useOrg } from "@/lib/org";

const keyRequestTimeoutMs = 15_000;

/**
 * API keys, scoped to the org in the header switcher. Any member may create and delete
 * (auth.md §4, revised 2026-07-21). A key's full secret is shown exactly once, at creation —
 * afterwards only the identifying prefix survives.
 */
export default function Keys() {
  const { user } = useAuth();
  const { current, loading } = useOrg();
  if (!user || loading || !current) return null;
  return <AccountKeys key={`${user.id}/${current.account_id}`} accountId={current.account_id} />;
}

function AccountKeys({ accountId }: { accountId: string }) {
  const formId = useId();
  const [keys, setKeys] = useState<ApiKeyItem[] | null>(null);
  const [creators, setCreators] = useState<Record<string, ApiAccountUser>>({});
  const [error, setError] = useState<string | null>(null);
  const [readError, setReadError] = useState("");
  const [revision, setRevision] = useState(0);

  // One-time secret from the most recent creation (cleared on navigation/reload — by design).
  const [minted, setMinted] = useState<{ title: string; secret: string } | null>(null);

  const [showForm, setShowForm] = useState(false);
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [pendingWrite, setPendingWrite] = useState<"create" | "delete" | null>(null);
  const activeWrite = useRef<AbortController | null>(null);

  useEffect(() => () => activeWrite.current?.abort(), []);

  function refresh() {
    setReadError("");
    setRevision((version) => version + 1);
  }

  useEffect(() => {
    const controller = new AbortController();
    const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(keyRequestTimeoutMs)]);
    api.listApiKeys(accountId, signal).then(({ api_keys }) => {
      if (signal.aborted) return;
      setKeys(api_keys);
      const ids = [...new Set(api_keys.map((key) => key.created_by_user_id))];
      if (ids.length === 0) return;
      // Creator profiles are optional and resolve after the key list.
      api.accountUsers(accountId, ids, signal).then(({ users }) => {
        if (!signal.aborted) setCreators(Object.fromEntries(users.map((user) => [user.user_id, user])));
      }).catch(() => {});
    }).catch((cause) => {
      if (!controller.signal.aborted) {
        setReadError(cause instanceof ApiError ? cause.message : "Couldn’t load API keys.");
      }
    });
    return () => controller.abort();
  }, [accountId, revision]);

  async function changeKey(operation: "create" | "delete", execute: (signal: AbortSignal) => Promise<unknown>) {
    if (activeWrite.current) return;
    const controller = new AbortController();
    const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(keyRequestTimeoutMs)]);
    activeWrite.current = controller;
    setPendingWrite(operation);
    setError(null);
    try {
      await execute(signal);
      refresh();
    } catch (cause) {
      if (controller.signal.aborted) return;
      if (isDefiniteClientRejection(cause, signal)) {
        setError(cause.message);
        return;
      }
      const result = operation === "create" ? "created" : "deleted";
      setError(`Couldn’t confirm the key was ${result}. Check the key list before trying again.`);
      refresh();
    } finally {
      activeWrite.current = null;
      setPendingWrite(null);
    }
  }

  function create(event: FormEvent) {
    event.preventDefault();
    if (minted || !title.trim()) return;
    void changeKey("create", async (signal) => {
      const { api_key, secret } = await api.createApiKey(accountId, title.trim(), description.trim() || undefined, signal);
      if (!api_key || typeof secret !== "string" || !secret) throw new Error("Incomplete key issuance response");
      setMinted({ title: api_key.title, secret });
      setTitle("");
      setDescription("");
      setShowForm(false);
    });
  }

  function remove(key: ApiKeyItem) {
    if (activeWrite.current || !window.confirm(`Delete "${key.title}"? Anything using it stops working immediately.`)) return;
    void changeKey("delete", (signal) => api.deleteApiKey(accountId, key.id, signal));
  }

  const writing = pendingWrite !== null;

  return (
    <div className="space-y-6">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h1 className="font-display text-3xl font-bold tracking-tight">API keys</h1>
          <p className="mt-1 text-muted">One secret authenticates every endpoint. Rotate anytime.</p>
        </div>
        <Button disabled={writing || Boolean(minted)} onClick={() => setShowForm((v) => !v)}>
          <Plus className="h-4 w-4" /> Create key
        </Button>
      </div>

      <Card className="flex items-start gap-3 border-amber-500/30 bg-amber-500/[0.06] p-4">
        <p className="text-sm text-muted">
          Treat keys like passwords. Keys look like{" "}
          <code className="rounded bg-surface2 px-1.5 py-0.5">foundation_api_…</code> and are shown in full
          only once, at creation — copy them somewhere safe.
        </p>
      </Card>

      {showForm && (
        <Card className="p-5">
          <form onSubmit={create}>
            <fieldset className="min-w-0 space-y-4" disabled={writing}>
              <div className="grid gap-4 sm:grid-cols-2">
                <div>
                  <Label htmlFor={`${formId}-name`}>Name</Label>
                  <Input
                    id={`${formId}-name`}
                    placeholder="Production backend"
                    value={title}
                    onChange={(e) => setTitle(e.target.value)}
                    required
                    autoFocus
                  />
                </div>
                <div>
                  <Label htmlFor={`${formId}-description`}>Description (optional)</Label>
                  <Input
                    id={`${formId}-description`}
                    placeholder="What uses this key?"
                    value={description}
                    onChange={(e) => setDescription(e.target.value)}
                  />
                </div>
              </div>
              <div className="flex gap-2">
                <Button type="submit" disabled={!title.trim()}>
                  {pendingWrite === "create" ? "Creating…" : "Create key"}
                </Button>
                <Button type="button" variant="ghost" onClick={() => setShowForm(false)}>
                  Cancel
                </Button>
              </div>
            </fieldset>
          </form>
        </Card>
      )}

      {minted && <MintedSecret title={minted.title} secret={minted.secret} onDismiss={() => setMinted(null)} />}

      {error && <p role="alert" className="rounded-lg bg-rose-500/10 px-3 py-2 text-sm text-rose-500">{error}</p>}
      {readError && <p role="alert" className="rounded-lg bg-rose-500/10 px-3 py-2 text-sm text-rose-500">
        {readError}{" "}<button type="button" className="underline" onClick={refresh}>Refresh keys</button>
      </p>}

      <Card className="overflow-hidden">
        <div className="scrollbar-thin overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="border-b border-border text-left text-muted">
              <tr>
                <th className="px-5 py-3 font-medium">Name</th>
                <th className="px-5 py-3 font-medium">Key</th>
                <th className="px-5 py-3 font-medium">Created by</th>
                <th className="px-5 py-3 font-medium">Created</th>
                <th className="px-5 py-3 font-medium">Last used</th>
                <th className="px-5 py-3"></th>
              </tr>
            </thead>
            <tbody>
              {keys === null ? (
                <tr>
                  <td colSpan={6} className="px-5 py-4 text-muted">
                    {readError ? "API keys unavailable." : "Loading…"}
                  </td>
                </tr>
              ) : keys.length === 0 ? (
                <tr>
                  <td colSpan={6} className="px-5 py-4 text-muted">
                    No API keys yet — create one to start calling the API.
                  </td>
                </tr>
              ) : (
                keys.map((k) => {
                  const creator = creators[k.created_by_user_id];
                  return (
                    <tr key={k.id} className="border-b border-border/60 last:border-0">
                      <td className="px-5 py-3">
                        <div className="font-medium text-fg">{k.title}</div>
                        {k.description && <div className="text-xs text-muted">{k.description}</div>}
                      </td>
                      <td className="px-5 py-3">
                        <code className="font-mono text-[13px] text-muted">{k.key_prefix}••••••••</code>
                      </td>
                      <td className="px-5 py-3">
                        {creator ? (
                          <div className="flex items-center gap-2">
                            <UserAvatar user={creator} className="h-6 w-6 shrink-0 text-[10px]" />
                            <span className="text-muted">{creator.display_name ?? creator.email}</span>
                          </div>
                        ) : (
                          <span className="text-xs text-muted">—</span>
                        )}
                      </td>
                      <td className="px-5 py-3 text-muted">{new Date(k.created_at).toLocaleDateString()}</td>
                      {/* Last used: date AND time. */}
                      <td className="px-5 py-3 text-muted">
                        {k.last_used_at ? new Date(k.last_used_at).toLocaleString() : "never"}
                      </td>
                      <td className="px-5 py-3 text-right">
                        <button
                          onClick={() => remove(k)}
                          disabled={writing}
                          title="Delete key"
                          className="text-muted transition hover:text-rose-500 disabled:opacity-50"
                        >
                          <Trash2 className="h-4 w-4" />
                        </button>
                      </td>
                    </tr>
                  );
                })
              )}
            </tbody>
          </table>
        </div>
      </Card>
    </div>
  );
}

/** The one-time secret reveal — visible until dismissed or the page changes. */
function MintedSecret({ title, secret, onDismiss }: { title: string; secret: string; onDismiss: () => void }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      // clipboard unavailable — the user can still select the text
    }
  };

  return (
    <Card className="border-emerald-500/30 bg-emerald-500/[0.06] p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="font-medium text-fg">"{title}" created — copy the key now.</p>
          <p className="mt-0.5 text-xs text-muted">
            This is the only time it will be shown. If you lose it, delete the key and create a new one.
          </p>
          <code className="mt-3 block select-all break-all rounded-lg bg-surface2 px-3 py-2 font-mono text-[13px] text-fg">
            {secret}
          </code>
        </div>
        <div className="flex shrink-0 flex-col gap-2">
          <Button size="sm" onClick={() => void copy()}>
            {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
            {copied ? "Copied" : "Copy"}
          </Button>
          <Button size="sm" variant="ghost" onClick={onDismiss}>
            Done
          </Button>
        </div>
      </div>
    </Card>
  );
}
