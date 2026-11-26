import { Link, NavLink, Outlet, useLocation, useMatch, useNavigate } from "react-router-dom";
import {
  BarChart3,
  Building2,
  CreditCard,
  Folder,
  KeyRound,
  LayoutDashboard,
  ListTree,
  LogOut,
  Plug,
  ScrollText,
  Settings as SettingsIcon,
  Sparkles,
  Wallet,
  Workflow,
} from "lucide-react";
import { ApiStatusBanner } from "@/components/ApiStatusBanner";
import { Badge, Logo, ThemeToggle } from "@/components/ui";
import { UserAvatar } from "@/components/UserAvatar";
import { accountDisplayName } from "@/lib/api";
import { BALANCE } from "@/lib/mock-api";
import { useAuth } from "@/lib/auth";
import { OrgProvider, useOrg } from "@/lib/org";
import { cn } from "@/lib/cn";
import { demoEnabled, withProject } from "@/lib/gateway";
import { ProjectPicker, ProjectProvider, useProject } from "@/surfaces/projects/ProjectScope";

const projectPages = new Set(["/usage", "/requests", "/projects", "/workflows", "/billing"]);

/** Account-scoped pages — hrefs are built via `withScope` so multi-org users keep the
 * `/dashboard/{account_id}/…` prefix as they navigate. User Settings is user-scoped and
 * rendered separately with a fixed URL. */
const NAV = [
  demoEnabled ? { rest: "/usage", label: "Usage", icon: BarChart3 } : { rest: "", label: "Overview", icon: LayoutDashboard, end: true },
  { rest: "/requests", label: "Requests", icon: ListTree },
  ...(demoEnabled ? [
    { rest: "/projects", label: "Projects", icon: Folder },
    { rest: "/workflows", label: "Workflows", icon: Workflow },
    { rest: "/providers", label: "Providers", icon: Plug },
  ] : []),
  { rest: "/keys", label: "API keys", icon: KeyRound },
  ...(demoEnabled ? [] : [{ rest: "/billing", label: "Billing", icon: CreditCard }]),
  { rest: "/organization", label: "Organization", icon: Building2 },
  { rest: "/audit", label: "Audit log", icon: ScrollText },
];

/** Scope switcher for users who belong to multiple organizations. */
function OrgSwitcher() {
  const { accounts, current, select } = useOrg();
  if (accounts.length === 0) return null;
  if (current && accounts.length === 1) {
    return <span className="truncate font-display font-semibold">{accountDisplayName(current)}</span>;
  }
  return (
    <select
      aria-label="Organization"
      value={current?.account_id ?? ""}
      onChange={(e) => select(e.target.value)}
      className="h-9 min-w-0 max-w-full rounded-lg border border-border bg-surface px-2 font-display text-sm font-semibold text-fg outline-none focus:border-brand"
    >
      {!current && <option value="" disabled>Choose an organization</option>}
      {accounts.map((a) => (
        <option key={a.account_id} value={a.account_id}>
          {accountDisplayName(a)} ({a.role})
        </option>
      ))}
    </select>
  );
}

export default function DashboardLayout({ accountInQuery = false }: { accountInQuery?: boolean }) {
  return (
    <OrgProvider explicitScope={demoEnabled} accountInQuery={accountInQuery}>
      <DashboardProjectScope accountInQuery={accountInQuery} />
    </OrgProvider>
  );
}

function DashboardProjectScope({ accountInQuery }: { accountInQuery: boolean }) {
  const { current } = useOrg();
  return (
    <ProjectProvider accountId={demoEnabled ? current?.account_id : undefined} includeAccountInUrl={accountInQuery}>
      <DashboardShell />
    </ProjectProvider>
  );
}

const navLinkClass = ({ isActive }: { isActive: boolean }) =>
  cn(
    "flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition",
    isActive ? "bg-brand/12 text-brand" : "text-muted hover:bg-surface2 hover:text-fg",
  );

function DashboardShell() {
  const { user, logout } = useAuth();
  const { current, withScope } = useOrg();
  const { project } = useProject();
  const { pathname, search, hash } = useLocation();
  const navigate = useNavigate();
  const playground = useMatch("/playground") ? pathname + search + hash : withProject("/playground", project?.id, current?.account_id);
  const page = "/" + pathname.replace(/^\/dashboard(?:\/acct_[A-Za-z0-9]+)?\/?/i, "").split("/")[0].toLowerCase();
  const showProjectPicker = demoEnabled && projectPages.has(page);

  // RequireAuth guarantees a user before this renders; the guard is just for type-safety.
  if (!user) return null;

  const handleLogout = async () => {
    await logout();
    navigate("/", { replace: true });
  };

  return (
    <div className="min-h-screen bg-bg text-fg">
      <div className="flex">
        {/* ── Sidebar ── */}
        <aside className="sticky top-0 hidden h-screen w-64 shrink-0 flex-col border-r border-border bg-surface/40 lg:flex">
          <div className="px-5 py-5">
            <Logo />
          </div>
          {showProjectPicker && <div className="px-5 pb-4"><ProjectPicker /></div>}
          <nav aria-label="Workspace sidebar" className="min-h-0 flex-1 space-y-1 overflow-y-auto px-3">
            <WorkspaceLinks />
            <NavLink to="/dashboard/user/settings" className={navLinkClass}>
              <SettingsIcon className="h-4 w-4" />
              User Settings
            </NavLink>
            <NavLink
              to={demoEnabled ? playground : "/models"}
              end
              className={(state) => cn("mt-2", navLinkClass(state))}
            >
              <Sparkles className="h-4 w-4" aria-hidden />
              Playground
            </NavLink>
          </nav>

          {/* Balance card */}
          {!demoEnabled && <div className="m-3 rounded-xl border border-border bg-surface p-4">
            <div className="flex items-center gap-2 text-xs text-muted">
              <Wallet className="h-3.5 w-3.5" /> Credits
            </div>
            <div className="mt-1 font-display text-xl font-bold">${BALANCE.credits.toFixed(2)}</div>
            <Link to={withScope("/billing")} className="mt-2 block text-xs font-medium text-brand hover:underline">
              Add credits →
            </Link>
          </div>}
        </aside>

        {/* ── Main ── */}
        <div className="flex min-w-0 flex-1 flex-col">
          <ApiStatusBanner />
          <header className="sticky top-0 z-30 flex h-16 items-center justify-between border-b border-border bg-bg/80 px-5 backdrop-blur sm:px-8">
            <div className="flex min-w-0 items-center gap-3">
              <OrgSwitcher />
              {!user.email_verified && (
                <Link
                  to="/dashboard/user/settings#email-verification"
                  className="hidden shrink-0 rounded-sm focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand md:inline-block"
                >
                  <Badge tone="amber" className="hover:underline">Email unverified</Badge>
                </Link>
              )}
            </div>
            <div className="flex shrink-0 items-center gap-3">
              <ThemeToggle />
              <Link to="/dashboard/user/settings" className="flex items-center gap-2">
                <span className="hidden text-right sm:block">
                  <span className="block text-sm font-medium leading-tight">{user.display_name ?? "—"}</span>
                  <span className="block text-xs text-muted">{user.email}</span>
                </span>
                <UserAvatar user={user} className="h-9 w-9 text-sm" />
              </Link>
              <button
                onClick={handleLogout}
                title="Sign out"
                className="rounded-lg p-2 text-muted transition hover:bg-surface2 hover:text-fg"
              >
                <LogOut className="h-4 w-4" />
              </button>
            </div>
          </header>
          <div className="flex items-center border-b border-border lg:hidden">
            <nav aria-label="Workspace navigation" className="flex min-w-0 flex-1 gap-2 overflow-x-auto px-3 py-2 [&>a]:shrink-0">
              <WorkspaceLinks />
              <NavLink to={demoEnabled ? playground : "/models"} end className={navLinkClass}>
                <Sparkles className="h-4 w-4" aria-hidden />
                Playground
              </NavLink>
            </nav>
            {showProjectPicker && <div className="shrink-0 pr-3"><ProjectPicker compact /></div>}
          </div>
          <main className="min-w-0 flex-1 px-5 py-8 sm:px-8">
            <Outlet />
          </main>
        </div>
      </div>
    </div>
  );
}

function WorkspaceLinks() {
  const { withScope } = useOrg();
  const { project } = useProject();
  return NAV.map(({ rest, label, icon: Icon, end }) => (
    <NavLink key={rest} to={withProject(withScope(rest), projectPages.has(rest) ? project?.id : undefined)} end={end} className={navLinkClass}>
      <Icon className="h-4 w-4" aria-hidden />
      {label}
    </NavLink>
  ));
}
