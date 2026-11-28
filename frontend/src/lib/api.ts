/**
 * The real API client (replaces mock-api for auth/account surfaces).
 *
 * Development defaults to http://localhost:4000. Set VITE_API_BASE_URL to
 * connect to a specific gateway or "/" to use the console's same-origin proxy.
 *
 * All requests send credentials (the `foundation_session` HttpOnly cookie);
 * the gateway's CORS layer allows the dev origins.
 */

const LOCAL_API_URL = "http://localhost:4000";
const SESSION_READ_TIMEOUT_MS = 10_000;

function resolveApiBaseUrl(): string {
  const explicit = import.meta.env.VITE_API_BASE_URL;
  if (explicit) return new URL(explicit, window.location.origin).href.replace(/\/+$/, "");
  if (import.meta.env.VITE_TOKENCRAFT_PACKAGED === "true") {
    const port = Number(document.querySelector<HTMLMetaElement>('meta[name="tokencraft-api-port"]')?.content);
    if (!Number.isInteger(port) || port < 1024 || port > 65535) {
      throw new Error("Start the packaged console with the TokenCraft launcher.");
    }
    return `http://localhost:${port}`;
  }
  return LOCAL_API_URL;
}

export const API_BASE_URL = resolveApiBaseUrl();

/** The `{ error: { code, message } }` envelope the gateway returns on failure. */
export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** Nudge the status banner (lib/api-status.ts) to re-check sooner. Fire-and-forget. */
function reportRequestFailure() {
  try {
    window.dispatchEvent(new Event("foundation:api-request-failed"));
  } catch {
    // never let the helper hurt the actual request path
  }
}

async function send<T>(
  method: string, path: string,
  init: { headers?: Record<string, string>; body?: BodyInit; signal?: AbortSignal },
): Promise<T> {
  let resp: Response;
  try {
    resp = await fetch(`${API_BASE_URL}${path}`, {
      method,
      credentials: "include",
      cache: "no-store",
      headers: init.headers,
      body: init.body,
      signal: init.signal,
    });
  } catch {
    init.signal?.throwIfAborted();
    reportRequestFailure();
    throw new ApiError(0, "network", `Could not reach the API at ${API_BASE_URL}. Is the server running?`);
  }
  if (resp.status >= 500) reportRequestFailure();
  const json = await resp.json().catch(() => null);
  init.signal?.throwIfAborted();
  if (!resp.ok) {
    const err = (json as { error?: { code?: string; message?: string } } | null)?.error;
    throw new ApiError(resp.status, err?.code ?? "unknown", err?.message ?? `Request failed (${resp.status})`);
  }
  return json as T;
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  return send(
    method,
    path,
    body !== undefined
      ? { headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }
      : {},
  );
}

/* ── Types (mirror the gateway's JSON) ── */

/** CDN links to an avatar image. `thumbnail_template` carries a `{WIDTH}` placeholder (in dev
 * it's just the full-image URL — no resizer in front of the bucket). */
export type AvatarLinks = {
  cdn_url: string;
  thumbnail_template: string;
};

/** What a user can choose to display. */
export type AvatarSource = "custom" | "gravatar" | "letter";

/** `preferred_source` values: a choice, or `not_yet_set` (system auto-picks: gravatar when
 * mirrored, else the monogram). */
export type PreferredSource = AvatarSource | "not_yet_set" | "generated";

/** The avatar as APIs report it: the user's preference, the derived effective source (what
 * actually displays), CDN links (null → render the monogram), and availability flags. */
export type ApiAvatar = {
  /** What's actually displaying: the preference when satisfiable, else "letter". */
  source: string;
  /** The user's stated choice; "not_yet_set" until they pick (system auto-picks meanwhile). */
  preferred_source: PreferredSource;
  links: AvatarLinks | null;
  /** Preview links for the custom upload, present whenever one exists (settings shows all
   * options side by side). */
  custom_links: AvatarLinks | null;
  /** Preview links for the mirrored gravatar, present whenever one exists. */
  gravatar_links: AvatarLinks | null;
  /** A custom upload exists (choosable even when not displaying). */
  has_custom: boolean;
  /** A mirrored gravatar exists (choosable even when not displaying). */
  has_gravatar: boolean;
  /** False while a gravatar (re-)poll is pending — the background job hasn't checked yet. */
  gravatar_checked: boolean;
  /** User-picked initials gradient slug; null = the default ("brand"). */
  monogram_gradient: string | null;
};

/** Crop rect in source-image pixels (square — encodes zoom + translate of a square viewport). */
export type AvatarCrop = { x: number; y: number; size: number };

export type ApiUser = {
  id: string;
  email: string;
  display_name: string | null;
  is_staff: boolean;
  email_verified: boolean;
  /** False for SSO-only accounts — offer "set a password" instead of "change". */
  has_password: boolean;
  /** Only present on `/me` responses; absent right after login/signup (refreshed on next load). */
  avatar?: ApiAvatar;
};

export type ApiSession = {
  id: string;
  ip: string | null;
  user_agent: string | null;
  device: string | null;
  /** How the session came to exist: "signup" | "invited_signup" | "login" | "sso_login" | …. */
  created_via: string;
  created_at: string;
  expires_at: string;
  this_device: boolean;
};

type AuthResponse = { user: ApiUser; session_id: string };
type MeResponse = { user: ApiUser; session_id: string; impersonation: boolean; sso_providers: string[] };
export type { MeResponse };

/** One of the caller's organizations (memberships), for the org switcher. */
export type ApiAccount = {
  account_id: string;
  name: string | null;
  /** Owner/admin-set; null → display `placeholder_business_name`. */
  business_name: string | null;
  /** Random "Adjective Noun" minted at account creation; immutable. */
  placeholder_business_name: string;
  role: string;
  joined_at: string;
};

/** What clients should render as the org's name. */
export function accountDisplayName(a: ApiAccount): string {
  return a.business_name ?? a.placeholder_business_name;
}

/** A row in an organization's member roster. */
export type ApiMember = {
  user_id: string;
  display_name: string | null;
  email: string;
  /** The member's chosen avatar (custom upload, gravatar, or monogram). */
  avatar: ApiAvatar;
  role: string;
  joined_at: string;
  is_you: boolean;
};

/** Display info for rendering a user anywhere (audit actors, pickers, …). */
export type ApiAccountUser = {
  user_id: string;
  display_name: string | null;
  email: string;
  avatar: ApiAvatar;
};

/** A resolved invitation, with what the join page needs to branch (see /join). */
export type ApiInvitation = {
  invitation_id: string;
  account_id: string;
  account_name: string | null;
  email: string;
  role: string;
  expires_at: string;
  /** A user already exists for the invited email → show login, not signup. */
  invitee_user_exists: boolean;
};

/** A pending (open) invitation, as the org page lists them. */
export type ApiPendingInvitation = {
  invitation_id: string;
  email: string;
  role: string;
  /** The short human code — re-shareable ("join with code"). */
  code: string;
  invited_by_user_id: string;
  created_at: string;
  expires_at: string;
};

/** An API key as lists report it — identifying prefix only; the secret is shown once. */
export type ApiKeyItem = {
  id: string;
  title: string;
  description: string | null;
  key_prefix: string;
  created_by_user_id: string;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
};

/** Every way the user can sign in: the password entry + SSO connections. */
export type ApiLoginMethods = {
  password: { is_enabled: boolean };
  sso_connections: {
    id: string;
    provider: string;
    provider_email: string;
    created_at: string;
    last_used_at: string;
  }[];
};

/** One audit-log row (user or account trail). */
export type AuditItem = {
  id: string;
  event: string;
  actor_user_id: string | null;
  user_id?: string | null;
  target_type: string | null;
  target_id: string | null;
  data: Record<string, unknown> | null;
  is_org_visible?: boolean;
  ip: string | null;
  created_at: string;
};

/** A keyset-paginated page: pass `next_before` back as `before` for the next page. */
export type AuditPage = { items: AuditItem[]; next_before: string | null };

/* ── Generation (jobs) ── */

export type ApiImageSize = "1024x1024" | "1536x1024" | "1024x1536" | "auto";
export type ApiImageQuality = "low" | "medium" | "high" | "auto";
export type ApiOutputFormat = "png" | "jpeg" | "webp";
export type ApiInputFidelity = "low" | "high";

/** Image parameters retained in legacy job history. */
export type ImageGenerationParams = {
  account_id: string;
  prompt: string;
  /** `file_…` ids (from uploadFile) or public https URLs. */
  image_inputs?: string[];
  mask?: string | null;
  /** 1–10; each image is generated independently (partial completion). */
  n?: number;
  size?: ApiImageSize;
  quality?: ApiImageQuality;
  output_format?: ApiOutputFormat;
  input_fidelity?: ApiInputFidelity;
  /** Validate, price, and log the exact provider payload — but never send. The job lands
   * terminal (`canceled` / failure_code `dry_run`) and costs nothing. */
  dry_run?: boolean;
};

export type ApiVideoResolution = "720p" | "1080p" | "4k";
export type ApiVideoAspectRatio = "16:9" | "9:16";

/** Video parameters retained in legacy job history. */
export type VideoGenerationParams = {
  account_id: string;
  prompt: string;
  negative_prompt?: string | null;
  /** Image-to-video conditioning frame: a `file_…` id (from uploadFile) or a public
   * https URL. */
  image_input?: string | null;
  /** 1–10; one provider operation per video (partial completion). */
  n?: number;
  resolution?: ApiVideoResolution;
  /** 4, 6, or 8. 1080p/4K forces 8 (coerced and reported). */
  duration_seconds?: number;
  aspect_ratio?: ApiVideoAspectRatio;
  dry_run?: boolean;
};

export type ApiJobOutput = { index: number; kind: string; url: string; mime: string; bytes: number };

/** Present only for fanned-out (n>1) jobs. */
export type ApiJobProgress = { completed: number; total: number; succeeded: number; failed: number };

export type ApiJobStatus = "queued" | "running" | "succeeded" | "failed" | "canceled" | "expired";

/** Which door created the job. */
export type ApiJobSource = "api_key" | "dashboard" | "mcp";

/** Which API plane the request came through (`unified` = model-specific endpoint). */
export type ApiJobShape = "granular" | "unified" | "any" | "chat";

/** `GET /v1/jobs/{id}/request` — a job's originating request in the SAME wire shape the
 * generate endpoints accept (image refs resolved to CDN URLs), so it can be replayed. */
export type ApiJobRequest = {
  job_id: string;
  model: string;
  /** `image` or `video` — which optional param set below is populated. */
  media_type: string;
  prompt: string;
  n: number;
  dry_run: boolean;
  /* image params (media_type = image; empty/absent fields are omitted on the wire) */
  image_inputs?: string[];
  mask?: string | null;
  size?: ApiImageSize | null;
  quality?: ApiImageQuality | null;
  output_format?: ApiOutputFormat | null;
  input_fidelity?: ApiInputFidelity | null;
  /* video params (media_type = video) */
  negative_prompt?: string | null;
  image_input?: string | null;
  resolution?: ApiVideoResolution | null;
  duration_seconds?: number | null;
  aspect_ratio?: ApiVideoAspectRatio | null;
};

export type ApiJob = {
  id: string;
  model: string;
  modality: string;
  source: ApiJobSource;
  api_shape: ApiJobShape;
  status: ApiJobStatus;
  /** True for dry-run jobs — nothing was sent to the provider. */
  dry_run: boolean;
  progress?: ApiJobProgress | null;
  outputs: ApiJobOutput[];
  failure_code: string | null;
  failure_message: string | null;
  estimated_micros: number;
  param_adjustments: { param: string; action: "pruned" | "coerced"; detail: string }[];
  created_at: string;
  completed_at: string | null;
};

export type AdminJob = ApiJob & { account_id: string };

function pageQuery(before?: string, limit = 25): string {
  const params = new URLSearchParams({ limit: String(limit) });
  if (before) params.set("before", before);
  return `?${params}`;
}

/* ── Auth ── */

export const api = {
  /** `invitation` (a code or magic-link token) redeems an invite during signup: the email
   * must match the invite's, and the user joins the inviting org instead of getting their
   * own. */
  signup: (email: string, password: string, displayName?: string, invitation?: string) =>
    request<AuthResponse>("POST", "/v1/auth/signup", {
      email,
      password,
      display_name: displayName || undefined,
      invitation: invitation || undefined,
    }),

  login: (email: string, password: string) =>
    request<AuthResponse>("POST", "/v1/auth/login", { email, password }),

  /** SSO: POST the provider credential (Google: the GIS ID-token JWT) for server-side
   * verification. Signup on first visit, login after; sets the session cookie. */
  oauthCallback: (provider: "google", credential: string) =>
    request<AuthResponse>("POST", `/v1/auth/oauth/${provider}/callback`, { credential }),

  /** GitHub SSO: POST the authorization code + PKCE verifier — the backend exchanges them
   * (with the client secret) and verifies the identity. */
  oauthGithubCallback: (code: string, codeVerifier: string) =>
    request<AuthResponse>("POST", "/v1/auth/oauth/github/callback", { code, code_verifier: codeVerifier }),

  /* ── Login methods (settings): password entry + SSO connections ── */

  listLoginMethods: () => request<ApiLoginMethods>("GET", "/v1/auth/login_methods"),

  /** Link Google to the CURRENT user (verified server-side; 409 if linked elsewhere). */
  linkGoogle: (credential: string) =>
    request<ApiLoginMethods>("POST", "/v1/auth/login_methods/google", { credential }),

  /** Link Google via a token-client access token (the custom, site-styled Connect button —
   * the backend verifies it via tokeninfo + userinfo). */
  linkGoogleAccessToken: (accessToken: string) =>
    request<ApiLoginMethods>("POST", "/v1/auth/login_methods/google", { access_token: accessToken }),

  /** Link GitHub to the CURRENT user (code exchanged server-side; 409 if linked elsewhere). */
  linkGithub: (code: string, codeVerifier: string) =>
    request<ApiLoginMethods>("POST", "/v1/auth/login_methods/github", { code, code_verifier: codeVerifier }),

  /** Disconnect an SSO connection (403 when it could lock the user out). */
  unlinkSso: (ssoId: string) => request<{ ok: boolean }>("DELETE", `/v1/auth/login_methods/${ssoId}`),

  logout: () => request<{ ok: boolean }>("POST", "/v1/auth/logout"),

  me: (signal?: AbortSignal) => {
    const deadline = AbortSignal.timeout(SESSION_READ_TIMEOUT_MS);
    return send<MeResponse>("GET", "/v1/auth/me", {
      signal: signal ? AbortSignal.any([signal, deadline]) : deadline,
    });
  },

  /* ── Account security ── */

  /** Change the password — pass `null` for `currentPassword` when the account has none yet
   * (SSO-only, `has_password: false`): that SETS the first password. */
  changePassword: (currentPassword: string | null, newPassword: string, logoutOtherSessions: boolean) =>
    request<{ ok: boolean }>("PUT", "/v1/auth/password", {
      current_password: currentPassword ?? undefined,
      new_password: newPassword,
      logout_other_sessions: logoutOtherSessions,
    }),

  changeEmail: (newEmail: string, currentPassword: string) =>
    request<{ ok: boolean; email: string; email_verified: boolean }>("PUT", "/v1/auth/email", {
      new_email: newEmail,
      current_password: currentPassword,
    }),

  requestPasswordReset: (email: string) =>
    request<{ ok: boolean; message: string }>("POST", "/v1/auth/password-reset", { email }),

  confirmPasswordReset: (token: string, newPassword: string, logoutOtherSessions: boolean) =>
    request<{ ok: boolean }>("POST", "/v1/auth/password-reset/confirm", {
      token,
      new_password: newPassword,
      logout_other_sessions: logoutOtherSessions,
    }),

  /** Send (or re-send) the verification email for the current address. */
  requestEmailVerification: () =>
    request<{ ok: boolean; message: string }>("POST", "/v1/auth/email/verification"),

  /** Consume a verification token (from the emailed link). Writes the org-visible
   * `email_verified` audit event. */
  confirmEmailVerification: (token: string) =>
    request<{ ok: boolean }>("POST", "/v1/auth/email/verification/confirm", { token }),

  /** Update the display name (empty/undefined clears it). Writes the org-visible
   * `display_name_changed` audit event. */
  updateProfile: (displayName: string | null) =>
    request<{ ok: boolean; display_name: string | null }>("PUT", "/v1/auth/profile", {
      display_name: displayName,
    }),

  /* ── Avatar ── */

  /** Upload/replace the avatar. Body is the raw file; `crop` (source pixels) encodes the
   * square viewport's zoom + pan — omitted, the server center-crops the largest square. */
  uploadAvatar: (file: Blob, crop?: AvatarCrop) => {
    const params = crop ? `?crop_x=${crop.x}&crop_y=${crop.y}&crop_size=${crop.size}` : "";
    return send<{ ok: boolean; avatar: ApiAvatar }>("POST", `/v1/auth/avatar${params}`, { body: file });
  },

  removeAvatar: () => request<{ ok: boolean; avatar: ApiAvatar }>("DELETE", "/v1/auth/avatar"),

  /** Choose what displays: photo, gravatar, or the letter monogram. */
  setAvatarPreference: (source: AvatarSource) =>
    request<{ ok: boolean; avatar: ApiAvatar }>("PUT", "/v1/auth/avatar/preference", { source }),

  /** Remove the mirrored gravatar (won't re-mirror unless explicitly refreshed). */
  removeGravatar: () => request<{ ok: boolean; avatar: ApiAvatar }>("DELETE", "/v1/auth/avatar/gravatar"),

  /** Re-request the gravatar — the background job re-fetches within seconds. */
  refreshGravatar: () => request<{ ok: boolean; avatar: ApiAvatar }>("POST", "/v1/auth/avatar/gravatar/refresh"),

  /** Pick the initials-monogram gradient (stored; shown to everyone who sees the monogram). */
  setMonogramGradient: (gradient: string) =>
    request<{ ok: boolean; avatar: ApiAvatar }>("PUT", "/v1/auth/avatar/monogram", { gradient }),

  /* ── Organizations ── */

  /** Every organization the caller belongs to (drives the org switcher). */
  listAccounts: () => request<{ accounts: ApiAccount[] }>("GET", "/v1/accounts"),

  /** The member roster of an organization (any member may view). */
  listMembers: (accountId: string) =>
    request<{ members: ApiMember[]; your_role: string }>("GET", `/v1/accounts/${accountId}/members`),

  /** Invite someone (owner/admin). Returns the code + join link to hand to the invitee. */
  inviteMember: (accountId: string, email: string, role: "admin" | "member") =>
    request<{ invitation_id: string; code: string; join_url: string; email: string; role: string; expires_at: string }>(
      "POST",
      `/v1/accounts/${accountId}/invitations`,
      { email, role },
    ),

  /** Remove a member (owner/admin; owner is immovable, admins can't remove admins). */
  removeMember: (accountId: string, userId: string) =>
    request<{ ok: boolean }>("DELETE", `/v1/accounts/${accountId}/members/${userId}`),

  /** Display info (name, email, chosen avatar) for user ids — CURRENT members of the
   * account only; unknown/departed ids are silently omitted. Any member may call. */
  accountUsers: (accountId: string, ids: string[]) =>
    request<{ users: ApiAccountUser[] }>(
      "GET",
      `/v1/accounts/${accountId}/users?ids=${ids.map(encodeURIComponent).join(",")}`,
    ),

  /** Set or clear the business name (owner/admin only; null/blank clears back to the
   * placeholder). The placeholder itself is immutable. */
  updateBusinessName: (accountId: string, businessName: string | null) =>
    request<{ ok: boolean; business_name: string | null; placeholder_business_name: string }>(
      "PUT",
      `/v1/accounts/${accountId}/business`,
      { business_name: businessName },
    ),

  /** The org's pending invitations (owner/admin only). */
  listInvitations: (accountId: string) =>
    request<{ invitations: ApiPendingInvitation[] }>("GET", `/v1/accounts/${accountId}/invitations`),

  /** Cancel a pending invitation (owner/admin) — its link and code die immediately. */
  cancelInvitation: (accountId: string, invitationId: string) =>
    request<{ ok: boolean }>("DELETE", `/v1/accounts/${accountId}/invitations/${invitationId}`),

  /** Resolve an invitation (public) for the join page — by short code or magic-link token. */
  resolveInvitation: (codeOrToken: string) =>
    request<ApiInvitation>("GET", `/v1/invitations/${encodeURIComponent(codeOrToken)}`),

  /** Accept an invitation as the logged-in user (creates the membership). */
  acceptInvitation: (invitationId: string) =>
    request<{ ok: boolean; invitation_id: string }>("POST", `/v1/invitations/${invitationId}/accept`),

  /* ── API keys (any member may list/create/delete) ── */

  listApiKeys: (accountId: string) =>
    request<{ api_keys: ApiKeyItem[] }>("GET", `/v1/accounts/${accountId}/api_keys`),

  /** Mint a key. `secret` in the response is shown ONCE — it can never be retrieved again. */
  createApiKey: (accountId: string, title: string, description?: string) =>
    request<{ api_key: ApiKeyItem; secret: string }>("POST", `/v1/accounts/${accountId}/api_keys`, {
      title,
      description: description || undefined,
    }),

  /** Revoke ("delete") a key — it stops authenticating immediately. */
  deleteApiKey: (accountId: string, keyId: string) =>
    request<{ ok: boolean }>("DELETE", `/v1/accounts/${accountId}/api_keys/${keyId}`),

  /* ── Audit logs (keyset-paginated) ── */

  /** The caller's own audit trail (private rows included). */
  userAuditLogs: (before?: string, limit?: number) =>
    request<AuditPage>("GET", `/v1/user/audit_logs${pageQuery(before, limit)}`),

  /** The organization's audit trail (owner/admin only). */
  accountAuditLogs: (accountId: string, before?: string, limit?: number) =>
    request<AuditPage>("GET", `/v1/accounts/${accountId}/audit_logs${pageQuery(before, limit)}`),

  /** A member's trail as the org sees it — org-visible rows only (owner/admin only). */
  memberAuditLogs: (accountId: string, userId: string, before?: string, limit?: number) =>
    request<AuditPage>("GET", `/v1/accounts/${accountId}/members/${userId}/audit_logs${pageQuery(before, limit)}`),

  /* ── Generation ── */

  /** The job's originating request, replayable (recreate flows, request inspection). */
  getJobRequest: (jobId: string, accountId: string) =>
    request<ApiJobRequest>("GET", `/v1/jobs/${jobId}/request?account_id=${encodeURIComponent(accountId)}`),

  /** The account's job history, newest first (keyset: pass `after` from the prior page). */
  listJobs: (accountId: string, opts?: { model?: string; after?: string }) => {
    const q = new URLSearchParams({ account_id: accountId });
    if (opts?.model) q.set("model", opts.model);
    if (opts?.after) q.set("after", opts.after);
    return request<{ jobs: ApiJob[]; next_after: string | null }>("GET", `/v1/jobs?${q}`);
  },

  /** Staff only: recent jobs across every account. */
  /** Staff only: the originating request behind any account's job. */
  adminGetJobRequest: (jobId: string) => request<ApiJobRequest>("GET", `/v1/admin/jobs/${jobId}/request`),

  adminListJobs: (after?: string) =>
    request<{ jobs: AdminJob[]; next_after: string | null }>(
      "GET",
      `/v1/admin/jobs${after ? `?after=${encodeURIComponent(after)}` : ""}`,
    ),

  /* ── Sessions ── */

  listSessions: () => request<{ sessions: ApiSession[] }>("GET", "/v1/auth/sessions"),

  revokeSession: (id: string) => request<{ ok: boolean }>("DELETE", `/v1/auth/sessions/${id}`),

  revokeOtherSessions: () => request<{ ok: boolean }>("DELETE", "/v1/auth/sessions?others=true"),
};
