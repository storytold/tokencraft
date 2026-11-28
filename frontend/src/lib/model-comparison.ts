import { MODELS, MODEL_BY_SLUG, type Model, type Modality } from "./catalog";
import { resultForPrompt, suiteForModel, type PromptSuite, type SharedPrompt } from "./model-examples";

const MAX_MODELS = 3;
const initialImageModels = ["google_nano_banana_2", "openai_gpt_image_1_5", "openai_gpt_image_2"];

export type ComparisonExample = SharedPrompt & {
  summary: string;
  suite?: PromptSuite;
};

export const comparisonModalities: readonly Modality[] = ["image", "video", "3d", "world", "chat"];

export function comparisonModelsFor(modality: Modality) {
  return MODELS.filter((model) => model.modality === modality);
}

export function defaultComparisonModels(modality: Modality) {
  return modality === "image"
    ? initialImageModels.map((slug) => MODEL_BY_SLUG[slug])
    : comparisonModelsFor(modality).slice(0, MAX_MODELS);
}

export function selectedComparisonModels(slugs: readonly string[]) {
  const models: Model[] = [];
  for (const slug of new Set(slugs)) {
    if (!Object.hasOwn(MODEL_BY_SLUG, slug)) continue;
    const model = MODEL_BY_SLUG[slug];
    if (models.length && model.modality !== models[0].modality) continue;
    models.push(model);
    if (models.length === MAX_MODELS) break;
  }
  return models;
}

export function comparisonExamples(modality: Modality): ComparisonExample[] {
  const model = comparisonModelsFor(modality)[0];
  const suite = model && suiteForModel(model.slug);
  const prepared = suite?.prompts.map((example) => ({ ...example, summary: suite.summary, suite })) ?? [];
  if (modality === "chat") {
    return [{
      id: "incident-summary",
      name: "Incident handoff",
      theme: "Summary Accuracy",
      criteria: "Accurate timing, user impact, and clear follow-up",
      summary: "One user message · Up to 500 output tokens",
      prompt: "Write a three-bullet handoff from these notes. At 09:15 UTC, image uploads slowed down. At 09:28, the team rolled back a configuration change. Normal upload times returned at 09:35. Existing images stayed available. The cause is still under investigation. Include the user impact, recovery, and one follow-up.",
    }];
  }
  return prepared;
}

const terms = (value: string) => new Set(value.toLowerCase().match(/[a-z0-9]{3,}/g) ?? []);

export function suggestedExample(models: readonly Model[], job = "") {
  const examples = comparisonExamples(models[0]?.modality ?? "image");
  const jobTerms = terms(job);
  return examples.map((example) => {
    const suite = example.suite;
    const coverage = suite ? models.filter(({ slug }) => resultForPrompt(suite, example, slug)).length : 0;
    const relevance = [...terms(example.name + " " + example.criteria)]
      .filter((term) => jobTerms.has(term)).length;
    return { example, coverage, relevance };
  }).sort((a, b) => b.coverage - a.coverage || b.relevance - a.relevance)[0]?.example;
}

export function modelComparisonUrl(models: readonly Model[], example?: ComparisonExample) {
  if (!models.length) return "/compare";
  const params = new URLSearchParams({ models: models.map((model) => model.slug).join(",") });
  if (example) params.set("example", example.id);
  return "/compare?" + params;
}
