import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { api, ApiError, type ApiAccount } from "./api";
import { useAuth } from "./auth";
import { errorMessage } from "./gateway";

/**
 * Organization scope for the dashboard, driven by the URL.
 *
 * Multi-org users get account-scoped URLs — `/dashboard/{account_id}/requests` — so links
 * are shareable and the switcher just navigates to the same page under another account.
 * Single-org users keep unscoped URLs (`/dashboard/requests`). User-scoped pages
 * (`/dashboard/user/…`) never carry an account id, but the selected scope persists across
 * them.
 *
 * A trivial unencrypted cookie remembers the last-viewed account so `/dashboard` (no account
 * in the URL) redirects a multi-org user back to where they were.
 */

const LAST_ACCOUNT_COOKIE = "foundation_last_account";
const organizationReadTimeoutMs = 15_000;
const emptyAccounts: ApiAccount[] = [];

/** Remember the last-viewed account (plain cookie — a UI hint, not a credential). */
export function rememberLastAccount(accountId: string) {
  document.cookie = `${LAST_ACCOUNT_COOKIE}=${accountId}; path=/; max-age=31536000; samesite=lax`;
}

function readLastAccount(): string | null {
  for (const pair of document.cookie.split(";")) {
    const [k, v] = pair.trim().split("=");
    if (k === LAST_ACCOUNT_COOKIE && v) return v;
  }
  return null;
}

/** The `acct_…` segment of an account-scoped dashboard URL, if present. */
function accountIdInPath(pathname: string): string | null {
  const m = pathname.match(/^\/dashboard\/(acct_[A-Za-z0-9]+)(?:\/|$)/);
  return m ? m[1] : null;
}

/** The page part of an account-scoped dashboard URL ("" for overview, "/requests", …), or
 * null when the location isn't account-scoped (e.g. `/dashboard/user/settings`). */
function scopedRest(pathname: string): string | null {
  if (pathname === "/dashboard" || pathname === "/dashboard/") return "";
  const withId = pathname.match(/^\/dashboard\/acct_[A-Za-z0-9]+(\/.*)?$/);
  if (withId) return withId[1] ?? "";
  const bare = pathname.match(/^\/dashboard(\/.*)$/);
  if (!bare) return null;
  return bare[1].startsWith("/user/") || bare[1] === "/user" ? null : bare[1];
}

type OrgContextValue = {
  accounts: ApiAccount[];
  /** The currently-selected organization (null while loading / if none). */
  current: ApiAccount | null;
  /** Switch scope: updates the URL (staying on the same page) and the cookie. */
  select: (accountId: string) => void;
  /** Build a dashboard href under the current scope, e.g. `withScope("/requests")` →
   * `/dashboard/acct_…/requests` for multi-org users, `/dashboard/requests` otherwise. */
  withScope: (rest: string) => string;
  reload: () => Promise<ApiAccount[] | undefined>;
  loading: boolean;
  error?: string;
};

const OrgCtx = createContext<OrgContextValue>({
  accounts: [],
  current: null,
  select: () => {},
  withScope: (rest) => `/dashboard${rest}`,
  reload: async () => undefined,
  loading: true,
});

type OrgProviderProps = {
  children: ReactNode; explicitScope?: boolean; accountInQuery?: boolean;
};

export function OrgProvider(props: OrgProviderProps) {
  const { user } = useAuth();
  return <OrganizationScope key={user?.id ?? ""} userId={user?.id} {...props} />;
}

function OrganizationScope({ children, userId, explicitScope = false, accountInQuery = false }: OrgProviderProps & { userId?: string }) {
  const location = useLocation();
  const navigate = useNavigate();
  const [loadedAccounts, setAccounts] = useState<ApiAccount[]>();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [error, setError] = useState<string>();
  const pending = useRef<AbortController | null>(null);
  const accounts = loadedAccounts ?? emptyAccounts;
  const loading = Boolean(userId && !loadedAccounts && !error);

  const cancelRead = useCallback(() => {
    pending.current?.abort();
    pending.current = null;
  }, []);

  const reload = useCallback(async () => {
    if (!userId) return;
    cancelRead();
    const controller = new AbortController();
    pending.current = controller;
    const deadline = AbortSignal.timeout(organizationReadTimeoutMs);
    const signal = AbortSignal.any([controller.signal, deadline]);
    setError(undefined);
    try {
      const { accounts } = await api.listAccounts(signal);
      signal.throwIfAborted();
      if (!Array.isArray(accounts)) throw new Error("Invalid organization response");
      setAccounts(accounts);
      return accounts;
    } catch (cause) {
      if (!controller.signal.aborted) {
        if (cause instanceof ApiError && (cause.status === 401 || cause.status === 403)) setAccounts(undefined);
        setError(deadline.aborted ? "Loading organizations took too long." : errorMessage(cause) || "Couldn’t load organizations.");
      }
    } finally {
      if (pending.current === controller) pending.current = null;
    }
  }, [cancelRead, userId]);

  useEffect(() => {
    void reload();
    return cancelRead;
  }, [cancelRead, reload]);

  const urlAccountId = accountInQuery ? new URLSearchParams(location.search).get("account_id") : accountIdInPath(location.pathname);

  // Resolve the scope: the URL wins, then an in-session selection, then the cookie, then the
  // oldest membership (the signup-created account).
  const current = useMemo(() => {
    if (accounts.length === 0) return null;
    const byId = (id: string | null) => (id ? accounts.find((a) => a.account_id === id) : undefined);
    if (accountInQuery && urlAccountId) return byId(urlAccountId) ?? null;
    return byId(urlAccountId) ?? byId(selectedId) ?? byId(readLastAccount()) ?? accounts[0];
  }, [accounts, accountInQuery, urlAccountId, selectedId]);

  const showScope = explicitScope || accounts.length > 1;

  const withScope = useCallback(
    (rest: string) => (showScope && current ? `/dashboard/${current.account_id}${rest}` : `/dashboard${rest}`),
    [showScope, current],
  );

  // Remember the scope whenever it resolves.
  useEffect(() => {
    if (current) rememberLastAccount(current.account_id);
  }, [current]);

  // Canonicalize the URL once memberships are known:
  //   - multi-org + unscoped account page → inject the remembered scope,
  //   - single-org + scoped page → drop the id unless stable account links are enabled,
  //   - an id we're not a member of → back to the scope that matters.
  useEffect(() => {
    if (loading || !current) return;
    const rest = scopedRest(location.pathname);
    if (rest === null) return; // user-scoped page — no account in these URLs
    const canonical = showScope ? `/dashboard/${current.account_id}${rest}` : `/dashboard${rest}`;
    if (location.pathname !== canonical) navigate(canonical + location.search + location.hash, { replace: true });
  }, [loading, current, showScope, location.pathname, location.search, location.hash, navigate]);

  const select = useCallback(
    (accountId: string) => {
      setSelectedId(accountId);
      rememberLastAccount(accountId);
      if (accountInQuery) {
        const params = new URLSearchParams(location.search);
        params.set("account_id", accountId);
        params.delete("project_id");
        params.delete("request_id");
        params.delete("webhook_subscription_id");
        navigate({ search: params.toString() });
        return;
      }
      // On an account-scoped page, jump to the same page under the new scope.
      const rest = scopedRest(location.pathname);
      if (rest !== null) navigate(`/dashboard/${accountId}${rest}`);
    },
    [accountInQuery, location.pathname, location.search, navigate],
  );

  return <OrgCtx.Provider value={{ accounts, current, select, withScope, reload, loading, error }}>{children}</OrgCtx.Provider>;
}

export const useOrg = () => useContext(OrgCtx);
