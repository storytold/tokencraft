import type { Provider } from "@/lib/gateway";

export const providerLabels: Record<Provider | "other", string> = {
  fal: "fal",
  kinovi: "Kinovi",
  openrouter: "OpenRouter",
  self_hosted: "Self-hosted",
  other: "Other provider",
};

export const workloadLabels = {
  image: "Image",
  video: "Video",
  audio: "Audio",
  text: "Text",
  mesh: "3D",
  other: "Other workload",
} as const;

export function latency(milliseconds: number | null) {
  if (milliseconds === null) return "—";
  if (milliseconds < 1_000) return `${milliseconds.toLocaleString()} ms`;
  return `${(milliseconds / 1_000).toLocaleString(undefined, { maximumFractionDigits: 1 })} s`;
}

export function age(at: string, observedAt: string) {
  const seconds = Math.max(0, Math.floor((Date.parse(observedAt) - Date.parse(at)) / 1000));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}
