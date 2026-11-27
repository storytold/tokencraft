# Run TokenCraft with your providers

The headless gateway owns project keys, catalog offers and quotes, routing, budgets, idempotency, durable request state, artifacts, usage, and outbound webhooks. Operators configure a catalog and choose the services that execute its offers. A connector owns the upstream API, model mapping, provider credentials, quota behavior, and translation to TokenCraft’s protocol.

## Attach an implementation

Start with the [provider directory](../plugins/providers/README.md) for the fal, Kinovi, and OpenRouter plugins or [configure a cell's compute endpoints](../plugins/providers/compute/README.md).

Start with the [Rust example](setup.md) to exercise a complete request before configuring an upstream.

Use the [mapping guide](provider-mapping.md) to draft translations from provider docs, preview both directions, and apply a reviewed mapping inside your connector.

1. Implement the HTTP interface below, or run an OpenAI-compatible chat endpoint. The connector can run from its own repository, deploy independently, and translate to one or more upstream services.
2. Put offers in a file named by `TOKENCRAFT_CORE_CATALOG`. Copy the [image catalog](../examples/connector/catalog.json) and change the model, offer ID, pricing, and capabilities. Choose a lowercase provider ID, such as `my-provider`. Use `adapter: "connector_queue"`, a `local_binding` name, and the connector’s `upstream_model` alias. One provider can have several offers. Routing policies and dispatch controls use the configured provider ID.
3. Set `TOKENCRAFT_CONNECTORS` to a file like the [example binding registry](../examples/connector/bindings.json). Set each referenced `TOKENCRAFT_CONNECTOR_TOKEN_…` environment variable on the gateway and configure its matching bearer token on the connector. Use a per-binding service token. Keep the provider’s own API key in the connector’s deployment.
4. Apply the migrations with `admin-cli migrate`, then start `api-gateway` with the catalog and the same persistent `TOKENCRAFT_DEMO_PAYLOAD_KEY` across all API and worker replicas. Set a persistent session secret for the API role. For separate processes, set `TOKENCRAFT_PROCESS_ROLE` to `api` or `worker` and use matching catalog and binding configuration.

The development profile uses a loopback PostgreSQL connection and HTTP listener. Follow the [hosted deployment guide](deployment.md#run-behind-an-https-ingress) to run the same catalog and connector protocol with verified PostgreSQL TLS and a private listener behind HTTPS ingress. A connector origin may be HTTPS, or HTTP on a literal loopback IP for local sidecars. Domains, paths, and headers supplied by tenants never select that origin.

For text, configure an `openai_compatible_chat` offer and `TOKENCRAFT_LOCAL_CHAT_BINDINGS`. The gateway uses the existing OpenAI-compatible JSON and SSE transport. A separate service can translate requests to any upstream chat API and inject its own credentials. For a local binding, use `{"version":1,"bindings":[{"name":"chat","endpoint":"http://127.0.0.1:4022/v1/chat/completions","key_env":"TOKENCRAFT_LOCAL_CHAT_KEY_CHAT"}]}` and set the matching catalog offer’s `local_binding` to `chat`. Core chat uses token usage and operator-configured rates.

Traced requests carry a gateway-generated `traceparent` header on queue operations and compatible chat calls. Authenticate the gateway with the service bearer token before using its trace context. A connector can attach local diagnostics to that parent. Keep the context at the connector boundary when calling a vendor. [Request diagnostics](request-diagnostics.md#follow-a-request-into-a-connector)

## Curate your own catalog

Your gateway loads the offers in your local `catalog.json`, including fully custom models. It can start and route entirely from that file and its bindings. Keep it in operator-controlled storage and review changes before deployment. You can add ArtCraft presets later using the same file as the import base.

ArtCraft can publish its plugin-backed model capabilities and supplier list prices to a public static registry. Your gateway's active offers, local model names, project access, and quote rates come from your deployment catalog and reviewed rate feed. Importing a preset requires a local binding and explicit local rates. You can operate the gateway offline from the ArtCraft registry, including when you run only custom models.

To run a custom image model, copy the [image catalog](../examples/connector/catalog.json) and set the local `model`, offer `id`, `provider`, and `local_binding`. Set `upstream_model` to the alias your connector accepts. Specify your aspect ratios and price in USD nanodollars per image. Several offers can use the same local model ID when they fulfill the same workload and you want TokenCraft to compare them.

For a model intended for one organization, give it a dedicated connection and add that connection to `connection_access` in the catalog. Replace the example account and project values with those returned by your gateway.

```json
{
  "connection_access": {
    "studio/connector_queue/studio-images": {
      "account": "YOUR_ACCOUNT_ID",
      "projects": ["YOUR_PROJECT_ID"]
    }
  }
}
```

The key joins the offer's provider, adapter, and binding name with slashes. The connection and all its offers are scoped to that account. Set `projects` to `null` to share the connection account-wide. Without a `connection_access` entry, the gateway exposes the offer in its public catalog. Check scoped discovery with a project key before using the model in a client. [Project model discovery](#find-models-within-a-project) · [Binding preflight](#check-catalog-bindings)

## Prepare offers from the ArtCraft catalog

Use the [version 1 JSON snapshot in Core](../catalog/artcraft-v1.json) or download `/catalog/artcraft-v1.json` from the built console. Choose the reference offers you want and prepare a selection file. Copy the snapshot's revision and use the names of your local connector bindings. The sample rates here illustrate operator pricing.

```json
{
  "version": 1,
  "reference_revision": "2026-11-26T09:38:37Z",
  "offers": [
    {
      "reference": "fal_sample",
      "binding": "my-fal",
      "rates": { "usd_nanos_per_image": 30000000 }
    },
    {
      "reference": "openrouter_gpt_41_mini",
      "binding": "my-openrouter",
      "model": "studio_chat",
      "rates": {
        "usd_nanos_per_1000_input_tokens": 400000,
        "usd_nanos_per_1000_output_tokens": 1600000
      }
    }
  ]
}
```

```sh
api-gateway catalog prepare --reference artcraft-v1.json --selection selection.json \
  --base catalog.json > catalog.next.json
```

Omit `--base` to create a new catalog. The command writes the validated catalog to stdout. Choose a separate output path and point `TOKENCRAFT_CORE_CATALOG` at the reviewed file when ready. Use an optional `reference_sha256` field to pin the exact bytes of the saved snapshot. Use `id` and `model` to choose local names. Give each new offer a distinct local ID.

Set explicit USD nanodollar rates in the gateway's billing units. Image offers use `usd_nanos_per_image`, video and audio use `usd_nanos_per_second`, 3D uses `usd_nanos_per_mesh`, and chat uses both rates per 1,000 tokens. Supplier list prices stay in the reference snapshot with their original terms. A supplier may charge per megapixel, per generation, or in credits. Use an operator rate that accounts for the actual provider contract.

The prepared file includes each selected plugin's upstream model and supported capabilities. Supply the separate queue or chat binding named in the selection when deploying. Configure the OpenRouter plugin's model allowlist for the upstream model. The import command runs against local files and requires neither the database nor provider access.

Use the plugin's binding example to add each selected alias to your queue or chat registry. Review the endpoints and the environment variable names used for credentials. If your console exposes **Pricing → ArtCraft catalog**, its **Plugin binding templates** can generate these files for the selected aliases. Check the merged files before starting the gateway. [Binding preflight](#check-catalog-bindings) · [Profile setup](setup.md#start-with-a-reviewed-catalog)

Imported offers include an `artcraft_reference` with the original offer ID, catalog revision, and snapshot SHA-256. When import verifies a signature, it also records the publisher key's SHA-256. The native quote includes this record on the candidate, and the admitted request retains it even if the gateway catalog later changes. This identifies the source preset. The estimate uses your configured rate or a reviewed rate feed, whose source and validity window appear separately as `rate_evidence`.

### Check catalog bindings

Run the binding preflight with the reviewed candidate and the same binding files and service credential environment you plan to deploy. Set only the file variables that apply to your catalog.

```sh
TOKENCRAFT_CONNECTORS=./connector-bindings.json \
TOKENCRAFT_LOCAL_CHAT_BINDINGS=./chat-bindings.json \
  api-gateway catalog check-bindings --catalog ./catalog.next.json
```

The command uses the gateway's catalog and binding loaders. It checks that every queue and chat offer resolves to a configured binding, validates service credential formats and endpoint rules, and checks compute placement against offer policy. It exits nonzero on invalid configuration. The JSON report gives the binding-pinned catalog revision before rate feeds apply, local offer assignments, and whether each binding uses tenant keys. It omits endpoint URLs and credential values. The preflight accesses local configuration only. Submit a test request to confirm provider connectivity. Use the rollout check to protect outstanding work. [Connector rollout](deployment.md#check-a-connector-rollout)

### Find models within a project

`GET /v1/catalog/models` lists public model IDs and capabilities. `GET /v1/catalog/models/scoped` adds the custom offers shared with the caller's project and groups capabilities after applying its provider allowlists and connection exclusions. Sessions supply `account_id` and optionally `project_id`. Project API keys use their own scope and require `requests:submit`. The scoped response is private and cannot be stored by a cache.

Use `GET /v1/catalog/models/scoped` to populate an application's model picker, then `POST /v1/quotes/scoped` to validate a specific workload, check connector availability, and compare current rates. Public model reviews are available for models in the public catalog.

Use a shared local model ID for equivalent offers when you want TokenCraft to compare providers. For example, a local five-second video model can have separate fal and Kinovi offers. Each offer keeps its own upstream model, binding, capabilities, and price. The scoped quote includes the estimates and the selected offer. By default it selects the lowest priced eligible offer. Request and account preferences can change the order within the project's limits. [Routing policy](deployment.md#routing-policy-precedence)

Compare starts image requests with an aspect ratio from the model's scoped catalog and video requests with the selected preset's ratio. Choose a catalog suggestion or type a custom ratio. Ratio matching uses the project policy by default, and the quote shows the applied ratio for each eligible offer.

If one project key serves several customers, include the same opaque `client_ref` when quoting and submitting. Filter subsequent request and usage reads by that reference. Issue a client-pinned key when a caller should only be able to read or submit for one client. [Request diagnostics](request-diagnostics.md)

### Adopt a new preset revision

Use the existing local offer ID, its recorded `artcraft_reference.sha256`, and the new snapshot's revision and SHA-256 in the selection file.

```json
{
  "version": 1,
  "reference_revision": "2026-11-26T23:00:00Z",
  "reference_sha256": "REPLACE_WITH_NEW_SNAPSHOT_SHA256",
  "offers": [
    {
      "id": "studio_image_fal",
      "reference": "fal_sample",
      "update_from_sha256": "REPLACE_WITH_INSTALLED_SNAPSHOT_SHA256"
    }
  ]
}
```

```sh
api-gateway catalog prepare --base catalog.json --selection update.json \
  --bundle artcraft.next.bundle.json --public-key artcraft-public.der > catalog.next.json
```

The resulting offer retains its local IDs, bindings, rates, configured capabilities, and data policy. The new preset must cover those capabilities on the same provider, adapter, and upstream model. Any contract change requires an operator to review the local configuration before adoption. A selection containing only updates can be reapplied safely. A previously verified import requires a verified update. To rotate the signing key, pin the new fingerprint in `reference_public_key_sha256` and supply the matching trusted key.

### Compare catalogs before deployment

```sh
api-gateway catalog review --base catalog.json --candidate catalog.next.json
```

The version 1 JSON report includes both normalized catalog file revisions and the exact before and after values. Change paths use local offer IDs. A price change may appear at `["offers", "studio_image_fal", "pricing", "usd_nanos_per_image"]`. Each value uses the gateway's configured billing units. An added or removed entry appears at its offer path with one side present.

For a deployment that only adopts ArtCraft presets, add `--require-preset-only`. The command succeeds for an unchanged catalog or changes to the revision, checksum, and signer of an existing preset. It exits with an error when the candidate also changes a local offer, price, binding, or shared setting. Use `catalog prepare` first to verify the chosen snapshot and compatibility.

If you deploy a rate feed, use `catalog rebase-rate-feed` to prepare the matching candidate feed from the current one. [Rate feed deployment](deployment.md#carry-reviewed-rates-into-a-preset-update)

### Verify a signed ArtCraft catalog

Get the publisher's Ed25519 public key through a channel you already trust. Use its SubjectPublicKeyInfo DER file with a downloaded catalog and detached signature.

```sh
api-gateway catalog verify-reference --reference artcraft-v1.json \
  --signature artcraft-v1.signature.json --public-key artcraft-public.der
```

The report includes `reference_revision`, `reference_sha256`, and `public_key_sha256`. To verify on every import, pass the same `--signature` and `--public-key` arguments to `catalog prepare`. Set `reference_public_key_sha256` in the selection file to require that exact verified key. Keep `reference_revision` and, when you need to pin exact bytes, `reference_sha256` in the selection. Retain the catalog, signature, and verification report with the reviewed operator change.

### Sign a reference catalog for publication

Keep the reference snapshot in [Core's catalog directory](../catalog/artcraft-v1.json). Review supplier prices and billing terms before releasing a new revision. Check the snapshot against the versioned catalog examples from the provider plugin releases you plan to support. The check uses the same catalog parser as the gateway and matches each preset by provider, adapter, upstream model, and capabilities. It also validates the public price fields. The JSON report records the exact snapshot and plugin catalog hashes. An offer can have an empty rate list when pricing is unconfirmed.

```sh
api-gateway catalog check-reference --reference artcraft-v1.json \
  --plugin-catalog fal.catalog.json \
  --plugin-catalog kinovi.catalog.json \
  --plugin-catalog openrouter.catalog.json
```

Use a dedicated Ed25519 signing key on the publisher's trusted machine. OpenSSL can produce the unencrypted PKCS#8 DER key and its corresponding public file.

```sh
umask 077
openssl genpkey -algorithm ED25519 -outform DER -out catalog-signing.der
openssl pkey -inform DER -in catalog-signing.der -pubout -outform DER -out artcraft-public.der
api-gateway catalog sign-reference --reference artcraft-v1.json \
  --plugin-catalog fal.catalog.json \
  --plugin-catalog kinovi.catalog.json \
  --plugin-catalog openrouter.catalog.json \
  --private-key catalog-signing.der > artcraft-v1.signature.json
```

Including the plugin catalogs repeats the publication checks on the bytes being signed. Keep the report with the release. The check reads local files. Verify provider pricing and test each plugin separately before signing.

Publish the exact catalog bytes and matching signature together. Use the catalog SHA-256 in each immutable release path and retain earlier pairs for pinned consumers. Distribute the trusted public key separately from the download pair and keep the private key on the signing machine. Consumers updating a pinned key must obtain and verify the new key through their trusted channel.

The version 1 signature file stores the Ed25519 signature in hexadecimal. The signed message is the ASCII string `TokenCraft ArtCraft reference catalog v1`, one newline byte, then the unmodified JSON bytes. `public_key_sha256` hashes the DER public key file. The signed reference identifies the published offer definitions. The operator still chooses its local bindings and routing prices. [Ed25519 key encoding](https://www.rfc-editor.org/rfc/rfc8410.html)

### Distribute a pinned catalog bundle

Stage a reviewed snapshot and its detached signature in the local directory you plan to serve at `/releases/`. Supply the versioned catalog examples for the provider plugins you intend to support. Staging verifies the signature and checks the public pricing fields and plugin coverage before writing any files.

```sh
api-gateway catalog stage-reference --reference artcraft-v1.json \
  --signature artcraft-v1.signature.json --public-key artcraft-public.der \
  --plugin-catalog fal.catalog.json --plugin-catalog kinovi.catalog.json \
  --plugin-catalog openrouter.catalog.json \
  --directory releases
```

The JSON report includes `catalog_path` for the public JSON at `<reference_sha256>/catalog.json`, `release_path` for the signed bundle at `<reference_sha256>/<public_key_sha256>.json`, and `current_path` for discovery at `current/<public_key_sha256>.json`. The report's `plugin_coverage` gives the exact plugin catalog hash that backs each published offer. The command updates the current bundle after checking both immutable files. Keep earlier immutable files available for pinned consumers.

Use a new revision label whenever the snapshot bytes change. Staging records each label's checksum in the registry's `revisions/` directory and rejects conflicts before it changes the current bundle. Keep that directory with your staging files. On the first run against an earlier staging directory, the command checks the existing immutable catalogs for conflicts before recording the index. The same snapshot can be staged again or signed by a rotated publisher key.

Publish the immutable files before updating current paths on the static host. Serve the JSON without authentication and configure current bundle paths to revalidate on fetch. Each signing key has its own current bundle. Clients that need signature verification can fetch the bundle using their trusted key.

A reference snapshot supports up to 4,096 offers and 4 MiB of JSON. Signed transport bundles support up to 6 MiB. An operator can use a large reference to select a few entries. The active gateway file supports up to 64 offers and 64 KiB, including any custom entries the operator retains.

Fetch the registry's current snapshot with the publisher key you already trust. Review the verified revision and checksum before selecting the offers to adopt. For a registry using a private certificate authority, pass its root PEM with `--ca`.

```sh
api-gateway catalog fetch-reference --registry https://catalog.example.com/releases/ \
  --current --public-key artcraft-public.der > artcraft.bundle.json
api-gateway catalog verify-reference --bundle artcraft.bundle.json --public-key artcraft-public.der
```

Use the verified checksum to fetch the same immutable revision again. Put that revision and checksum in your selection file.

```sh
api-gateway catalog fetch-reference --registry https://catalog.example.com/releases/ \
  --sha256 "$REFERENCE_SHA256" --public-key artcraft-public.der > artcraft.bundle.json
api-gateway catalog prepare --bundle artcraft.bundle.json --public-key artcraft-public.der \
  --selection selection.json --base catalog.json > catalog.next.json
```

Both fetch modes verify the signature with the supplied key. The pinned mode also checks the requested snapshot hash. `prepare` verifies the saved bundle again and honors the selection's revision and key pins. The `reference_base64` field preserves the signed JSON byte for byte. Use `pack-reference` to produce an individual bundle for offline distribution.

CI rehearses this flow with a temporary publisher key and a loopback HTTPS registry. Run the same check from the repository root with `python3 -B scripts/smoke_reference_catalog.py target/debug/api-gateway`. It verifies plugin coverage, pinned access to an older release, and preset updates that preserve the operator's local offers, bindings, and prices.

### Update the public pricing page

Use the file at `catalog_path` in the checked release staging directory as the browser download. If you only have the bundle, recover the same bytes with the trusted publisher key and the immutable bundle path from `stage-reference`.

```sh
api-gateway catalog verify-reference --bundle "$IMMUTABLE_BUNDLE" \
  --public-key artcraft-public.der --emit-catalog > artcraft-v1.next.json
```

Use the verified JSON as the release's `catalog/artcraft-v1.json`. The console build serves that file byte for byte at `/catalog/artcraft-v1.json`. CI checks the catalog's plugin coverage and compares it with the console build's output. Publication checks reject extra catalog, offer, and billing fields. The signed bundle and the browser download share one revision and checksum. Use the gateway's quote API to compare active offers and rates for a specific workload.

On **Pricing → ArtCraft catalog**, search by model, provider, modality, or offer ID. The price table shows up to 50 rows per page. Large offer lists in **Prepare offers** can be searched before you add a draft. The browser checks the same 4 MiB and 4,096-offer limits as the CLI and uses the full snapshot for the downloaded JSON and selection checksum.

## Apply a provider review bundle

A gateway administrator can use **Dashboard → Providers** to prepare a plugin or custom connection and choose all or selected projects. For a custom connection, enter a **Provider ID** to group its offers, dispatch settings, requests, and usage under a stable identifier. Use 1–64 lowercase letters, digits, underscores, or hyphens, starting with a letter. Leave the field blank to use `self_hosted`. The console exports a patch with the changed offers and the initial organization and project access. Apply the bundle against the reviewed catalog file.

```sh
api-gateway catalog apply-provider-patch \
  --base /path/to/current-catalog.json \
  --bundle /path/to/provider-change.json > /path/to/candidate-catalog.json
```

The command verifies the base and proposed file revisions and writes the merged catalog to stdout. Set up the matching connector binding and its environment secret before deploying the candidate. Scoped quotes include connections shared with the project. Submit-authorized callers can also list project-visible model and provider IDs with `GET /v1/catalog/models/scoped`. The Compare page uses that list for request-level provider priority. Organization admins can review sharing in the provider inventory. A project can exclude a shared connection under **Project Settings → Provider exclusions**. Both provider allowlists continue to apply.

## Accept organization and project keys for chat

Use a separate direct chat binding for tenant-supplied keys. Set `tenant_keys: true` and supply a `revision` in the binding registry. Use an independent service bearer token of at least 32 characters in the variable named by `key_env`. The token authenticates TokenCraft to the connector. Use a loopback address for a same-host sidecar or an HTTPS endpoint on the private network for a remote connector.

```json
{
  "version": 1,
  "bindings": [{
    "name": "openrouter-tenant",
    "endpoint": "http://127.0.0.1:4022/v1/chat/completions",
    "key_env": "TOKENCRAFT_LOCAL_CHAT_KEY_OPENROUTER",
    "tenant_keys": true,
    "revision": "v1"
  }]
}
```

Set the catalog offer’s `local_binding` to that name. Generate a separate key with `openssl rand -hex 32` and set `TOKENCRAFT_PROVIDER_CREDENTIAL_KEY` on the API and workers. Back it up alongside the database. The gateway encrypts provider credentials and exposes only metadata. Use `GET /v1/provider-credentials/{binding}?account_id=…` as an organization admin. Add `project_id=…` to inspect or edit one project. The same path accepts `PUT` with `{"expected_version":0,"key":"<upstream key>"}` for a first key. Send the latest version on subsequent writes. Revoke a version with `DELETE /v1/provider-credentials/{binding}/{version}` and the same query.

Organizations manage credentials for the connections visible to their account. A project can add a key or view an inherited default while the connection is shared and enabled for that project. An administrator can still revoke a previously stored version in its original scope after the connection's access changes.

In the console, expand **Provider credentials** under **Providers** for an organization default, or open **Project Settings → Provider exclusions** for a project override. Choose the binding and credential scope. Enter a key to save or rotate it. The panel shows key versions and states and asks for confirmation before revocation.

The organization key is the default for projects. A project override takes precedence. Choose **Use organization default** in the console or send `{"expected_version":2,"inherit":true}` through the API to return a project to its active organization key. A revoked override makes the offer unavailable until an organization admin saves another key or selects inheritance. Scoped quotes exclude unavailable bindings.

Each admitted request pins its credential scope, version, and binding identity. Rotation keeps the old key available to jobs already pinned to it, then clears its ciphertext once those jobs reach a terminal state. Revocation clears the ciphertext immediately and prevents dispatch from acquiring the key. Quiesce unfinished work before changing the binding identity. The worker sends the pinned upstream key in `X-TokenCraft-Upstream-Key` only to the configured connector. The connector must authenticate the service bearer first, omit the header from logs, and forward the key solely to its upstream API using that API’s authentication scheme.

## Accept organization and project keys for queued media

Set `"tenant_keys": true` on a new queue binding in `TOKENCRAFT_CONNECTORS` and give it an independent service bearer token of at least 32 characters. Use the same credential vault key, admin API, and **Providers → Provider credentials** settings as for chat. Give tenant chat and media bindings distinct names. Administrators can inspect or revoke an earlier media credential after detaching its binding by passing `adapter=connector_queue` in the credential request query.

TokenCraft sends the pinned key on submission, polling, and artifact requests. The connector saves a per-job key tag and compares it on later calls. Keep the key header out of proxy and connector logs. Authenticate upstream API requests with the key. The connector fetches outputs from reviewed artifact URLs. The completion notification and the request-to-handle lookup authenticate with their own grants.

Rotation keeps the original key available for a scheduled submission retry, polling, artifact downloads, and a permitted recheck. New requests use the active key. Revoking the pinned version before dispatch blocks submission, including a scheduled retry after a confirmed throttle. The gateway checks the pinned key before each poll and each artifact download. A call already authorized may finish. If another call needs a revoked key, the gateway stops and records an uncertain outcome, retaining the original job handle and any observed charge. Rolling an existing queue binding to tenant keys changes its identity. Preserve the previous instance and binding in drain mode until their jobs finish.

## Rotate a connector service token

The fal, Kinovi, and OpenRouter plugins can accept an old and new gateway token during a rollout. Start the plugin with its normal service token variable set to the new token. On the plugin, set `TOKENCRAFT_PREVIOUS_SERVICE_TOKEN` to the old token and `TOKENCRAFT_PREVIOUS_SERVICE_TOKEN_EXPIRES_AT` to a UTC deadline, for example `2026-11-25T21:30:00Z`.

Roll the API and workers with the new value in the environment variable already named by the binding. Keep the name, origin, revision, and mode of the binding stable. Requests already admitted retain their binding identity and pinned provider credential. After all gateway processes use the new token, remove the two overlap variables from the plugin. The plugin also rejects the old service token automatically at the deadline.

## Queued media protocol, version 1

Each request authenticates with `Authorization: Bearer <binding token>`. JSON replies use HTTP 200 and `Content-Type: application/json`. The gateway bounds response size and time, and uses no HTTP redirect, proxy, or automatic transport retry. The following paths are relative to the operator-configured origin.

### Submit

`POST /v1/jobs` carries an `Idempotency-Key` equal to `request_id`.

```json
{
  "version": 1,
  "request_id": "req_example",
  "model": "example-image",
  "input": {
    "kind": "image",
    "prompt": "A colored square",
    "count": 2,
    "aspect_ratio": "1:1"
  }
}
```

| Input kind | Additional fields following `prompt` | Artifact type |
| --- | --- | --- |
| `image` | `count` from 1 to 8, `aspect_ratio` | PNG |
| `video` | `duration_seconds`, `aspect_ratio`, `resolution` such as `720p` | MP4 |
| `audio` | `duration_seconds`, `format: "wav"` | PCM WAV |
| `mesh` | `format: "glb"` | GLB with embedded geometry |

The gateway sends the aspect ratio applied by the quote. A video resolution is the short edge, from 240 to 2160 pixels. The gateway derives the long edge from the ratio, rounds down to an even pixel count, and caps it at 4096. Results must match the selected workload and pass the gateway’s format and size checks.

Echo the request ID and return one outcome.

```json
{
  "version": 1,
  "request_id": "req_example",
  "outcome": {"status": "accepted", "handle": "job_connector_123"}
}
```

- `accepted` requires a stable handle of 1–128 ASCII letters, digits, underscores or hyphens. Assign a distinct handle to each request within the binding and persist the mapping before replying. Identical replay returns the same handle. If another request already owns a returned handle, the new request enters reconciliation without polling that handle or submitting another job.
- `rejected` means the connector definitively declined the request and no upstream work was accepted.
- `unavailable` means the connector can prove no upstream work was accepted. It may supply `retry_after_ms` up to 2000 for one bounded retry.
- `uncertain` means the request may have been accepted. Timeouts, malformed replies, and non-protocol HTTP errors also leave the request awaiting reconciliation without a fresh submission.

The fal and Kinovi plugins close unused supplier response bodies promptly when an upstream response fails. They persist the cooldown after a supplier throttle. A matching replay after that delay can make the one permitted supplier retry. A retry without a conclusive outcome remains in reconciliation. A fresh quote during a recorded cooldown can select another eligible offer.

### Observe and fetch

- `GET /v1/jobs/{handle}` returns `{"version":1,"handle":"job_connector_123","status":"succeeded"}`. Valid statuses are `queued`, `running`, `succeeded`, and `failed`. Return the same handle in every successful reply and use `failed` only for a definitive terminal failure.
- When polling includes `X-TokenCraft-Provider-Charge: 1`, a `succeeded` response may include `"provider_charge":{"unit":"credit","quantity":"2.17"}` for a confirmed task charge. Use a lowercase unit of up to 32 characters, beginning with a letter and containing letters, digits, or underscores. Quantities are nonnegative decimal strings with up to 15 integer and nine fractional digits. Omit the field for unknown charges, reservations, and legacy callers. An invalid charge is ignored while processing the status.
- After observing success, the worker reads zero-based slots at `GET /v1/jobs/{handle}/artifacts/{slot}`. Return HTTP 200 and the raw file with its exact content type. Image slots use `image/png` with a 2 MiB limit per output. The other media use one slot and an 8 MiB limit. Every output passes content validation before publication. Connector responses contain no fetch URLs. Keep artifacts available for the normal polling window and any permitted recheck.

The gateway records completion and the first valid charge before artifact retrieval. A successful artifact response with an invalid media type, size, or content fails with `provider_artifact_invalid`. Validation uses the shape selected by routing, including any allowed aspect ratio adjustment. The gateway publishes no files for the failed request and retains the observed charge. Reliability and settled-cost ranking count this outcome and definitive connector job failures for matching route history. Return an unavailable status until the artifact is ready to be served in full.

Unavailable artifacts, interrupted transfers, and failed storage publication retry from the same handle with delays of about 2, 4, 8, 16, and then up to 30 seconds. PostgreSQL keeps the schedule across worker restarts. The polling deadline still applies. An eligible operator recheck starts a fresh retry schedule. The charge appears on the request, in usage grouped by provider and unit, and in scoped exports. Jobs keep their original binding digest across API and worker restarts. To roll a connector origin or revision, introduce a new named binding, repoint the catalog, and keep the older entry with `drain_only: true` and its original name, origin, and revision until outstanding jobs and rechecks have drained. Use the [drain check](deployment.md#check-a-connector-rollout) before retiring the old entry.

## Recover a lost submission receipt

Set `"receipt_lookup": true` on a queue binding when the connector can read its durable request-to-handle mapping. Implement `GET /v1/jobs/by-request/{request_id}` with the binding service bearer credential. Return `{"version":1,"request_id":"req_example","handle":"job_connector_123"}` for a known accepted job, or `404` when the mapping is unavailable. Return the same handle that the original submission would acknowledge. Serve the lookup directly from connector state, using only the service credential. The subsequent status and artifact reads use the existing pinned credential checks.

The gateway checks the original binding when the submission response is lost, the returned handle is already assigned, or the worker lease expires before recording acceptance. It verifies the echoed request ID and claims the handle before resuming polls. Missing or invalid lookups retry for up to 15 minutes from dispatch. An unresolved job stays available for reconciliation.

The fal and Kinovi example bindings enable receipt lookup. Upgrade the connector and gateway binaries before adding the field to an existing configuration. Adding it preserves the binding identity, so the worker can recover eligible jobs already dispatched to that binding.

## Send completion notifications

Set `completion_webhooks: true` on a new connector binding to enable completion hints and receipt lookup. The gateway adds an optional `completion` object to `POST /v1/jobs` for that binding. Persist it with the stable request-to-handle mapping before replying.

```json
{
  "path": "/v1/connector-jobs/req_example/completion",
  "token": "<request-bound token>"
}
```

When the job reaches a terminal status, post `{"version":1}` to that path on your operator-configured gateway origin. Supply `Authorization: Bearer <token>` and `Content-Type: application/json`. Use HTTPS except for local fixtures. Keep the token in the connector and out of logs. The gateway returns `204` for an authenticated notification or replay. In case of an interrupted response, retry the same notification. When relaying an upstream webhook, verify it in the connector before notifying the gateway.

The notification carries an availability hint and can wake a poll before completion is recorded. The worker reads authoritative status and artifacts from the pinned connector. Once the gateway has recorded completion, it keeps the publication retry schedule. The request trace records callback receipt and verified handle recovery separately. Recovered jobs produce the usual terminal customer webhook. Enabling or disabling completion webhooks changes the binding identity. Roll to a new named binding and retain the old entry with `drain_only: true` while its requests drain.

An authenticated callback received before the gateway authorizes a retry can suspend that retry and recover the original job by request ID. Preserve the same `Idempotency-Key` mapping at the connector across retries and timeouts. A callback can also arrive after the next call has already been authorized.

Run the receipt recovery smoke against locally built gateway and admin binaries with PostgreSQL tools on `PATH`.

```sh
python3 -B scripts/smoke_connector_recovery.py target/debug/api-gateway target/debug/admin-cli
```

## Operate a binding

Keep model capabilities and prices in the gateway catalog up to date. Each connector can release independently. Draining and migration are covered in [Deployment](deployment.md).
