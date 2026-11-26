/**
 * Mock API + fixtures. NO real backend — this stands in for the future gateway
 * so the UX can be built and the "seams" evaluated. Everything here is
 * deterministic (no Date.now/Math.random at module load) so the mock renders
 * identically on every reload.
 *
 * Request ledger note (flagged for the architecture docs): the dashboard shows
 * two scopes of history —
 *   • "mine"    — requests the logged-in dashboard user made directly, and
 *   • "account" — EVERYTHING that transited the account, including traffic from
 *                 our customers' own end-users (attributed via `end_user`).
 * The `end_user` field models the customer-of-our-customer identifier that a
 * tenant passes on each API call so usage can be traced back to their users.
 */

import { MODEL_BY_SLUG, type Model, priceLabel } from "./catalog";

/* ── Auth / account (mock "logged in") ── */
export type User = {
  id: string;
  name: string;
  email: string;
  org: string;
  orgId: string;
  role: "owner" | "admin" | "member";
  avatarInitials: string;
  plan: "Pay-as-you-go" | "Scale" | "Enterprise";
};

export const CURRENT_USER: User = {
  id: "usr_3QK7ZP2M",
  name: "Ada Rivera",
  email: "ada@lumenlabs.io",
  org: "Lumen Labs",
  orgId: "org_8FT2A1CX",
  role: "owner",
  avatarInitials: "AR",
  plan: "Scale",
};

export type Balance = {
  currency: "USD";
  /** Prepaid credits remaining, in USD. */
  credits: number;
  /** Auto-reload threshold + amount. */
  autoReload: boolean;
  autoReloadThreshold: number;
  autoReloadAmount: number;
  /** This calendar month's spend so far. */
  monthToDateSpend: number;
};

export const BALANCE: Balance = {
  currency: "USD",
  credits: 428.17,
  autoReload: true,
  autoReloadThreshold: 50,
  autoReloadAmount: 250,
  monthToDateSpend: 1_284.42,
};

/* ── API keys (mock) ── */
export type ApiKey = {
  id: string;
  name: string;
  prefix: string;
  last4: string;
  created: string;
  lastUsed: string | null;
  env: "live" | "test";
};

export const API_KEYS: ApiKey[] = [
  { id: "key_9GB2", name: "Production", prefix: "sk_live_", last4: "a91f", created: "2026-02-11", lastUsed: "2026-07-19", env: "live" },
  { id: "key_4LM8", name: "Staging", prefix: "sk_live_", last4: "77c2", created: "2026-04-02", lastUsed: "2026-07-18", env: "live" },
  { id: "key_1TT0", name: "Local dev", prefix: "sk_test_", last4: "0b3e", created: "2026-06-20", lastUsed: "2026-07-17", env: "test" },
];

/* ── Request ledger ── */
export type JobStatus = "succeeded" | "failed" | "running";

export type Request = {
  id: string;
  modelSlug: string;
  modality: Model["modality"];
  endpoint: string;
  status: JobStatus;
  /** ISO timestamp. */
  createdAt: string;
  latencyMs: number;
  costUsd: number;
  /** Where the call came from. */
  source: "playground" | "api";
  /** Which of the tenant's API keys (null for playground). */
  keyId: string | null;
  /** The customer-of-our-customer identifier, if the tenant supplied one. */
  endUser: string | null;
  /** True when the logged-in dashboard user made this directly. */
  mine: boolean;
  /** Short human summary of the input. */
  summary: string;
};

/**
 * A compact, deterministic fixture ledger. Mixes:
 *  - the logged-in user's own playground + API calls (mine=true)
 *  - other teammates' API calls (mine=false, endUser=null)
 *  - end-user traffic from the tenant's customers (mine=false, endUser set)
 */
export const REQUESTS: Request[] = [
  { id: "req_7f3a91", modelSlug: "google_nano_banana_2", modality: "image", endpoint: "/v1/models/google_nano_banana_2/generate", status: "succeeded", createdAt: "2026-07-19T14:22:10Z", latencyMs: 2140, costUsd: 0.08, source: "playground", keyId: null, endUser: null, mine: true, summary: "Neon-lit ramen shop, rain, cinematic" },
  { id: "req_7f3a55", modelSlug: "anthropic_claude_opus_4_8", modality: "chat", endpoint: "/v1/chat/completions", status: "succeeded", createdAt: "2026-07-19T14:05:44Z", latencyMs: 5210, costUsd: 0.031, source: "playground", keyId: null, endUser: null, mine: true, summary: "Draft release notes for v2 launch" },
  { id: "req_6d2b04", modelSlug: "bytedance_seedance_2", modality: "video", endpoint: "/v1/models/bytedance_seedance_2/generate/video", status: "succeeded", createdAt: "2026-07-19T13:41:02Z", latencyMs: 38400, costUsd: 0.6, source: "api", keyId: "key_9GB2", endUser: "cust_a2f_user_5512", mine: false, summary: "Product hero clip, 5s, orbit" },
  { id: "req_6d1a77", modelSlug: "openai_gpt_image_2", modality: "image", endpoint: "/v1/any/image", status: "succeeded", createdAt: "2026-07-19T13:38:55Z", latencyMs: 3300, costUsd: 0.12, source: "api", keyId: "key_9GB2", endUser: "cust_a2f_user_5512", mine: false, summary: "Character sheet, 4K, ref x3" },
  { id: "req_5c9e21", modelSlug: "worldlabs_marble_1_1", modality: "world", endpoint: "/v1/models/worldlabs_marble_1_1/generate", status: "running", createdAt: "2026-07-19T14:24:31Z", latencyMs: 0, costUsd: 1.1, source: "api", keyId: "key_9GB2", endUser: "cust_b71_user_0090", mine: false, summary: "Explorable forest clearing" },
  { id: "req_5c81f0", modelSlug: "tencent_hunyuan3d_2_5", modality: "3d", endpoint: "/v1/models/tencent_hunyuan3d_2_5/generate", status: "succeeded", createdAt: "2026-07-19T12:50:09Z", latencyMs: 21900, costUsd: 0.5, source: "api", keyId: "key_4LM8", endUser: "cust_b71_user_0090", mine: false, summary: "Low-poly treasure chest, quad" },
  { id: "req_4b7c33", modelSlug: "openai_gpt_image_1_5", modality: "image", endpoint: "/v1/any/image", status: "failed", createdAt: "2026-07-19T12:31:47Z", latencyMs: 900, costUsd: 0, source: "api", keyId: "key_9GB2", endUser: "cust_c04_user_3120", mine: false, summary: "Rejected — content policy" },
  { id: "req_4a55c9", modelSlug: "google_nano_banana", modality: "image", endpoint: "/v1/any/image", status: "succeeded", createdAt: "2026-07-19T11:58:22Z", latencyMs: 1780, costUsd: 0.02, source: "api", keyId: "key_9GB2", endUser: "cust_c04_user_3120", mine: false, summary: "Blog thumbnail, flat illustration" },
  { id: "req_3f22a1", modelSlug: "meta_llama_4_maverick", modality: "chat", endpoint: "/v1/chat/completions", status: "succeeded", createdAt: "2026-07-19T11:20:05Z", latencyMs: 4100, costUsd: 0.008, source: "api", keyId: "key_4LM8", endUser: "cust_a2f_user_7781", mine: false, summary: "Summarize support ticket thread" },
  { id: "req_3e10b8", modelSlug: "anthropic_claude_haiku_4_5", modality: "chat", endpoint: "/v1/chat/completions", status: "succeeded", createdAt: "2026-07-19T10:44:51Z", latencyMs: 1200, costUsd: 0.002, source: "api", keyId: "key_9GB2", endUser: "cust_a2f_user_7781", mine: false, summary: "Classify intent: refund vs. exchange" },
  { id: "req_2d09f4", modelSlug: "bytedance_seedance_2_fast", modality: "video", endpoint: "/v1/models/bytedance_seedance_2_fast/generate/video", status: "succeeded", createdAt: "2026-07-19T09:31:18Z", latencyMs: 16800, costUsd: 0.3, source: "playground", keyId: null, endUser: null, mine: true, summary: "Preview clip, 5s, pan" },
  { id: "req_1a77e2", modelSlug: "google_nano_banana_pro", modality: "image", endpoint: "/v1/models/google_nano_banana_pro/generate", status: "succeeded", createdAt: "2026-07-18T22:11:40Z", latencyMs: 2600, costUsd: 0.05, source: "playground", keyId: null, endUser: null, mine: true, summary: "Poster, 2K, bold typography" },
  { id: "req_0c33d1", modelSlug: "openai_gpt_5", modality: "chat", endpoint: "/v1/chat/completions", status: "succeeded", createdAt: "2026-07-18T20:02:12Z", latencyMs: 6700, costUsd: 0.044, source: "api", keyId: "key_9GB2", endUser: "cust_c04_user_8830", mine: false, summary: "Generate product descriptions x20" },
  { id: "req_0b12a9", modelSlug: "tencent_hunyuan3d_2", modality: "3d", endpoint: "/v1/any/3d", status: "succeeded", createdAt: "2026-07-18T18:47:33Z", latencyMs: 19500, costUsd: 0.35, source: "api", keyId: "key_4LM8", endUser: "cust_b71_user_0090", mine: false, summary: "Stylized barrel, triangle mesh" },
];

/** Distinct end-users seen on the account (for the account-wide view filter). */
export const END_USERS = Array.from(new Set(REQUESTS.map((r) => r.endUser).filter(Boolean))) as string[];

/* ── Derived summaries ── */
export function requestModel(r: Request): Model | undefined {
  return MODEL_BY_SLUG[r.modelSlug];
}

export type UsagePoint = { day: string; spend: number; requests: number };

/** Deterministic 14-day usage series for the dashboard chart. */
export const USAGE_14D: UsagePoint[] = [
  { day: "Jul 06", spend: 61.2, requests: 820 },
  { day: "Jul 07", spend: 74.9, requests: 910 },
  { day: "Jul 08", spend: 58.1, requests: 770 },
  { day: "Jul 09", spend: 92.4, requests: 1120 },
  { day: "Jul 10", spend: 88.0, requests: 1040 },
  { day: "Jul 11", spend: 70.6, requests: 880 },
  { day: "Jul 12", spend: 44.3, requests: 590 },
  { day: "Jul 13", spend: 51.7, requests: 640 },
  { day: "Jul 14", spend: 99.8, requests: 1210 },
  { day: "Jul 15", spend: 112.5, requests: 1330 },
  { day: "Jul 16", spend: 104.2, requests: 1250 },
  { day: "Jul 17", spend: 97.1, requests: 1180 },
  { day: "Jul 18", spend: 120.6, requests: 1410 },
  { day: "Jul 19", spend: 86.4, requests: 990 },
];

/* ── Invoices (mock, gapless numbering per design docs) ── */
export type Invoice = { number: string; period: string; amount: number; status: "paid" | "open"; issued: string };
export const INVOICES: Invoice[] = [
  { number: "INV-000148", period: "Jun 2026", amount: 2412.88, status: "paid", issued: "2026-07-01" },
  { number: "INV-000131", period: "May 2026", amount: 1988.4, status: "paid", issued: "2026-06-01" },
  { number: "INV-000119", period: "Apr 2026", amount: 2140.09, status: "paid", issued: "2026-05-01" },
];

/* ── Mock "generate" call used by the Playground ── */
export type GenerateResult = {
  ok: boolean;
  requestId: string;
  costUsd: number;
  costBreakdown: string;
  latencyMs: number;
};

let genCounter = 100;

/**
 * Simulate a generation. Returns a mock result after a short delay so the
 * Playground can show a realistic pending → done transition. Cost is computed
 * from the model's first price and the relevant control values.
 */
export function mockGenerate(model: Model, params: Record<string, unknown>): Promise<GenerateResult> {
  genCounter += 1;
  const id = `req_pg${genCounter.toString(36)}`;
  const primary = model.pricing[0];
  let cost = primary.usd;
  let breakdown = priceLabel(primary);

  if (primary.unit === "per_video_second") {
    const secs = Number(params.duration ?? 5);
    cost = primary.usd * secs;
    breakdown = `${secs}s × ${priceLabel(primary)}`;
  } else if (primary.unit === "per_image") {
    const n = Number(params.num_images ?? 1);
    cost = primary.usd * n;
    breakdown = `${n} × ${priceLabel(primary)}`;
  } else if (primary.unit === "per_million_tokens_in") {
    // rough estimate for chat: assume ~1k in + ~1k out
    const out = model.pricing.find((p) => p.unit === "per_million_tokens_out");
    cost = (primary.usd + (out?.usd ?? 0)) / 1000;
    breakdown = "≈1K in + 1K out tokens";
  }

  const latencyMs = model.modality === "video" ? 18000 : model.modality === "3d" || model.modality === "world" ? 22000 : model.modality === "chat" ? 3000 : 2200;

  return new Promise((resolve) => {
    setTimeout(() => resolve({ ok: true, requestId: id, costUsd: Number(cost.toFixed(4)), costBreakdown: breakdown, latencyMs }), 1400);
  });
}

/* ── Playground request history (like Fal) ── */
export type PlayRun = {
  id: string;
  modelSlug: string;
  status: JobStatus;
  createdAt: string;
  costUsd: number;
  latencyMs: number;
  /** true = this logged-in user ran it in the dashboard; false = API / other. */
  mine: boolean;
  /** customer-of-our-customer id when the run came from an end-user. */
  endUser: string | null;
  /** deterministic hue for the mock output thumbnail. */
  hue: number;
};

function seedHash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0) % 100000;
}

/**
 * Deterministic per-model history so every model's playground has a populated
 * "recent runs" panel. Mixes the logged-in user's own dashboard runs with
 * account-wide traffic (API calls + our customers' end-users) — the same two
 * scopes as the dashboard Requests view, filtered to this one model.
 */
export function playgroundHistory(modelSlug: string): PlayRun[] {
  const endUsers = ["cust_a2f_user_5512", "cust_b71_user_0090", "cust_c04_user_3120", "cust_a2f_user_7781"];
  const runs: PlayRun[] = [];
  for (let i = 0; i < 9; i++) {
    const h = seedHash(`${modelSlug}|${i}`);
    const mine = i < 4; // first four are the current user's dashboard runs
    const minsAgo = 6 + i * 47 + (h % 40);
    const t = new Date(Date.parse("2026-07-19T14:30:00Z") - minsAgo * 60_000);
    const status: JobStatus = i === 1 ? "running" : h % 17 === 0 ? "failed" : "succeeded";
    runs.push({
      id: `req_pg${(h % 1_000_000).toString(36)}`,
      modelSlug,
      status,
      createdAt: t.toISOString(),
      costUsd: Number((((h % 90) + 2) / 1000).toFixed(3)),
      latencyMs: 1200 + (h % 40000),
      mine,
      endUser: mine ? null : endUsers[h % endUsers.length],
      hue: h % 360,
    });
  }
  return runs;
}
