import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Link, NavLink, Outlet, useLocation, useNavigate, useNavigationType } from "react-router-dom";
import { Github, LayoutDashboard, LogOut, Settings } from "lucide-react";
import { Container, Logo, ThemeToggle } from "@/components/ui";
import { UserAvatar } from "@/components/UserAvatar";
import { useAuth } from "@/lib/auth";
import { cn } from "@/lib/cn";
import { demoEnabled } from "@/lib/gateway";
import { connectPath, entryPath } from "@/lib/onboarding";

const NAV = [
  { to: "/models", label: "Models" },
  ...(demoEnabled ? [{ to: "/playground", label: "Playground" }] : [{ to: "/rankings", label: "Rankings" }]),
  { to: "/compare", label: "Compare" },
  { to: "/docs", label: "Docs" },
];

export default function MarketingLayout() {
  const { user } = useAuth();
  const navigation = user ? [...NAV, { to: "/dashboard", label: "Dashboard" }] : NAV;
  const { pathname, hash } = useLocation();
  const navigationType = useNavigationType();
  const previousPath = useRef(pathname);

  useLayoutEffect(() => {
    const leftLanding = previousPath.current === "/" && pathname !== "/";
    previousPath.current = pathname;
    if (leftLanding && navigationType === "PUSH" && !hash) {
      window.scrollTo({ top: 0, behavior: "instant" });
    }
  }, [hash, navigationType, pathname]);

  return (
    <div className="min-h-screen bg-bg text-fg">
      <header className="border-b border-border bg-bg">
        <Container className="flex flex-wrap items-center gap-x-5 md:flex-nowrap">
          <Logo className="my-3 shrink-0" />
          <nav aria-label="Main navigation" className="order-last flex w-full flex-wrap border-t border-border md:order-none md:w-auto md:flex-1 md:border-l md:border-t-0">
            {navigation.map((n) => (
              <NavLink
                key={n.to}
                to={n.to}
                className={({ isActive }) => cn(
                  "border-b-2 px-1.5 py-3 font-mono text-xs font-medium uppercase tracking-wide hover:bg-surface2 sm:px-2.5 md:px-3 md:py-5",
                  isActive ? "border-brand text-fg" : "border-transparent text-muted hover:text-fg",
                )}
              >
                {n.label}
              </NavLink>
            ))}
          </nav>
          <div className="ml-auto flex items-center gap-2">
            <ThemeToggle />
            <HeaderAuth />
          </div>
        </Container>
      </header>

      <main>
        <Outlet />
      </main>

      <footer className="border-t border-border bg-bg">
        <Container className="flex flex-wrap items-center justify-between gap-5 py-7">
          <Logo />
          <nav aria-label="Footer navigation" className="flex flex-wrap gap-x-5 gap-y-2 font-mono text-xs font-medium uppercase text-muted">
            <Link className="hover:text-fg" to="/models">Model catalog</Link>
            <Link className="hover:text-fg" to="/docs">API docs</Link>
            <a className="flex items-center gap-1.5 hover:text-fg" href="https://github.com/storytold/tokencraft"><Github className="h-4 w-4" aria-hidden /> GitHub</a>
          </nav>
        </Container>
      </footer>
    </div>
  );
}

/**
 * The header's auth corner. Guests get Sign in; a signed-in user gets their
 * avatar with a context menu (info, Dashboard, User Settings, Sign out). Renders nothing
 * during the initial /me probe so "Sign in" never flashes for someone who has a session.
 */
function HeaderAuth() {
  const { user, loading } = useAuth();
  if (loading) return <div className="h-8 w-8" aria-hidden />;
  if (!user) {
    return (
      <>
        <Link to={entryPath("/login", connectPath)} className="px-2 py-2 font-mono text-xs font-semibold uppercase hover:text-brand sm:px-3">Sign in</Link>
      </>
    );
  }
  return <UserMenu />;
}

function UserMenu() {
  const { user, logout } = useAuth();
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  if (!user) return null;

  const handleLogout = async () => {
    setOpen(false);
    await logout();
    navigate("/", { replace: true });
  };

  const itemClass =
    "flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm font-medium text-muted transition hover:bg-surface2 hover:text-fg";

  return (
    <div ref={rootRef} className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="menu"
        aria-expanded={open}
        className="rounded-full transition hover:opacity-85 focus:outline-none focus:ring-2 focus:ring-brand/50"
      >
        <UserAvatar user={user} className="h-9 w-9 text-sm" />
      </button>
      {open && (
        <div
          role="menu"
          className="absolute right-0 z-50 mt-2 w-64 rounded-xl border border-border bg-surface p-1.5 shadow-xl"
        >
          <div className="px-3 py-2">
            <div className="truncate text-sm font-medium text-fg">{user.display_name ?? "—"}</div>
            <div className="truncate text-xs text-muted">{user.email}</div>
          </div>
          <div className="my-1 border-t border-border/60" />
          <Link to="/dashboard" role="menuitem" className={itemClass} onClick={() => setOpen(false)}>
            <LayoutDashboard className="h-4 w-4" />
            Dashboard
          </Link>
          <Link
            to="/dashboard/user/settings"
            role="menuitem"
            className={itemClass}
            onClick={() => setOpen(false)}
          >
            <Settings className="h-4 w-4" />
            User Settings
          </Link>
          <div className="my-1 border-t border-border/60" />
          <button role="menuitem" onClick={handleLogout} className={itemClass}>
            <LogOut className="h-4 w-4" />
            Sign out
          </button>
        </div>
      )}
    </div>
  );
}
