import { useCallback, useEffect, useRef, useState } from "react";
import { Badge, Button, Card, Select } from "@/components/ui";
import { UserAvatar } from "@/components/UserAvatar";
import { api, type ApiAccount, type ApiAccountUser, type ApiMember, type AuditItem, type AuditPage } from "@/lib/api";
import { useAuth } from "@/lib/auth";
import { useOrg } from "@/lib/org";

const auditReadTimeoutMs = 15_000;
type Tab = "mine" | "org" | "members";
type ReadPage = (before: string | undefined, signal: AbortSignal) => Promise<AuditPage>;
type ActorDirectory = {
  infos: Record<string, ApiAccountUser>;
  ensure: (ids: (string | null)[]) => void;
};

const readMyPage: ReadPage = (before, signal) => api.userAuditLogs(before, undefined, signal);

function useUserInfos(accountId?: string): ActorDirectory {
  const [infos, setInfos] = useState<Record<string, ApiAccountUser>>({});
  const attempted = useRef(new Set<string>());
  const pending = useRef(new Set<AbortController>());

  useEffect(() => {
    const requests = pending.current;
    return () => {
      for (const controller of requests) controller.abort();
    };
  }, []);

  const ensure = useCallback((ids: (string | null)[]) => {
    if (!accountId) return;
    const fresh = [...new Set(ids.filter((id): id is string => Boolean(id)))]
      .filter((id) => !attempted.current.has(id))
      .slice(0, 100);
    if (fresh.length === 0) return;
    for (const id of fresh) attempted.current.add(id);

    const controller = new AbortController();
    pending.current.add(controller);
    const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(auditReadTimeoutMs)]);
    // The actor directory enriches loaded rows independently of pagination.
    api.accountUsers(accountId, fresh, signal).then(({ users }) => {
      if (!signal.aborted && users.length > 0) {
        setInfos((previous) => ({ ...previous, ...Object.fromEntries(users.map((user) => [user.user_id, user])) }));
      }
    }).catch(() => {
      for (const id of fresh) attempted.current.delete(id);
    }).finally(() => pending.current.delete(controller));
  }, [accountId]);

  return { infos, ensure };
}

export default function AuditLog() {
  const { user } = useAuth();
  const { current, loading } = useOrg();
  if (loading || !user) return null;
  return <AuditWorkspace key={`${user.id}/${current?.account_id ?? ""}/${current?.role ?? ""}`} account={current} />;
}

function AuditWorkspace({ account }: { account: ApiAccount | null }) {
  const accountId = account?.account_id ?? "";
  const [tab, setTab] = useState<Tab>("mine");
  const actors = useUserInfos(accountId);
  const isOrgAdmin = account?.role === "owner" || account?.role === "admin";
  const readOrgPage = useCallback<ReadPage>((before, signal) =>
    api.accountAuditLogs(accountId, before, undefined, signal), [accountId]);

  const tabs: { key: Tab; label: string }[] = [
    { key: "mine", label: "My Activity" },
    ...(isOrgAdmin ? [
      { key: "org" as Tab, label: "Org Activity" },
      { key: "members" as Tab, label: "Org Member Activity" },
    ] : []),
  ];

  return (
    <div className="max-w-3xl space-y-6">
      <div>
        <h1 className="font-display text-3xl font-bold tracking-tight">Audit log</h1>
        <p className="mt-1 text-muted">
          Your account activity{isOrgAdmin && account ? ` — and ${account.name ?? "your organization"}'s` : ""}.
        </p>
      </div>

      {tabs.length > 1 && (
        <div className="inline-flex rounded-lg border border-border bg-surface p-1" role="group" aria-label="Audit activity">
          {tabs.map((item) => (
            <button
              key={item.key}
              onClick={() => setTab(item.key)}
              type="button"
              aria-pressed={tab === item.key}
              className={
                tab === item.key
                  ? "rounded-md px-3 py-1.5 text-xs font-medium brand-gradient text-white shadow-glow"
                  : "rounded-md px-3 py-1.5 text-xs font-medium text-muted transition hover:text-fg"
              }
            >
              {item.label}
            </button>
          ))}
        </div>
      )}

      {tab === "mine" && (
        <AuditSection
          title="My activity"
          subtitle="Everything on your user — including items only you can see."
          readPage={readMyPage}
          actors={actors}
        />
      )}
      {tab === "org" && isOrgAdmin && (
        <AuditSection
          title="Organization activity"
          subtitle="Membership, invitations, keys, and billing-profile events (admins only)."
          readPage={readOrgPage}
          actors={actors}
        />
      )}
      {tab === "members" && isOrgAdmin && accountId && <MemberAuditSection accountId={accountId} actors={actors} />}
    </div>
  );
}

function AuditSection({ title, subtitle, readPage, actors }: {
  title: string; subtitle: string; readPage: ReadPage; actors: ActorDirectory;
}) {
  return (
    <Card className="overflow-hidden">
      <div className="border-b border-border px-5 py-4">
        <h2 className="font-display font-semibold">{title}</h2>
        <p className="text-xs text-muted">{subtitle}</p>
      </div>
      <AuditList readPage={readPage} actors={actors} />
    </Card>
  );
}

function AuditList({ readPage, actors: { infos, ensure }, empty = "Nothing here yet." }: {
  readPage: ReadPage; actors: ActorDirectory; empty?: string;
}) {
  const [items, setItems] = useState<AuditItem[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const active = useRef<AbortController | null>(null);

  const load = useCallback(async (before?: string) => {
    if (active.current) return;
    const controller = new AbortController();
    active.current = controller;
    const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(auditReadTimeoutMs)]);
    setLoading(true);
    setError("");
    try {
      const page = await readPage(before, signal);
      signal.throwIfAborted();
      setItems((previous) => before ? [...previous, ...page.items] : page.items);
      setCursor(page.next_before);
      ensure(page.items.map((item) => item.actor_user_id));
    } catch {
      if (!controller.signal.aborted) setError("Couldn’t load this log.");
    } finally {
      if (active.current === controller) {
        active.current = null;
        setLoading(false);
      }
    }
  }, [readPage, ensure]);

  useEffect(() => {
    void load();
    return () => {
      active.current?.abort();
      active.current = null;
    };
  }, [load]);

  return (
    <>
      {items.length > 0 && (
        <ul>
          {items.map((item) => (
            <AuditRow key={item.id} item={item} actor={item.actor_user_id ? infos[item.actor_user_id] : undefined} />
          ))}
        </ul>
      )}
      {items.length === 0 && loading && <p role="status" className="px-5 py-4 text-sm text-muted">Loading…</p>}
      {items.length === 0 && !loading && !error && <p className="px-5 py-4 text-sm text-muted">{empty}</p>}
      {error && <p className="px-5 py-4 text-sm text-rose-500" role="alert">{error}</p>}
      {(cursor || error) && (
        <div className="border-t border-border px-5 py-3">
          <Button variant="outline" size="sm" onClick={() => void load(cursor ?? undefined)} disabled={loading}>
            {loading ? "Loading…" : error ? "Retry" : "Load more"}
          </Button>
        </div>
      )}
    </>
  );
}

function MemberAuditSection({ accountId, actors }: { accountId: string; actors: ActorDirectory }) {
  const [members, setMembers] = useState<ApiMember[]>();
  const [rosterError, setRosterError] = useState(false);
  const [revision, setRevision] = useState(0);
  const [userId, setUserId] = useState("");
  const readPage = useCallback<ReadPage>((before, signal) =>
    api.memberAuditLogs(accountId, userId, before, undefined, signal), [accountId, userId]);

  useEffect(() => {
    const controller = new AbortController();
    const signal = AbortSignal.any([controller.signal, AbortSignal.timeout(auditReadTimeoutMs)]);
    api.listMembers(accountId, signal).then(({ members }) => {
      if (!signal.aborted) setMembers(members);
    }).catch(() => {
      if (!controller.signal.aborted) setRosterError(true);
    });
    return () => controller.abort();
  }, [accountId, revision]);

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4">
        <div>
          <h2 className="font-display font-semibold">Member activity</h2>
          <p className="text-xs text-muted">A member's history — organization-visible items only.</p>
        </div>
        <Select aria-label="Organization member" value={userId} onChange={(event) => setUserId(event.target.value)}
          className="w-56" disabled={!members?.length}>
          <option value="">Choose a member…</option>
          {members?.map((member) => (
            <option key={member.user_id} value={member.user_id}>
              {member.display_name ?? member.email}
            </option>
          ))}
        </Select>
      </div>
      {userId && <AuditList key={userId} readPage={readPage} actors={actors} empty="No organization-visible activity." />}
      {!userId && members && (
        <p className="px-5 py-4 text-sm text-muted">
          {members.length ? "Pick a member to see their organization-visible activity." : "No organization members to display."}
        </p>
      )}
      {!members && !rosterError && <p role="status" className="px-5 py-4 text-sm text-muted">Loading members…</p>}
      {rosterError && <div className="space-y-2 px-5 py-4">
        <p role="alert" className="text-sm text-rose-500">Couldn’t load organization members.</p>
        <Button variant="outline" size="sm" onClick={() => {
          setRosterError(false);
          setRevision((value) => value + 1);
        }}>Retry loading members</Button>
      </div>}
    </Card>
  );
}

/* ── One row ── */

function AuditRow({ item, actor }: { item: AuditItem; actor?: ApiAccountUser }) {
  const summary = summarize(item);
  return (
    <li className="flex items-start justify-between gap-3 border-b border-border/60 px-5 py-3 last:border-0">
      <div className="flex min-w-0 items-start gap-3">
        {/* The actor's chosen avatar (upload / gravatar / initials) via the user-info API. */}
        {actor && <UserAvatar user={actor} className="mt-0.5 h-7 w-7 shrink-0 text-[10px]" />}
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium text-fg">{humanizeEvent(item.event)}</span>
            {item.is_org_visible === false && <Badge tone="muted">only you</Badge>}
          </div>
          {actor && (
            <div className="mt-0.5 text-xs text-muted">by {actor.display_name ?? actor.email}</div>
          )}
          {summary && <div className="mt-0.5 truncate text-xs text-muted">{summary}</div>}
        </div>
      </div>
      <div className="shrink-0 text-right text-xs text-muted">
        <div>{new Date(item.created_at).toLocaleString()}</div>
        {item.ip && <div className="font-mono">{item.ip}</div>}
      </div>
    </li>
  );
}

function humanizeEvent(event: string): string {
  return event.replaceAll("_", " ");
}

/** A terse one-line summary from the event's data payload. */
function summarize(item: AuditItem): string | null {
  const d = item.data;
  if (!d) return null;
  if (typeof d.from !== "undefined" || typeof d.to !== "undefined") {
    return `${d.from ?? "—"} → ${d.to ?? "—"}`;
  }
  if (typeof d.old_email === "string" && typeof d.new_email === "string") {
    return `${d.old_email} → ${d.new_email}`;
  }
  const parts = Object.entries(d)
    .filter(([, v]) => typeof v === "string" || typeof v === "number")
    .map(([k, v]) => `${k}: ${v}`);
  return parts.length > 0 ? parts.join(" · ") : null;
}
