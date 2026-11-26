import { providerChoices } from "@/surfaces/comparison/api";
import { statusNames } from "@/surfaces/comparison/RequestHistoryFilters";
import { dateRangeError, diagnosticNames, shiftDate, validDate, type InvestigationFilters } from "./historyDiagnostics";

export type FilterReply = { filters: InvestigationFilters; engine: "ai" | "local" } | { error: string };

const statuses = Object.keys(statusNames);
const diagnostics = Object.keys(diagnosticNames);
const months = "jan feb mar apr may jun jul aug sep oct nov dec".split(" ");
const monthName = "Jan(?:uary)?|Feb(?:ruary)?|Mar(?:ch)?|Apr(?:il)?|May|Jun(?:e)?|Jul(?:y)?|Aug(?:ust)?|Sep(?:t(?:ember)?)?|Oct(?:ober)?|Nov(?:ember)?|Dec(?:ember)?";
const absoluteDate = new RegExp(`\\b(?:\\d{4}-\\d{2}-\\d{2}|(${monthName})\\.?\\s+(\\d{1,2})(?:st|nd|rd|th)?(?:,?\\s+(\\d{4}))?)\\b`, "gi");

function interpretLocally(prompt: string, current: InvestigationFilters, today: string, providers: readonly string[]): FilterReply {
  let text = prompt.toLowerCase();
  const filters = { ...current };
  let recognized = false;

  if (/\b(?:clear|reset|remove) (?:all |the )?filters\b|^show all requests[.!]?$/.test(text)) {
    return { filters: {}, engine: "local" };
  }
  if (/\b(?:except|exclude|without|did not|don't|other than|not)\b/.test(text)) {
    return { error: "Try naming the statuses or diagnostics you want to include." };
  }

  for (const provider of providers) {
    const name = provider === "self_hosted" ? "self[- _]hosted" : provider;
    const pattern = new RegExp(`(?<![a-z0-9_-])(?:from\\s+)?${name}(?![a-z0-9_-])`);
    if (!pattern.test(text)) continue;
    filters.provider = provider;
    text = text.replace(pattern, " ");
    recognized = true;
    break;
  }

  const warning = /\bwarn(?:ing)?s?\b/.test(text);
  const statusMatches = [
    ["failed", /\b(?:fail(?:s|ed|ures?)?|errors?|errored)\b/],
    ["succeeded", /\b(?:succeed(?:ed)?|success(?:es|ful)?|completed)\b/],
    ["awaiting_reconciliation", /\b(?:unconfirmed|awaiting reconciliation|uncertain outcome)\b/],
    ["running", /\b(?:running|in progress)\b/],
    ["queued", /\b(?:queued|pending)\b/],
    ["canceled", /\b(?:cancel(?:l)?ed|cancellations?)\b/],
  ] as const;
  const matchingStatuses = statusMatches.filter(([, pattern]) => pattern.test(text));
  if (matchingStatuses.length > 1) return { error: "Use one request status per search. Failed or has warnings is also supported." };
  const status = matchingStatuses[0]?.[0];
  if (status || warning) {
    recognized = true;
    const either = warning && status && /\bor\b/.test(text);
    if (either && status !== "failed") return { error: "Try a single status, warnings, or failed or has warnings." };
    filters.status = either ? undefined : status;
    filters.diagnostic = either ? "failed_or_warning" : warning ? "warnings" : undefined;
  }

  const relative = text.match(/\b(?:last|past) (\d+) days?\b/);
  const dates = [...text.matchAll(absoluteDate)].map(([value, month, day, year]) => month
    ? `${year ?? today.slice(0, 4)}-${String(months.indexOf(month.slice(0, 3).toLowerCase()) + 1).padStart(2, "0")}-${day.padStart(2, "0")}`
    : value);
  if (dates.some((date) => !validDate(date)) || dates.length > 2) {
    return { error: "Use two valid dates, such as November 20 through November 25." };
  }
  if (relative) {
    const days = Number(relative[1]);
    if (days < 1 || days > 92) return { error: "Choose a UTC date range of 1 to 92 days." };
    filters.from = shiftDate(today, 1 - days);
    filters.to = shiftDate(today, 1);
    recognized = true;
  } else if (dates.length === 2) {
    filters.from = dates[0];
    filters.to = /\bbefore\b/.test(text) ? dates[1] : shiftDate(dates[1], 1);
    recognized = true;
  } else if (dates.length === 1 || /\b(?:today|yesterday)\b/.test(text)) {
    if (dates.length === 1 && /\b(?:from|since|after|before|until|between|through)\b/.test(text)) {
      return { error: "Give the start and end dates, or use a single date with “on.”" };
    }
    filters.from = dates[0] ?? shiftDate(today, /\byesterday\b/.test(text) ? -1 : 0);
    filters.to = shiftDate(filters.from, 1);
    recognized = true;
  } else if (/\b(?:from|between|during|until|since|before)\b/.test(text)) {
    return { error: "Try a date range like November 20 through November 25 or last 7 days." };
  }

  const error = dateRangeError(filters);
  if (error) return { error };
  return recognized ? { filters, engine: "local" }
    : { error: "Try a request status, warnings, a provider name, or a date range like last 7 days." };
}

type ModelSession = {
  prompt(input: string, options: { responseConstraint: object; signal: AbortSignal }): Promise<string>;
  destroy(): void;
};

type PromptModel = {
  availability(): Promise<string>;
  create(options: { signal: AbortSignal }): Promise<ModelSession>;
};

const schema = {
  type: "object",
  additionalProperties: false,
  properties: {
    understood: { type: "boolean" },
    status: { enum: [...statuses, null] },
    diagnostic: { enum: [...diagnostics, null] },
    from: { type: ["string", "null"] },
    through: { type: ["string", "null"] },
    clear: { type: "boolean" },
  },
  required: ["understood"],
};

function modelFilters(result: unknown, current: InvestigationFilters, providers: readonly string[]): InvestigationFilters | undefined {
  if (!result || typeof result !== "object") return;
  const value = result as Record<string, unknown>;
  if (value.understood !== true) return;
  if (value.clear === true) return {};
  const filters = { ...current };
  if (value.status !== undefined || value.diagnostic !== undefined) {
    if (value.status != null && (typeof value.status !== "string" || !statuses.includes(value.status))) return;
    if (value.diagnostic != null && (typeof value.diagnostic !== "string" || !diagnostics.includes(value.diagnostic))) return;
    filters.status = value.status as InvestigationFilters["status"] ?? undefined;
    filters.diagnostic = value.diagnostic as InvestigationFilters["diagnostic"] ?? undefined;
  }
  if (value.provider !== undefined) {
    if (value.provider !== null && (typeof value.provider !== "string" || !providers.includes(value.provider))) return;
    filters.provider = value.provider as InvestigationFilters["provider"] ?? undefined;
  }
  if (value.from !== undefined || value.through !== undefined) {
    if (value.from === null && value.through === null) {
      delete filters.from;
      delete filters.to;
    } else {
      if (typeof value.from !== "string" || typeof value.through !== "string"
        || !validDate(value.from) || !validDate(value.through)) return;
      filters.from = value.from;
      filters.to = shiftDate(value.through, 1);
    }
  }
  if (dateRangeError(filters)) return;
  if (value.status === undefined && value.diagnostic === undefined && value.provider === undefined
    && value.from === undefined && value.through === undefined) return;
  return filters;
}

export async function suggestHistoryFilters(
  prompt: string, current: InvestigationFilters, observedProviders: readonly string[], signal: AbortSignal,
): Promise<FilterReply> {
  const providers = providerChoices([...observedProviders, current.provider]);
  const today = new Date().toISOString().slice(0, 10);
  const model = (globalThis as typeof globalThis & { LanguageModel?: PromptModel }).LanguageModel;
  let session: ModelSession | undefined;
  try {
    if (model && await model.availability() === "available" && !signal.aborted) {
      const deadline = AbortSignal.any([signal, AbortSignal.timeout(8_000)]);
      session = await model.create({ signal: deadline });
      const output = await session.prompt([
        "Translate the user's request history question into a filter patch. Respond with the supplied JSON schema.",
        "Use only the supported fields. Omit fields the user did not mention. Use understood=false for unsupported or ambiguous requests.",
        "For failed OR warnings use diagnostic=failed_or_warning and status=null. For failed AND warnings use status=failed and diagnostic=warnings.",
        "A date range uses two inclusive UTC dates in YYYY-MM-DD and may span up to 92 days. For a single day use the same date twice.",
        `Today in UTC is ${today}. Current search: ${JSON.stringify({ status: current.status, diagnostic: current.diagnostic, provider: current.provider, from: current.from, through: current.to && shiftDate(current.to, -1) })}.`,
        `User question: ${JSON.stringify(prompt)}`,
      ].join("\n"), {
        responseConstraint: { ...schema, properties: { ...schema.properties, provider: { enum: [...providers, null] } } },
        signal: deadline,
      });
      const filters = modelFilters(JSON.parse(output), current, providers);
      if (filters) return { filters, engine: "ai" };
    }
  } catch {
    signal.throwIfAborted();
  } finally {
    if (!signal.aborted) session?.destroy();
  }
  signal.throwIfAborted();
  return interpretLocally(prompt, current, today, providers);
}
