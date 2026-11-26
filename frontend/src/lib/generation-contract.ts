import registry from "../../content/api/generation.json";

type Value = string | number;
export type GenerationField = {
  name: string;
  type: string;
  detail: string;
  required?: boolean;
  default?: string;
  values?: Value[];
};

type Primitive = {
  label: string;
  status: string;
  summary: string;
  fields: GenerationField[];
  rules: string[];
  example: Record<string, unknown>;
};

type Profile = {
  primitive: PrimitiveId;
  fields: string[];
  detail: string;
  rules: string[];
  values?: Record<string, Value[]>;
};

export type PrimitiveId = keyof typeof registry.primitives;
export const primitives: Record<PrimitiveId, Primitive> = registry.primitives;
const modelEntries: Record<string, { label: string; profile: string }> = registry.models;
const profiles = registry.profiles as Record<string, Profile>;

export function primitiveById(id: string): PrimitiveId | undefined {
  if (Object.hasOwn(primitives, id)) return id as PrimitiveId;
}

export function modelPrimitive(modality: string): PrimitiveId | undefined {
  return primitiveById(modality === "chat" ? "text" : modality);
}

const endpoints: Partial<Record<PrimitiveId, string>> = {
  image: "/v1/image",
  video: "/v1/video",
  audio: "/v1/audio",
  "3d": "/v1/3d",
  text: "/v1/chat/completions",
};

export function generationEndpoint(primitive: PrimitiveId | undefined) {
  return primitive && endpoints[primitive];
}

export function scopedMediaExample(id: PrimitiveId | undefined) {
  const examples: Partial<Record<PrimitiveId, Record<string, unknown>>> = registry.scoped_media_examples;
  return id ? examples[id] : undefined;
}

export function modelRequestExample(primitive: PrimitiveId | undefined, model: string) {
  if (primitive === "text") return { ...primitives.text.example, model, max_completion_tokens: 128 };
  const media = scopedMediaExample(primitive);
  if (media) return { ...media, model, dry_run: true };
}

export function modelContract(slug: string) {
  if (!Object.hasOwn(modelEntries, slug)) return;
  const { label, profile } = modelEntries[slug];
  const spec = profiles[profile];
  const primitive = primitives[spec.primitive];
  const fields = primitive.fields.filter(({ name, required }) => required || spec.fields.includes(name))
    .map((field) => ({ ...field, values: spec.values?.[field.name] ?? field.values }));
  return { label, profile: spec, fields };
}

export function modelsFor(primitive: PrimitiveId) {
  return Object.entries(modelEntries)
    .filter(([, entry]) => profiles[entry.profile].primitive === primitive)
    .map(([slug, { label }]) => ({ slug, label }));
}
