import type { AudioCapability, ModelCatalog, QuoteInput, VideoCapability } from "@/surfaces/comparison/api";

export const modelNames: Record<string, string> = {
  studio_image_demo: "Studio Image",
  studio_chat_demo: "Studio Chat",
  studio_text_chat: "Studio Chat",
  studio_flux_image: "FLUX Image",
  studio_video_demo: "Studio Video",
  studio_seedance_20: "Seedance 2.0",
  studio_audio_demo: "Studio Audio",
  studio_mesh_demo: "Studio 3D",
};

export function catalogModelName(id: string) {
  return Object.hasOwn(modelNames, id) ? modelNames[id] : undefined;
}

export type Modality = QuoteInput["workload"]["kind"];

export const modalityNames: Record<Modality, string> = {
  text: "Text", image: "Image", video: "Video", audio: "Audio", mesh: "3D",
};

export type ModelChoice = {
  model: string;
  kind: Modality;
  aspectRatios: string[];
  video?: VideoCapability[];
  audio?: AudioCapability[];
  meshFormats?: string[];
};

export function modelChoices(catalog: ModelCatalog | null) {
  const choices = new Map<string, ModelChoice>();
  for (const model of catalog?.models ?? []) {
    const base = { model: model.id, aspectRatios: model.aspect_ratios };
    if (model.image) choices.set(`${model.id}/image`, { ...base, kind: "image" });
    if (model.text) choices.set(`${model.id}/text`, { ...base, kind: "text" });
    if (model.video?.length) choices.set(`${model.id}/video`, { ...base, kind: "video", video: model.video });
    if (model.audio?.length) choices.set(`${model.id}/audio`, { ...base, kind: "audio", audio: model.audio });
    if (model.mesh_formats?.length) choices.set(`${model.id}/mesh`, { ...base, kind: "mesh", meshFormats: model.mesh_formats });
  }
  const order = Object.keys(modalityNames);
  return Array.from(choices).sort((a, b) => order.indexOf(a[1].kind) - order.indexOf(b[1].kind));
}

export function defaultImageAspectRatio(ratios: readonly string[]) {
  return ratios.includes("1:1") ? "1:1" : ratios[0] ?? "1:1";
}

export function validAspectRatio(value: string) {
  if (!/^[0-9]{1,3}:[0-9]{1,3}$/.test(value)) return false;
  return value.split(":").every((part) => Number(part) >= 1 && Number(part) <= 100);
}

export const presetLabels = { video: "Video preset", audio: "Audio preset", mesh: "3D format" };

export function modelPresets(choice?: ModelChoice) {
  const presets = new Map<string, Extract<QuoteInput["workload"], { kind: keyof typeof presetLabels }>>();
  for (const capability of choice?.video ?? []) {
    for (const duration of capability.durations_seconds) {
      for (const ratio of capability.aspect_ratios) {
        const label = `${duration}s · ${capability.resolution} · ${ratio}`;
        presets.set(label, { kind: "video", duration_seconds: duration, aspect_ratio: ratio, resolution: capability.resolution });
      }
    }
  }
  for (const capability of choice?.audio ?? []) {
    for (const duration of capability.durations_seconds) {
      presets.set(`${duration}s · ${capability.format.toUpperCase()}`, { kind: "audio", duration_seconds: duration, format: capability.format });
    }
  }
  for (const format of choice?.meshFormats ?? []) {
    presets.set(format.toUpperCase(), { kind: "mesh", format });
  }
  return Array.from(presets);
}
