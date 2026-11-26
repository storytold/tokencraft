import { lazy, Suspense } from "react";
import { Routes, Route, Navigate, useLocation, useParams } from "react-router-dom";
import { ThemeProvider } from "@/lib/theme";
import { AuthProvider, useAuth } from "@/lib/auth";
import { RequireAuth } from "@/components/RequireAuth";
import { GuestOnly } from "@/components/GuestOnly";

import MarketingLayout from "@/surfaces/marketing/MarketingLayout";
import Landing from "@/surfaces/marketing/Landing";
import AuthLayout from "@/surfaces/auth/AuthLayout";
import { demoEnabled } from "@/lib/gateway";
import { legalDraftsEnabled } from "@/lib/legalDrafts";
import { connectPath } from "@/lib/onboarding";

const PlaygroundPage = lazy(() => import("@/surfaces/playground/PlaygroundPage"));
const RoutingComparison = lazy(() => import("@/surfaces/comparison/ComparisonPage"));
const CompareModelsPage = lazy(() => import("@/surfaces/models/CompareModelsPage"));
const LegalDraft = legalDraftsEnabled ? lazy(() => import("@/surfaces/marketing/LegalDraft")) : null;
const Docs = lazy(() => import("@/surfaces/marketing/Docs"));
const Pricing = lazy(() => import("@/surfaces/marketing/Pricing"));
const ApiReference = lazy(() => import("@/surfaces/marketing/ApiReference"));
const GenerationReference = lazy(() => import("@/surfaces/marketing/GenerationReference"));
const Catalog = lazy(() => import("@/surfaces/models/Catalog"));
const ModelPage = lazy(() => import("@/surfaces/models/ModelPage"));
const ExamplePage = lazy(() => import("@/surfaces/models/ExamplePage"));
const RankingsOverview = lazy(() => import("@/surfaces/rankings/Overview"));
const MediaRankings = lazy(() => import("@/surfaces/rankings/MediaRankings"));
const RankingModel = lazy(() => import("@/surfaces/rankings/RankingModel"));

const Login = lazy(() => import("@/surfaces/auth/Login"));
const Signup = lazy(() => import("@/surfaces/auth/Signup"));
const Connect = lazy(() => import("@/surfaces/auth/Connect"));
const ConnectAgents = lazy(() => import("@/surfaces/auth/ConnectAgents"));
const GithubCallback = lazy(() => import("@/surfaces/auth/GithubCallback"));
const Join = lazy(() => import("@/surfaces/auth/Join"));
const Reset = lazy(() => import("@/surfaces/auth/Reset"));
const ResetConfirm = lazy(() => import("@/surfaces/auth/ResetConfirm"));
const VerifyEmail = lazy(() => import("@/surfaces/auth/VerifyEmail"));

const DashboardLayout = lazy(() => import("@/surfaces/dashboard/DashboardLayout"));
const Overview = lazy(() => import("@/surfaces/dashboard/Overview"));
const Requests = lazy(() => import("@/surfaces/dashboard/Requests"));
const GatewayRequestsLayout = lazy(() => import("@/surfaces/dashboard/GatewayRequestsLayout"));
const GatewayRequests = lazy(() => import("@/surfaces/dashboard/GatewayRequests"));
const GatewaySetup = lazy(() => import("@/surfaces/dashboard/GatewaySetup"));
const Providers = lazy(() => import("@/surfaces/dashboard/Providers"));
const Projects = lazy(() => import("@/surfaces/projects/Projects"));
const Workflows = lazy(() => import("@/surfaces/workflows/WorkflowsPage"));
const Usage = lazy(() => import("@/surfaces/dashboard/Usage"));
const Exports = lazy(() => import("@/surfaces/dashboard/Exports"));
const Billing = lazy(() => import("@/surfaces/dashboard/Billing"));
const Keys = lazy(() => import("@/surfaces/dashboard/Keys"));
const Organization = lazy(() => import("@/surfaces/dashboard/Organization"));
const AuditLogPage = lazy(() => import("@/surfaces/dashboard/AuditLog"));
const Settings = lazy(() => import("@/surfaces/dashboard/Settings"));
const Admin = lazy(() => import("@/surfaces/admin/Admin"));

function CompareRoute() {
  const { search } = useLocation();
  const params = new URLSearchParams(search);
  if (demoEnabled && !params.has("models") && (params.has("request_id") || params.has("webhook_subscription_id"))) {
    return <Navigate replace to={"/routing" + search} />;
  }
  return <CompareModelsPage />;
}

function ProjectWorkspaceLayout() {
  const { user, loading } = useAuth();
  if (loading) return <p className="p-8 text-muted" role="status">Loading…</p>;
  return user ? (
    <ThemeProvider surface="dashboard" defaultMode="dark"><DashboardLayout accountInQuery /></ThemeProvider>
  ) : (
    <ThemeProvider surface="marketing"><MarketingLayout /></ThemeProvider>
  );
}

function LegacyBusinessRoute() {
  const { accountId } = useParams();
  const { search } = useLocation();
  const scope = accountId ? `/dashboard/${encodeURIComponent(accountId)}` : "/dashboard";
  return <Navigate to={{ pathname: `${scope}/organization`, search, hash: "#general" }} replace />;
}

function LegacyExportsRoute() {
  const { pathname, search, hash } = useLocation();
  return <Navigate replace to={{ pathname: pathname.replace(/\/exports\/?$/i, "/requests/exports"), search, hash }} />;
}

export default function App() {
  return (
    <AuthProvider>
    <Suspense fallback={<p className="p-8 text-muted" role="status">Loading…</p>}>
    <Routes>
      {/* ── Marketing + catalog (public) ── */}
      <Route
        element={
          <ThemeProvider surface="marketing">
            <MarketingLayout />
          </ThemeProvider>
        }
      >
        <Route path="/" element={<Landing />} />
        <Route path="/compare" element={<CompareRoute />} />
        {LegalDraft && (
          <>
            <Route path="/terms" element={<LegalDraft kind="terms" />} />
            <Route path="/privacy" element={<LegalDraft kind="privacy" />} />
          </>
        )}
        <Route path="/docs" element={<Docs />} />
        <Route path="/pricing" element={<Pricing />} />
        <Route path="/docs/api" element={<ApiReference />} />
        <Route path="/docs/api/:method/*" element={<ApiReference />} />
        <Route path="/docs/primitives/:primitive" element={<GenerationReference />} />
        <Route path="/docs/models/:model" element={<GenerationReference />} />
        <Route path="/models" element={<Catalog />} />
        <Route path="/models/:slug" element={<ModelPage />} />
        <Route path="/models/:slug/gallery/:exampleId" element={<ExamplePage />} />
        <Route path="/rankings" element={<RankingsOverview />} />
        <Route path="/rankings/models/:slug" element={<RankingModel />} />
        <Route path="/rankings/:media" element={<MediaRankings />} />
      </Route>

      {demoEnabled && <Route element={<ProjectWorkspaceLayout />}>
        <Route path="/playground" element={<PlaygroundPage />} />
        <Route element={<RequireAuth />}>
          <Route path="/routing" element={<RoutingComparison />} />
        </Route>
      </Route>}

      {/* ── Auth ── */}
      <Route
        element={
          <ThemeProvider surface="auth">
            <AuthLayout />
          </ThemeProvider>
        }
      >
        {/* Sessionless pages: a logged-in user is redirected away (to ?redirect_to when
            it's a safe same-origin path, else the dashboard). */}
        <Route element={<GuestOnly />}>
          <Route path="/login" element={<Login />} />
          <Route path="/signup" element={<Signup />} />
          <Route path="/reset" element={<Reset />} />
        </Route>
        <Route element={<RequireAuth />}>
          <Route path={connectPath} element={<Connect />} />
          <Route path="/connect/agents" element={<ConnectAgents />} />
        </Route>
        {/* NOT guest-only: /join branches on session itself, and the emailed token pages
            must work regardless of login state. */}
        {/* Invite links land here with ?token=… (or ?code=…); bare /join offers code entry */}
        <Route path="/join" element={<Join />} />
        {/* The emailed reset link lands here with ?token=… */}
        <Route path="/reset-password" element={<ResetConfirm />} />
        {/* The emailed verification link lands here with ?token=… */}
        <Route path="/verify-email" element={<VerifyEmail />} />
        {/* GitHub redirects back here with ?code&state (not guest-only: mid-flow state). */}
        <Route path="/auth/github/callback" element={<GithubCallback />} />
      </Route>

      {/* ── Dashboard (requires login) ── */}
      <Route element={<RequireAuth />}>
        <Route
          element={
            <ThemeProvider surface="dashboard" defaultMode="dark">
              <DashboardLayout />
            </ThemeProvider>
          }
        >
          {/* OrgProvider canonicalizes the optional account segment. The project console
              keeps it even for a single organization so recipients can resolve shared URLs. */}
          <Route path="/dashboard/:accountId?" element={demoEnabled ? <Navigate to="usage" replace /> : <Overview />} />
          {demoEnabled ? (
            <Route path="/dashboard/:accountId?/requests" element={<GatewayRequestsLayout />}>
              <Route index element={<GatewayRequests />} />
              <Route path="exports" element={<Exports />} />
            </Route>
          ) : <Route path="/dashboard/:accountId?/requests" element={<Requests />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/requests/generation" element={<Requests />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/usage" element={<Usage />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/exports" element={<LegacyExportsRoute />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/projects" element={<Projects />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/workflows" element={<Workflows />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/providers" element={<Providers />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/providers/operations" element={<Providers operations />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/projects/keys" element={<Projects view="keys" />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/projects/providers" element={<Projects view="providers" />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/projects/providers/operations" element={<Providers operations />} />}
          {demoEnabled && <Route path="/dashboard/:accountId?/setup" element={<GatewaySetup />} />}
          <Route path="/dashboard/:accountId?/billing" element={demoEnabled ? <Usage /> : <Billing />} />
          <Route path="/dashboard/:accountId?/keys" element={<Keys />} />
          <Route path="/dashboard/:accountId?/organization" element={<Organization />} />
          {demoEnabled && <Route path="/dashboard/:accountId?/organization/routing-policy" element={<Organization routing />} />}
          <Route path="/dashboard/:accountId?/business" element={<LegacyBusinessRoute />} />
          <Route path="/dashboard/:accountId?/audit" element={<AuditLogPage />} />
          {/* User-scoped (never carries an account id). */}
          <Route path="/dashboard/user/settings" element={<Settings />} />
          <Route path="/dashboard/settings" element={<Navigate to="/dashboard/user/settings" replace />} />
        </Route>
      </Route>

      {/* ── Admin (placeholder) ── */}
      <Route
        path="/admin"
        element={
          <ThemeProvider surface="admin" defaultMode="dark">
            <Admin />
          </ThemeProvider>
        }
      />

      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
    </Suspense>
    </AuthProvider>
  );
}
