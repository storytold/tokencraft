import { gatewayRequest, type ProjectScope } from "@/lib/gateway";

export const grantRoles = {
  developer: "Developer",
  billing_viewer: "Billing viewer",
  client_viewer: "Client viewer",
};
export type GrantRole = keyof typeof grantRoles;
export type Project = { id: string; name: string; is_default: boolean; role: "admin" | GrantRole; client_ref: string | null };
export type Grant = { user_id: string; role: GrantRole; client_ref: string | null };

export const keyScopes = {
  "requests:read": "Read requests",
  "requests:submit": "Submit requests",
  "requests:cancel": "Cancel requests",
  "usage:read": "Read usage",
  "exports:read": "Export project data",
};
export type KeyScope = keyof typeof keyScopes;
type Principal = { id: string; name: string; scopes: KeyScope[]; client_ref: string | null; revoked_at: string | null };
type KeyMetadata = { id: string; principal_id: string; key_prefix: string; expires_at: string; revoked_at: string | null };
export type Credentials = { attached_principal_id?: string | null; principals: Principal[]; keys: KeyMetadata[] };
export type IssuedKey = { id: string; token: string; expires_at: string };
export const providerKeyNames = {
  openrouter: "OpenRouter", kinovi: "Kinovi", fal: "fal primary", "fal-secondary": "fal secondary",
} as const;
export const credentialProviders = ["openrouter", "kinovi", "fal", "fal-secondary"] as const;
export type ProviderKeyScope = { account_id: string; project_id?: string };
export type ProviderKeyTarget = ProviderKeyScope & ({ provider: keyof typeof providerKeyNames } | { binding: string });
type ProviderKeyVersion = {
  version: number;
  created_at: string;
  status: "active" | "retiring" | "retired" | "revoked" | "unavailable" | "inheriting";
};
export type ProviderKeyList = {
  enabled: boolean;
  mode?: "live";
  versions: ProviderKeyVersion[];
  organization_default?: Pick<ProviderKeyVersion, "version" | "status"> | null;
};

function path(accountId: string, rest = "") {
  return `/v1/projects${rest}?${new URLSearchParams({ account_id: accountId })}`;
}

function inProject(scope: ProjectScope, rest: string, before?: string) {
  const endpoint = path(scope.account_id, `/${encodeURIComponent(scope.project_id)}${rest}`);
  return before ? `${endpoint}&${new URLSearchParams({ before })}` : endpoint;
}

function providerKeyPath(scope: ProviderKeyTarget, version?: number) {
  const revision = version === undefined ? "" : `/${version}`;
  if ("binding" in scope) {
    const query = new URLSearchParams({ account_id: scope.account_id });
    if (scope.project_id) query.set("project_id", scope.project_id);
    return `/v1/provider-credentials/${encodeURIComponent(scope.binding)}${revision}?${query}`;
  }
  const suffix = `/provider-credentials/${scope.provider}${revision}`;
  if (scope.project_id) {
    return inProject({ account_id: scope.account_id, project_id: scope.project_id }, suffix);
  }
  return `/v1/accounts/${encodeURIComponent(scope.account_id)}${suffix}`;
}

function write(method: string, body: object): RequestInit {
  return { method, headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
}

export const projectsApi = {
  list: (accountId: string, signal: AbortSignal) => gatewayRequest<Project[]>(path(accountId), { signal }),
  create: (accountId: string, name: string) => gatewayRequest<Project>(path(accountId), write("POST", { name })),
  grants: (scope: ProjectScope, signal: AbortSignal) => gatewayRequest<Grant[]>(inProject(scope, "/grants"), { signal }),
  grant: (scope: ProjectScope, grant: Grant) => gatewayRequest<void>(
    inProject(scope, `/grants/${encodeURIComponent(grant.user_id)}`),
    write("PUT", { role: grant.role, client_ref: grant.client_ref }),
  ),
  revokeGrant: (scope: ProjectScope, userId: string) => gatewayRequest<void>(
    inProject(scope, `/grants/${encodeURIComponent(userId)}`), { method: "DELETE" },
  ),
  credentials: (scope: ProjectScope, signal: AbortSignal) => gatewayRequest<Credentials>(inProject(scope, "/credentials"), { signal }),
  providerKeys: (scope: ProviderKeyTarget, signal: AbortSignal) => gatewayRequest<ProviderKeyList>(
    providerKeyPath(scope), { signal },
  ),
  saveProviderKey: (scope: ProviderKeyTarget, expectedVersion: number, key: string) => gatewayRequest<void>(
    providerKeyPath(scope), write("PUT", { expected_version: expectedVersion, key }),
  ),
  inheritProviderKey: (scope: ProviderKeyTarget, expectedVersion: number) => gatewayRequest<void>(
    providerKeyPath(scope), write("PUT", { expected_version: expectedVersion, inherit: true }),
  ),
  revokeProviderKey: (scope: ProviderKeyTarget, version: number) => gatewayRequest<void>(
    providerKeyPath(scope, version), { method: "DELETE" },
  ),
  createPrincipal: (scope: ProjectScope, input: {
    name: string; scopes: KeyScope[]; client_ref: string | null; expires_in_days: number; attach?: boolean; bootstrap_default?: boolean;
  }, signal?: AbortSignal) => gatewayRequest<IssuedKey>(
    inProject(scope, "/service-principals"), { ...write("POST", input), signal },
  ),
  attachKey: (scope: ProjectScope, principalId: string) => gatewayRequest<void>(
    inProject(scope, "/credentials/attached"), write("PUT", { principal_id: principalId }),
  ),
  rotateKey: (scope: ProjectScope, principalId: string, expiresInDays: number) => gatewayRequest<IssuedKey>(
    inProject(scope, `/service-principals/${encodeURIComponent(principalId)}/keys`),
    write("POST", { expires_in_days: expiresInDays }),
  ),
  revokeKey: (scope: ProjectScope, keyId: string) => gatewayRequest<void>(
    inProject(scope, `/keys/${encodeURIComponent(keyId)}`), { method: "DELETE" },
  ),
  revokePrincipal: (scope: ProjectScope, principalId: string) => gatewayRequest<void>(
    inProject(scope, `/service-principals/${encodeURIComponent(principalId)}`), { method: "DELETE" },
  ),
};

export type WebhookSubscription = {
  id: string; name: string; destination_id: string; client_ref: string | null;
  workflow_app_id?: string;
  signing_version: number; signing_rotated_at: string | null; revoked_at: string | null;
};
export type WebhookDelivery = {
  id: string; request_id?: string; workflow_run_id?: string; subscription_id: string; event_type: string;
  state: "pending" | "sending" | "delivered" | "dead"; occurred_at: string;
  available_at: string; attempt_count: number; last_error: string | null;
  receiver_available_after?: string | null;
  next_attempt_at?: string | null;
};
type WebhookAttempt = { number: number; started_at: string; outcome: string | null; http_status: number | null };
export type WebhookDetail = { delivery: WebhookDelivery; attempts: WebhookAttempt[] };
export type WebhookPage<T> = { items: T[]; next_before: string | null };
export type IssuedWebhook = WebhookSubscription & { signing_secret: string };

export const webhooksApi = {
  destinations: (scope: ProjectScope, signal: AbortSignal) => gatewayRequest<string[]>(inProject(scope, "/webhook-destinations"), { signal }),
  subscriptions: (scope: ProjectScope, before: string | undefined, signal: AbortSignal) =>
    gatewayRequest<WebhookPage<WebhookSubscription>>(inProject(scope, "/webhooks", before), { signal }),
  subscription: (scope: ProjectScope, id: string, signal: AbortSignal) =>
    gatewayRequest<WebhookSubscription>(inProject(scope, `/webhooks/${encodeURIComponent(id)}`), { signal, cache: "no-store" }),
  create: (scope: ProjectScope, input: { name: string; destination_id: string; client_ref: string | null; workflow_app_id?: string }) =>
    gatewayRequest<IssuedWebhook>(inProject(scope, "/webhooks"), write("POST", input)),
  rotate: (scope: ProjectScope, id: string, key: string, expectedVersion: number) =>
    gatewayRequest<IssuedWebhook>(inProject(scope, `/webhooks/${encodeURIComponent(id)}/rotations`), {
      method: "POST", redirect: "error", cache: "no-store", signal: AbortSignal.timeout(15_000),
      headers: { "Content-Type": "application/json", "Idempotency-Key": key },
      body: JSON.stringify({ expected_signing_version: expectedVersion }),
    }),
  revoke: (scope: ProjectScope, id: string) =>
    gatewayRequest<void>(inProject(scope, `/webhooks/${encodeURIComponent(id)}`), { method: "DELETE" }),
  deliveries: (scope: ProjectScope, before: string | undefined, signal: AbortSignal) =>
    gatewayRequest<WebhookPage<WebhookDelivery>>(inProject(scope, "/webhook-deliveries", before), { signal }),
  detail: (scope: ProjectScope, id: string, signal: AbortSignal) =>
    gatewayRequest<WebhookDetail>(inProject(scope, `/webhook-deliveries/${encodeURIComponent(id)}`), { signal }),
  redrive: (scope: ProjectScope, id: string) =>
    gatewayRequest<void>(inProject(scope, `/webhook-deliveries/${encodeURIComponent(id)}/redrive`), { method: "POST" }),
};
