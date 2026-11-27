import type { ProjectScope } from "@/lib/gateway";
import type { PromptMode, Quote } from "./api";

export type ModelSubmission = Readonly<{ key: string; body: string }>;
export type ModelSubmissionOptions = {
  clientRef: string;
  appRef: string;
  subscription: string | null;
  promptMode?: PromptMode;
  prompt: string;
};

export function parseClientReference(value: string): string | undefined {
  const reference = value.trim();
  return !reference || /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}$/.test(reference) ? reference : undefined;
}

export function prepareModelSubmission(
  scope: ProjectScope, quote: Pick<Quote, "id" | "input">, options: ModelSubmissionOptions,
): ModelSubmission {
  const { clientRef, appRef, subscription, promptMode, prompt } = options;
  return {
    key: crypto.randomUUID(),
    body: JSON.stringify({
      ...scope, input: quote.input, quote_id: quote.id, client_ref: clientRef || null, app_ref: appRef || undefined,
      delivery: subscription ? { webhook_subscription_id: subscription } : undefined,
      messages: promptMode === "chat" ? [{ role: "user", content: prompt }] : undefined,
      image_prompt: promptMode === "image" ? prompt : undefined,
      video_prompt: promptMode === "video" ? prompt : undefined,
      audio_prompt: promptMode === "audio" ? prompt : undefined,
      mesh_prompt: promptMode === "mesh" ? prompt : undefined,
    }),
  };
}

export async function readModelReceipt(response: Response): Promise<string | null> {
  if (response.status !== 200 && response.status !== 202) return null;
  const id = response.headers.get("TokenCraft-Request-Id");
  if (isRequestId(id)) {
    void response.body?.cancel().catch(() => undefined);
    return id;
  }
  const body = await response.json().catch(() => null) as { id?: unknown } | null;
  return isRequestId(body?.id) ? body.id : null;
}

function isRequestId(value: unknown): value is string {
  return typeof value === "string" && /^req_[0-9a-z]{26}$/.test(value);
}
