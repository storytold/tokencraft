# Foundation — Frontend (mock)

A **frontend-only** build of the Foundation generative-media + LLM API gateway.
The backend is entirely stubbed — no Rust, no services — so we can see the UX and
evaluate how the seams fit before wiring anything real. Everything under
`src/lib/` (catalog, pricing, requests, auth) is mock data.

## Stack

- **Vite 6** + **React 19** + **TypeScript** (strict)
- **Tailwind CSS 3** with a CSS-variable token system
- **react-router-dom 7**, **lucide-react** icons

## Run

```bash
npm install
npm run dev        # http://localhost:3000
npm run build      # tsc + vite build
npm run typecheck
```

## Structure

The single Vite app is organized to mirror the intended Nx split so nothing has
to be undone when the real apps/libs are broken out:

```
src/
  lib/            → future shared libs
    catalog.ts    catalog-as-code: models, modalities, pricing, playground controls
    mock-api.ts   mock auth, balance, API keys, request ledger, endpoint reference
    theme.tsx     per-surface light/dark ThemeProvider
    cn.ts         class merge helper
  components/     shared UI primitives (ui.tsx) + ModelCard
  surfaces/       → future apps, one folder per surface
    marketing/    landing and developer docs (public)
    models/       catalog (gallery/table views, rank + sort), model page + gallery + playground
    rankings/     the Rankings product — overview, per-media pages, model detail
    auth/         login, signup, password reset
    dashboard/    overview, requests, keys, billing, settings (mock logged-in)
    admin/        placeholder for the internal console
```

## Model page

- **URL-synced tabs** — Overview / Playground / API / Examples / Pricing / Changelog each live at a
  hash (`/models/:slug#api`), so tabs are linkable and use browser back/forward.
- **API section** — a **10-language** code switcher (cURL, JavaScript, Python, PHP, C#, Java, Go,
  Rust, Kotlin, Swift) with **theme-aware syntax highlighting** (Prism tokenizes; token colors are
  CSS variables that flip with light/dark), a copy button, the endpoint table, and a
  **Built-for-agents CTA** (llms.txt · MCP server · coding agents). Snippets are generated per
  model (`src/lib/codegen.ts`) — chat models hit `/v1/chat/completions`, others the model-specific
  `/generate`; each uses that language's idiomatic HTTP client + raw-string/heredoc JSON body.

## Catalog index (`/models`)

- **Grid and table views** use the model creator's mark. Generated previews appear in Examples, and playground outputs appear after generation.
- **Catalog search** matches model names, slugs, creators, and capabilities. Use `Ctrl+K` or `⌘ K` to focus search from the catalog.
- **Model Advisor** opens from the button beside search and recommends models from a job description. The local fixture uses catalog capabilities and sample estimates.
- **Rank + usage sparkline** on every model in both views.
- **Sort/rank selector**: Volume (generations), Volume (output), Volume (price-weighted), Cost
  (high→low), Cost (low→high), Trending (WoW), Newest.
- Backed by `src/lib/model-stats.ts` — the frontend stand-in for the design's **hot catalog data**
  (`model_rankings_and_reports.md` §3.3 / §5 `model_usage_summary`, served as `catalog_stats.json`),
  so sorting all models is an instant client-side re-sort over one payload. Numbers are
  deterministic from the slug (dummy data). Price-weighted "value" is list-price × volume — public,
  not revenue (privacy line preserved).

### AI web research

To enable web research on a non-fixture gateway, set `TOKENCRAFT_ADVISOR_OPENROUTER_KEY` and `TOKENCRAFT_ADVISOR_MODEL` in the gateway environment. Choose a model that supports OpenRouter server tools and JSON-object output. Signed-in developers can submit a job description and receive catalog recommendations with primary-source links. The advisor uses the catalog if web research is unavailable. The gateway allows up to three concurrent requests and 60 requests per hour per process. [OpenRouter web search](https://openrouter.ai/docs/guides/features/server-tools/web-search)

Creator marks are downloaded from the official sites for [Google](https://about.google/), [OpenAI](https://openai.com/brand/), [Anthropic](https://www.anthropic.com/), [Meta](https://www.meta.com/about/), [ByteDance](https://www.bytedance.com/), [Tencent](https://www.tencent.com/), and [World Labs](https://worldlabs.ai/).

## Shared example prompts

The Examples tab shows four fixed prompts for each comparable output type. Image, video, textured mesh, and Gaussian-splat worlds each have their own suite. Each suite fixes the input type, reference count, output count, and relevant output settings. Optional provider-specific tuning uses its default. Video starts muted for a visual comparison. Add separate suites for speech, music, and sound effects when the catalog supports those tasks, with the same source text or audio instructions and a fixed output format within each suite.

Image onboarding and the Examples tab share `content/benchmark-suites/image-1.json`. The other suites are in `src/lib/model-examples.ts`. Keep the first successful result that meets the suite settings. Confirm the request, model, provider, and output before staging a result. Create a new suite revision when a prompt or comparison setting changes. Use public test inputs and approved artifacts.

### Onboard an image model

Once the same slug has a working text-to-image route in the gateway catalog and an entry in the frontend catalog, run this from the repository root with a generation API key in `TOKENCRAFT_API_KEY`.

```bash
TOKENCRAFT_BASE_URL=https://your-gateway.example \
TOKENCRAFT_CDN_ORIGIN=https://your-generation-cdn.example \
node script/onboard-image-model.mjs --model YOUR_MODEL_SLUG --max-total-usd-micros 500000
```

The command estimates the full suite before submitting jobs. The example budget is $0.50 in USD micros. It submits the four exact prompts with one image per prompt, no references, and default provider tuning. The selected model must produce 1024 × 1024 images. `TOKENCRAFT_CDN_ORIGIN` defaults to the gateway origin for deployments that serve both there.

The command writes the verified images and then atomically publishes the model’s four-entry `results.json`. It defaults to `frontend/public/cdn`. Pass `--output` to target the directory served at `/cdn` in your deployment. For object storage, deploy the staged images first, then the index. The Examples tab picks up the index on its next load without a frontend rebuild.

The runner saves job IDs in `~/.local/state/tokencraft/image-onboarding` by default. Keep that directory across runs or pass a durable private `--state-dir`. Rerunning uses the saved jobs and skips a complete published suite. If the connection dropped during admission, find the accepted job in the generation account and pass `--resume-job SCENARIO=JOB_ID`. You can also use that flag to attach a matching replacement for a failed job. The runner verifies the job’s original prompt and settings before using it.

Publish approved media under `/cdn/benchmarks/{suite}/{scenario}/{model}/`, then write a JSON array of the model’s matched run objects to `/cdn/benchmarks/{suite}/{model}/results.json`. Serve the index as `application/json`. The Examples tab loads each selected model's index when opened and replaces the placeholder for each matched result. This works without a frontend rebuild. Local static deployments can use `public/cdn/benchmarks/`. The example below shows one item in the array.

```json
{
  "suite": "image-1",
  "scenario": "product-label",
  "model": "google_nano_banana_2",
  "prompt": "COPY THE EXACT PRODUCT LABEL PROMPT FROM THE SUITE",
  "effective_settings": {
    "task": "text_to_image",
    "reference_count": 0,
    "output_count": 1,
    "width": 1024,
    "height": 1024,
    "tuning": "provider_default"
  },
  "request_id": "REQUEST_ID_FROM_THE_TRACE",
  "provider": "PROVIDER_FROM_THE_TRACE",
  "artifact": "benchmarks/image-1/product-label/google_nano_banana_2/output.png"
}
```

The catalog displays a placeholder when an image or video lacks a matching record. Records must also pass the suite’s prompt, effective settings, model, and artifact checks. Images must match the target dimensions. Video must also be within 0.15 seconds of the suite duration. For bundled examples, the app can also read individual run objects from `content/benchmarks/` when a published index is unavailable.

### Comparison placeholder assets

The comparison uses one preview image per image prompt across all models, plus a [stock flower video from MDN](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/video). The video is copied from [MDN's CC0 media directory](https://interactive-examples.mdn.mozilla.net/media/cc0-videos/flower.mp4), with a frame from the clip used as its poster. The four stills were created with the built-in ImageGen tool and are saved under `public/benchmarks/placeholders/`.

<details>
<summary>ImageGen prompts</summary>

- `product-label.webp` — Use case: product-mockup. Asset: one square TokenCraft examples preview image. A straight-on studio product photograph of one matte cobalt-blue water bottle standing on a pale stone plinth, with soft light from the left. Its white label reads ORBIT in large letters, with 500 ml beneath it. Use a warm off-white backdrop. No other objects or text. Single photograph, edge to edge, no frames, no UI, no watermark.
- `portrait.webp` — Use case: photorealistic-natural. Asset: one square TokenCraft examples preview image. An editorial waist-up portrait of an adult ceramic artist in a dark green apron, holding a small unglazed clay bowl in both hands. A workshop window lights the left side of their face. Include a softly blurred shelf in the background and keep the bowl and hands in focus. Single professional photograph, edge to edge, no text or watermark.
- `spatial-layout.webp` — Use case: photorealistic-natural. Asset: one square TokenCraft examples preview image. An overhead photograph of a cream table with exactly three objects: a red apple at the top left, a blue ceramic cup at the top right, and a single silver spoon below the apple. Leave the bottom right empty. Keep every object fully in frame. Single clean edge-to-edge photograph, no text or watermark.
- `illustration.webp` — Use case: illustration-story. Asset: one square TokenCraft examples preview image. A cut-paper illustration of a cyclist passing a small bus shelter in the rain. Use exactly four colors: navy, warm cream, muted orange, and slate blue. Show layered paper edges and a clear silhouette, with no lettering. Single edge-to-edge artwork, no frame or watermark.

</details>

## Prompt Gallery (curated per-model examples)

The model page's **Additional example prompts** section includes a **Prompt Gallery** of examples showing what a
model can do and *how each effect is achieved* (the `awesome-*-prompts` pattern, productized).

- **Content-as-code**: examples are markdown files in `content/gallery/{model}/*.md`
  (frontmatter — model, task, prompt, params, technique tags, CDN asset key, provenance
  `job_id` — plus a prose "how it's done" body). Loaded via `import.meta.glob` in
  `src/lib/gallery.ts`; in production this is the `gallery.json` compile step
  (`design/subsystems/prompt_gallery.md`).
- **Assets** are fake images (`ffmpeg`-generated abstract art) and short videos (mp4 + poster)
  under `public/cdn/samples/...`, referenced by CDN object key exactly as the design specifies.
- **Model-specific categories**: examples are grouped into sections that match how each model is
  used — Nano Banana: *Photorealistic · Stylized · Editing · Multi-reference composition · Text &
  branding*; Seedance: *Text-to-video · Image-to-video · Reference-to-video · Camera control*.
- **Reference images** (the Seedance-guide pattern): edits show the **input → result**,
  image-to-video shows the source still, and multi-reference composition shows each labeled
  reference (e.g. *Product* + *Scene*) → result. Cards carry an input-thumbnail strip; the
  lightbox lays out references → result.
- **UI**: technique filter, hover-to-play video previews, lightbox with full prompt/params/prose,
  **"Remix in playground"** (loads the example's params into the form), and deep-linkable SEO
  example pages at `/models/:slug/gallery/:exampleId`.
- Prototyped for **Nano Banana 2** (13 image examples inc. edits + composition) and **Seedance
  2.0** (9 video examples inc. i2v + reference-to-video). Assets are `ffmpeg`-generated, with
  before/after edit pairs derived via filters.

The playground also has a **request history** panel (like Fal): recent runs for the model with a
scope toggle — **My runs** vs **All account traffic** (everything that transited the account for
this model, including customers' end-users via `end_user`).

## Rankings (a headline feature)

`/rankings` is a full generative-media usage index — our answer to (and a superset of)
OpenRouter's LLM Rankings. Eight **verticals**: Image and Video first-class, then Language
(LLM), Speech, Transcription, Music, 3D, World. Data lives in `src/lib/rankings.ts`, shaped
like the design's rollup **cubes** (`design/core/model_rankings_and_reports.md` §5) so wiring
to `/v1/public/rankings*` later is a data-source swap, not a rebuild. Every number is derived
deterministically from a seed hash (no `Math.random`) so it renders identically on every reload.

OpenRouter-grade visuals:

- **Weekly stacked-bar chart** (top models over ~16 weeks, hoverable per-week breakdown) — the
  signature hero on the overview and every vertical page, with a media/metric switcher.
- **Week-over-week %** delta column on every leaderboard row, plus a 14-day sparkline.
- **Stacked-area** day-by-day share-over-time; **donut** market share by model and by lab.

Pages:

- `/rankings` — overview; stacked-bar hero, image & video leaderboards front and center,
  market-share donuts, movers of the week, vertical quick-nav.
- `/rankings/:media` — per-vertical page with a metric switch (e.g. video: total duration /
  seconds / requests) and sub-tabs **Leaderboard · By task · By category · By language\* ·
  Market share · Trends**. Image tasks: generate / edit / infill / style / upscale / background.
  Video categories: dialogue scene / action scene / b-roll / … (\*language tab for LLM / Speech /
  Transcription — programming languages like **Rust** + natural languages).
- `/rankings/models/:slug` — model detail: rank, share, WoW, trend, top tasks, top content
  categories, top languages, top apps, success rate, p50/p95 latency.

## Theming

Each **surface** owns its own day/night choice (persisted per surface in
`localStorage`). `ThemeProvider surface="…"` toggles a local `.dark` class, and
the token variables in `index.css` flip only within that wrapper. So marketing
can be light while the dashboard is dark, independently.

## Endpoints modeled (see `/docs` in-app and `mock-api.ts`)

- **Model-specific** — `POST /v1/models/{model}/generate[/{modality}]`, pins an exact model+version.
- **Modality** — `POST /v1/any/{image|video|3d|world}`, routes to the best (or named) model.
- **Cost twins** — every generate endpoint has a `…/cost` sibling that prices a request without running it.
- **Language** — OpenAI-compatible `POST /v1/chat/completions` (+ `/cost`).
- **Catalog / model pages** — public `GET /v1/models`, `/v1/models/{model}`, `/changelog`, `/examples`.

## Cost structure

Metering units: `per_image`, `per_video_second`, `per_mesh`, `per_splat`,
`per_million_tokens_in/out`. Requests draw down prepaid USD credits; tax is added
at top-up; invoices are numbered gaplessly. Details on the in-app `/docs` page.

## End-user attribution (flagged for the architecture docs)

Every request can carry an `end_user` identifier — the customer-of-our-customer.
The dashboard's **Requests** view has two scopes:

- **My requests** — what the logged-in user ran in the dashboard.
- **All account traffic** — *everything that transited the account*, including
  traffic your customers' end-users generated through your integration.

This is the piece the design docs still need to document; see the note at the top
of `src/lib/mock-api.ts`.
