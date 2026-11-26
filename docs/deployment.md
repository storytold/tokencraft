# Deploy the gateway

Start a PostgreSQL server and the connector services for the offers you intend to serve. Use an operator-controlled catalog, with a provider ID and binding for each offer. Keep vendor credentials in the respective connector process. The gateway receives only the credentials needed to authenticate to that connector.

| Gateway configuration | Value |
| --- | --- |
| `DATABASE_URL` | PostgreSQL database used by the gateway and workers |
| `FOUNDATION_ENV`, `API_GATEWAY_PORT` | Use `development` and `4000` for a manual local run |
| `APP_BASE_URL`, `COOKIE_DOMAIN`, `COOKIE_SECURE` | For local Vite, use `http://localhost:3000`, `localhost`, and `false` |
| `SESSION_SECRET`, `TOKENCRAFT_DEMO_PAYLOAD_KEY` | Independently generated secrets. For local use run `openssl rand -hex 32` for each. Reuse the payload key for the same database. |
| `TOKENCRAFT_CORE_CATALOG` | Path to version 1 catalog JSON |
| `TOKENCRAFT_CONNECTORS` | Path to queue connector bindings if the catalog contains queue offers |
| `TOKENCRAFT_LOCAL_CHAT_BINDINGS` | Path to chat bindings if the catalog contains compatible chat offers |
| Binding token | The environment variable named by each binding |
| `TOKENCRAFT_DEMO_ARTIFACT_DIR` | Persistent local artifact directory for a single-host media setup |

The queue protocol accepts a loopback HTTP origin for local development or a configured HTTPS origin. A local chat endpoint uses loopback HTTP and the path `/v1/chat/completions`. Run an OpenRouter plugin on the same host as its gateway worker or configure a supported private compute binding.

With those settings, apply migrations before compiling the gateway. SQLx checks queries against that schema.

```sh
cargo run --locked -p admin_cli --bin admin-cli -- migrate
cargo run --locked -p api_gateway --bin api-gateway
```

The default process runs the API and workers together. Set `TOKENCRAFT_PROCESS_ROLE=api` or `TOKENCRAFT_PROCESS_ROLE=worker` to run them separately. Both roles need the same database, persistent payload key, catalog, and bindings. Use shared artifact storage when those roles run on separate machines. Check `/ready` and the catalog model list after deploying. Configure [event archives](event-archives.md) for verified filesystem or S3-compatible retention.

## Roll out compatible database changes

Deploy a gateway that supports migration overlap on the current schema before applying a newer migration. Each process checks the version, success flag, and checksum of every migration embedded in its binary. A newer binary becomes ready once its migrations are applied. An older binary can stay ready only if every later applied migration declares compatibility with its schema version.

Add the declaration in the same transaction as the additive migration. The first migration to use overlap creates the compatibility table. For example, a migration 75 reviewed against schema-74 binaries would include:

```sql
CREATE TABLE public.gateway_schema_compatibility (
    migration_version bigint PRIMARY KEY,
    oldest_gateway_schema bigint NOT NULL
        CHECK (oldest_gateway_schema BETWEEN 1 AND migration_version)
);

INSERT INTO public.gateway_schema_compatibility
    (migration_version, oldest_gateway_schema)
VALUES (75, 74);
```

Later compatible migrations add their own rows. Keep the table writable only by trusted migration operators and readable by gateway roles. Verify the oldest supported API and worker against the changed schema, including writes and in-flight requests, before declaring compatibility. For a breaking change, drain the affected binaries before applying it or use separate additive and cleanup migrations. Missing declarations, unsuccessful migrations, and mismatched embedded checksums all fail readiness.

Gateway background workers start after the database and schema probes pass. When a probe detects an incompatible schema, workers pause new provider claims, webhook deliveries, archive work, and scheduled cleanup. Work already started can finish under its existing lease. The next successful probe lets workers resume from durable state on their normal schedule.

The API checks the same database and schema state before handling operational requests, including direct calls that bypass ingress. During a pause it returns HTTP `503` with `Retry-After: 5`. Clients can retry a submission with the same body and idempotency key. The Rust and TypeScript SDKs offer opt-in submission retries and honor the retry hint when polling. The public health, readiness, and API contract endpoints remain reachable for diagnosis.

### Cut over without schema overlap

1. Pause new submissions at ingress while the old API still serves status and results. Let active provider jobs and deliveries drain, then stop the old API and workers so the backup captures a stable recovery point.
2. Retain the old image and deployment configuration. Back up PostgreSQL and record the corresponding recovery points for artifact storage, connector job state, and retained keys. Verify that the backup can be restored to a separate database before depending on it.
3. Apply the new image's migrations and [runtime grants](#separate-migration-and-runtime-roles). Start the new API and workers and check readiness. Run the [hosted smoke check](#run-behind-an-https-ingress) against the live gateway origin and verify an existing scoped session or key, request receipt, artifact, and usage before reopening submissions.
4. If recovery is needed, start the old image against the separately restored database and verify the original request and artifact before switching traffic. Preserve the upgraded database and reconcile any writes accepted after the backup before resuming admissions on the restored database.

Use a drain and restart for the schema 64 to 70 and schema 70 to 74 upgrades. The old hosted binaries refuse startup against the newer schema in each case. Migrations 71 through 74 add the workflow tables. Reapply the runtime grants after migrating so the restricted API and worker roles can access them. If you enable [Workflows](workflows.md), set the same persistent workflow key on both roles and verify a draft run and a published run with a scoped app key.

## Connect existing compute endpoints

In the organization console, open **Providers → Compute → Connect endpoint**. Choose AWS, Firebase / Cloud Run, Render, or Custom endpoint. All four templates use the same OpenAI-compatible chat request and response. Supply the deployed endpoint, target name, placement, and priority. Use HTTPS and the name of a bearer token environment variable for a remote endpoint. Local fixtures can use loopback HTTP. The review step validates configuration without calling the endpoint.

Download the review bundle and have the gateway operator merge it into the compute registry. With the open-source gateway CLI, use the deployed registry as the base and write the merged output to a new file.

```sh
api-gateway catalog apply-compute-patch --base bindings.json \
  --bundle compute-provider-patch.json > bindings.next.json
```

For the first registry, omit `--base`. Review the new placement and allowed locations before deploying the registry through `TOKENCRAFT_LOCAL_CHAT_BINDINGS`. Configure the named token in the worker environment. An `openai_compatible_chat` inference offer selects the pool through its `local_binding`. An organization can see its own pool before an offer uses it. Once an offer is linked, its projects can set fallback for that pool. Every endpoint in the pool should serve the same upstream model.

Open **Projects → Settings → Compute fallback** to inherit pool priorities or select and order targets for that project. The first target is primary. A single selection pins the project to that target. Newly onboarded targets are scoped to the organization that requested them. Compute fallback uses the existing safe retry rules, which can move to the next target when the connection fails before submission.

## Build the gateway runtime image

From the core repository, build the gateway target and tag it with the release version. The image includes the matching migration CLI and records the Git commit in `VERSION.json` when supplied at build time.

```sh
docker build --file deploy/container/Dockerfile --target gateway \
  --build-arg "SOURCE_REVISION=$(git rev-parse HEAD)" --tag tokencraft-gateway:0.1.0 .
```

Run the image on your private ingress network. Supply the [hosted configuration](#run-behind-an-https-ingress) and mount the operator's catalog, binding files, and any private PostgreSQL CA read-only. The gateway runs as UID and GID 65532. Set `TOKENCRAFT_LISTEN_IP` to the private address or `0.0.0.0` inside its container so the ingress can reach it. A connector in another container uses a configured HTTPS origin.

Use the same image with `--entrypoint admin-cli` and the argument `migrate` as a one-off job. Set `FOUNDATION_ENV` to the hosted environment and use the migration owner's verified database connection. Then run the API and worker roles from that image. The [Compose quickstart](setup.md#start-with-docker-compose) provides local synthetic inference with its own disposable state.

The **Container smoke** workflow builds both targets on pull requests, pushes to `main`, version tags, and manual runs. It verifies the Linux image architecture, source revision, release profile, and matching package versions. A version tag must match the package version, for example `v0.1.0`. The workflow applies migrations with the gateway image and starts two API replicas and a separate worker using restricted PostgreSQL credentials. It checks that the replicas share the organization quota, accepted requests replay across replicas, and the worker completes the original request and records its usage once. It also exercises the demo's bundled connector, persistence across container recreation, and the optional console mount. Version tags and manual runs reload the archived gateway image and verify its image ID before upload. In the extracted CI artifact, run `sha256sum -c SHA256SUMS`, then load the tarball with `docker load -i tokencraft-gateway-0.1.0-linux-amd64.tar.gz` using the file's actual version.

## Refresh reviewed rates

Set `TOKENCRAFT_DEMO_RATE_FEED` to a local JSON file on each API and worker replica. Use the `base_revision` from `GET /v1/catalog/offers` and the IDs of the offers you want the feed to manage.

For a new deployment, add a top-level `"rate_feed_offers": ["my_video", "my_audio", "my_mesh"]` alongside `version` and `offers` in the reviewed routing catalog. The gateway then requires those exact IDs on every load. Offers outside the list keep their configured prices. The field becomes part of the base revision. To introduce it on an existing deployment or change the managed IDs, prepare the revised catalog and its matching feed together.

For the first feed, use the planned catalog and the target's connector and chat binding environment to compute `base_revision` without starting the gateway.

```sh
api-gateway catalog check-rate-feed --catalog catalog.json
```

```json
{
  "base_revision": "demo_REPLACE_WITH_BASE_REVISION",
  "observed_at": "2026-11-25T23:00:00Z",
  "valid_until": "2026-11-26T23:00:00Z",
  "rates": {
    "my_video": {
      "source_url": "https://vendor.example/video-pricing",
      "schedule": { "kind": "video", "usd_nanos_per_second": 32000000 }
    },
    "my_audio": {
      "source_url": "https://vendor.example/audio-pricing",
      "schedule": { "kind": "audio", "usd_nanos_per_second": 4000000 }
    },
    "my_mesh": {
      "source_url": "https://vendor.example/mesh-pricing",
      "schedule": { "kind": "mesh", "usd_nanos_per_mesh": 80000000 }
    }
  }
}
```

Image schedules use `usd_nanos_per_image`. Text schedules use `usd_nanos_per_1000_input_tokens` and `usd_nanos_per_1000_output_tokens`. Supply integer rates in USD nanodollars using the routing catalog's billing unit. Each rate must be between half and twice its reviewed base amount, subject to the catalog's limits. This range applies consistently after reloads and restarts. A change outside the range requires a new reviewed base catalog. Before upgrading a running gateway, check its accepted feed against the reviewed catalog so a previously accumulated rate change can be reviewed before restart. The gateway uses source URLs as provenance metadata and retains each offer's reviewed capabilities and binding.

Validate a proposed file before replacing the active feed. Use the same reviewed catalog and base revision as the target gateway. For an existing deployment, pass a copy of the last accepted file as `--previous-feed` to check freshness and offer coverage against it.

```sh
api-gateway catalog check-rate-feed --catalog catalog.json --base-revision "$BASE_REVISION" \
  --feed proposed-rates.json --previous-feed current-rates.json
```

Omit `--previous-feed` for a first publication. Omit `--base-revision` when running the check with the target's configured bindings. Use the explicit revision from `GET /v1/catalog/offers` to check from a machine that has only a copy of the catalog. The check reports the resulting revision, included offer IDs, and their deadlines. The report's `valid_until` is the next unexpired deadline. The check rejects a proposal whose rates have all expired and leaves the input files untouched.

Replace the file atomically with a complete snapshot. Each process checks it every 15 seconds. A validity window can span up to seven days. A running process keeps its last accepted snapshot after a failed refresh. Quotes exclude feed-backed offers once their rate expires and can select other eligible offers. The offers API reports the effective `revision` and the `rate_valid_until` timestamps that the console uses to label expired prices.

### Refresh offers on different schedules

For prices reviewed at different times, move `observed_at` and `valid_until` into every rate entry. Omit the top-level pair. Upgrade API and worker replicas before installing a feed with per-offer windows. Existing files with one top-level window remain valid.

```json
{
  "base_revision": "demo_REPLACE_WITH_BASE_REVISION",
  "rates": {
    "my_video": {
      "source_url": "https://vendor.example/video-pricing",
      "observed_at": "2026-11-25T22:00:00Z",
      "valid_until": "2026-11-26T22:00:00Z",
      "schedule": { "kind": "video", "usd_nanos_per_second": 32000000 }
    },
    "my_audio": {
      "source_url": "https://vendor.example/audio-pricing",
      "observed_at": "2026-11-25T23:00:00Z",
      "valid_until": "2026-11-28T23:00:00Z",
      "schedule": { "kind": "audio", "usd_nanos_per_second": 4000000 }
    }
  }
}
```

When publishing one updated price, copy all other entries with their original observation times, deadlines, source URLs and rates. A live reload or a check with `--previous-feed` rejects older observations and changes at the same observation time. An expired entry stays in the reviewed set and remains ineligible until a new observation is approved. Requests retain the price evidence recorded at admission.

### Prepare a feed from approved changes

Use `catalog prepare-rate-feed` to turn reviewed offers into a complete candidate. Use the catalog and binding revisions from the `check-rate-feed` report. When updating, include the SHA-256 of the last accepted rate file, which can be calculated even after its rates expire. Enter only offers with approved observations.

```json
{
  "version": 1,
  "base_revision": "demo_REPLACE_WITH_BINDING_PINNED_REVISION",
  "catalog_file_revision": "demo_REPLACE_WITH_CATALOG_FILE_REVISION",
  "previous_feed_sha256": "REPLACE_WITH_LAST_ACCEPTED_FILE_SHA256",
  "rates": {
    "my_video": {
      "source_url": "https://vendor.example/video-pricing",
      "observed_at": "2026-11-25T23:00:00Z",
      "valid_until": "2026-11-26T23:00:00Z",
      "schedule": { "kind": "video", "usd_nanos_per_second": 32000000 }
    }
  }
}
```

```sh
api-gateway catalog prepare-rate-feed --catalog catalog.json --approved approved.json \\
  --previous-feed rates.json > rates.next.json
api-gateway catalog check-rate-feed --catalog catalog.json --feed rates.next.json --previous-feed rates.json
```

For an initial feed, omit both `previous_feed_sha256` and `--previous-feed`. For an update, the command copies the other offers and preserves each observation and deadline. A legacy shared window is expanded into individual windows. Use the gateway's existing review and atomic replacement procedure to publish the candidate. Rerun preparation against the latest file if another operator publishes during review.

### Carry reviewed rates into a preset update

After preparing a preset-only catalog update, use the deployment's binding configuration and a copy of its last accepted feed.

```sh
api-gateway catalog rebase-rate-feed --base catalog.json --candidate catalog.next.json \
  --feed current-rates.json > rates.next.json
```

The command checks the candidate against the base with the same preset-only rules as `catalog review`. It reads the configured bindings once, then validates the feed against both catalog revisions. The output carries the same rates, sources, observation times, and deadlines under the new base revision. Use newly reviewed observations if every rate has expired.

Deploy the two candidate files together on API and worker replicas. Keep the previous pair available for rollback until its feed expires.

### Collect OpenRouter list prices

Collect OpenRouter pricing for the exact upstream model IDs in the operator catalog.

```sh
api-gateway catalog observe-openrouter --catalog catalog.json > openrouter-observations.json
```

The command uses the public models endpoint and writes a JSON report. Use `--input snapshot.json` to read a saved response. The report includes the source, response SHA-256, original pricing fields, and exact list rates in USD nanodollars per 1,000 tokens. A file observation leaves the fetch time unset. Record the supplier observation time when reviewing rates from a saved response. [OpenRouter models API](https://openrouter.ai/docs/api/api-reference/models/get-models)

Review `list_rates` and any populated `additional_price_fields` against the applicable billing terms. Extra prices, such as search or cached inputs, produce `review_required`. Missing models and rates stay explicit. The command reads the operator catalog and writes observations to stdout. Deploy approved rates through the reviewed feed using the `base_revision` from your running gateway. The report's `catalog_file_revision` identifies the input file.

### Collect fal endpoint prices

Match configured fal queue offers to the pricing API's exact endpoint IDs.

```sh
api-gateway catalog observe-fal --catalog catalog.json --input fal-prices.json > fal-observations.json
```

Omit `--input` and supply `TOKENCRAFT_FAL_PRICING_KEY` in the command's environment to fetch prices directly. The command queries up to 50 distinct endpoints per request and reports a SHA-256 checksum for each response. Supply complete responses when using a saved file. File observations leave the fetch time unset. [fal pricing API](https://fal.ai/docs/platform-apis/v1/models/pricing)

Each observation includes the vendor's original billing unit and price, the exact `usd_nanos_per_vendor_unit` when available, and the active offer's configured schedule for comparison. Check the endpoint's billing terms against the offer's workload before approving a rate. For example, a price per megapixel needs a verified image size, and a price per video second may depend on resolution or audio. Apply approved schedules through the reviewed feed described above. [fal model pricing](https://fal.ai/docs/documentation/model-apis/pricing)

### Review Kinovi credit estimates

Create a probe for a Kinovi queue offer in your operator catalog. Use synthetic text and the same media settings that the plugin sends for the workload you are reviewing. For the example `kinovi_sample` offer, a probe can look like this. [Seedance 2.0 input contract](https://dev.kinovi.ai/docs/models/seedance-20)

```json
{
  "version": 1,
  "offer": "kinovi_sample",
  "inputs": {
    "prompt": "A blue square rotating on a plain white background",
    "duration": "5",
    "outputResolution": "720p",
    "aspectRatio": "16:9"
  }
}
```

Supply `TOKENCRAFT_KINOVI_PRICING_KEY` in the command's environment. The command sends the offer's upstream model ID and the probe inputs to Kinovi's `estimate_cost` tool. The tool estimates the task in credits without creating a generation request. [Kinovi estimate tool](https://dev.kinovi.ai/docs/mcp) · [API key setup](https://dev.kinovi.ai/docs/mcp/api-key)

```sh
api-gateway catalog observe-kinovi --catalog catalog.json --probe kinovi-probe.json > kinovi-observation.json
```

For an offline review, pass `--input kinovi-estimate.json`. Pair the result with the exact probe you used. The saved file records your association between the probe and the result. Replace the digest placeholder below with the SHA-256 of `kinovi-probe.json` and the illustrative numbers with the credits from that probe.

```json
{
  "probe_sha256": "REPLACE_WITH_PROBE_SHA256",
  "estimate": { "modelId": "seedance-20", "credits": 180, "originalCredits": 200 }
}
```

To compare in USD, pass `--usd-nanos-per-credit` with an integer from your Kinovi billing terms. The report uses the estimated credits for the task and produces an exact total when the amount fits at nanodollar precision and the estimate has at most six fractional credit digits. Otherwise it flags the amount for review. Keep the original credit estimate, conversion terms, and probe as review evidence. Verify any per-second or per-generation schedule across its supported workload before applying an approved rate feed. [Kinovi billing](https://kinovi.ai/docs/credits-billing)

## Run behind an HTTPS ingress

Set `FOUNDATION_ENV=production` or `FOUNDATION_ENV=staging` and set `TOKENCRAFT_CORE_CATALOG` to your operator-reviewed catalog. Both environments use the same hosted safeguards.

| Hosted configuration | Value |
| --- | --- |
| `DATABASE_URL` | PostgreSQL TCP URL with `sslmode=verify-full`. Use the hostname on the server certificate and set `sslrootcert` in the URL when the server uses a private CA. |
| `APP_BASE_URL` | HTTPS console origin, for example `https://console.example.com`. Use the origin without a path. |
| `COOKIE_SECURE` | `true`. The hosted session cookie is bound to the API host. |
| `SESSION_SECRET`, `TOKENCRAFT_DEMO_PAYLOAD_KEY` | Generate a separate value for each with `openssl rand -hex 32`. Share the same values across replicas and preserve the payload key with the database backup. |
| `TOKENCRAFT_LISTEN_IP`, `API_GATEWAY_PORT` | Private HTTP listener. Defaults to `127.0.0.1:4000`. Set a literal IP explicitly if an ingress or probe needs to reach it from another container. |
| `TOKENCRAFT_CORE_CATALOG`, binding registries | Configure the real services that will execute the offers. The hosted profile accepts connector-backed offers. |

Apply migrations and grant runtime access before starting any hosted process. The binaries check the migration history before opening a listener. Run `api-gateway` with `TOKENCRAFT_PROCESS_ROLE=api` or `worker` for independent scaling. If the roles run on the same machine, assign distinct ports. [Database role setup](#separate-migration-and-runtime-roles)

Terminate public TLS at your ingress and keep the gateway's HTTP listener private. Forward API requests and streaming response bodies without buffering. Set `X-Forwarded-For` to the client address observed at the trusted ingress. Route worker `/ready` probes through the private network. Build the console with `VITE_API_BASE_URL` set to the public HTTPS gateway origin. Serve the console and API on the same site, such as `console.example.com` and `api.example.com`. Browsers send the session cookie to the API host. Credentialed CORS and session writes require the configured console origin. API keys work for non-browser clients.

When proxying the API on the console origin, forward `/ready` and `/v1/` to the gateway. The console can serve the release snapshot at `/openapi.json` and `/docs/openapi.json`. Run the read-only check from the release checkout after deployment. Supply the gateway origin separately so the check verifies the live contract even when the console serves a static snapshot.

The Vercel configs restrict browser scripts to the console, Google Identity Services, and the hashed standalone design script. They also block framing and send explicit content-type and referrer policies. The gateway supplies its own headers on proxied API responses. After editing the standalone design HTML, refresh its hashes in both Vercel configs with `node scripts/console_security_headers.mjs --write`.

```sh
bash scripts/smoke_hosted_console.sh https://console.example.com https://api.example.com
```

The check compares both console copies and the running gateway with the release snapshot and verifies the console catalog bytes. It also sends an unauthenticated request to `/v1/rate-limits` at both origins and expects the gateway's `401` JSON response. This confirms that the route is mounted and that the console forwards it to an API, without needing an account or creating a job. Treat a mismatch as a failed release check even when both `/ready` probes pass.

For deployments with the bundled `example_image` connector, run the authenticated check with a dedicated project key in `TOKENCRAFT_SMOKE_API_KEY`. Grant it `requests:submit`, `requests:read`, and `usage:read`. Supply `--peer` only if that trusted origin serves the same deployment.

```sh
python3 -B scripts/smoke_authenticated_gateway.py \
  --api https://api.example.com --peer https://console.example.com
```

The check submits one synthetic image with a zero-cost cap and replays its receipt. It verifies the PNG digest, connector trace, and one usage record scoped to the run. Inject the scoped project key into the process environment from your secret manager. For a private HTTPS ingress, supply its trusted CA bundle with `--ca`.

To deliver signed customer webhooks, set `TOKENCRAFT_DEMO_WEBHOOK_CONFIG` to an operator-maintained JSON registry such as `{"destinations":{"customer_updates":"https://events.example.com/tokencraft"}}`. Give the API and workers the same independent 32-byte hex `TOKENCRAFT_DEMO_WEBHOOK_KEY`. Hosts use the standard public certificate roots by default. Set `TOKENCRAFT_WEBHOOK_CA_FILE` to an operator-managed PEM bundle to use a private trust store for all webhook destinations. Tenants select a registry entry by ID. Register reviewed HTTPS destinations and set egress policy for the hosts you allow. TLS verifies the destination hostname and the client rejects redirects.

Each worker runs up to four delivery attempts at a time. A worker taking over an expired lease records that attempt and retries with the same event ID and body. Receivers should deduplicate by `webhook-id` because a successful delivery can be retried after a lost response or worker restart. Revocation prevents new attempts and redrives. An attempt already leased when revocation commits may finish.

For retryable responses, the worker honors `Retry-After` in seconds or as an HTTP date, up to 60 seconds. It waits at least the normal backoff and retains the scheduled retry in PostgreSQL. [HTTP Retry-After](https://www.rfc-editor.org/rfc/rfc9110.html#name-retry-after)

### Separate migration and runtime roles

Use a dedicated PostgreSQL database whose owner applies migrations and owns the TokenCraft objects in the `public` schema. Give the API and worker a separate login with an independent credential. The runtime login needs no database ownership, elevated role membership, or permission to create objects in `public`. In clusters upgraded from an older PostgreSQL release, check whether `PUBLIC` still has `CREATE` on that schema and revoke it in the dedicated database if needed.

With the migration owner's verified `DATABASE_URL` configured for `admin-cli`, apply migrations. In staging and production the CLI requires a TCP connection with `sslmode=verify-full` before it connects. Run the grant script against the same database as the migration owner. The example uses a PostgreSQL service definition called `tokencraft_migration` with `sslmode=verify-full`. Keep the password in your secret configuration.

```sh
FOUNDATION_ENV=production admin-cli migrate
psql -X "service=tokencraft_migration" -v runtime_role=tokencraft_runtime \
  -f deploy/postgres/grant-runtime.sql
```

The [grant script](../deploy/postgres/grant-runtime.sql) grants access to the owner's application tables, views, sequences, and invoker functions. It gives the runtime role read-only access to the migration ledger and any schema compatibility declarations. Reapply it after each migration before sending traffic to the new replicas. Configure the API and worker with the runtime login in `DATABASE_URL`. Keep the migration login in the one-off deployment job.

The bundled Render demo migrates during startup by default. For a demo with a separate migration job, apply the matching image's migrations and runtime grants first, then set `TOKENCRAFT_RENDER_MIGRATE_ON_START=false` and configure the service with the runtime login. The gateway opens its listener only after it confirms the matching schema.

## Shared artifact storage

Use a private S3-compatible bucket to serve generated media from multiple gateway and worker replicas. Apply migrations and upgrade both roles before enabling bucket writes. Configure these values on the API and workers. New artifacts use the bucket when configured. Keep the original `TOKENCRAFT_DEMO_ARTIFACT_DIR` mounted until existing filesystem results expire or archive. PostgreSQL continues to serve existing inline artifacts.

| Gateway configuration | Value |
| --- | --- |
| `TOKENCRAFT_ARTIFACT_S3_ENDPOINT` | Region-specific HTTPS origin. Literal loopback HTTP is accepted for local fixtures. |
| `TOKENCRAFT_ARTIFACT_S3_BUCKET` | Private unversioned bucket with Object Lock disabled. |
| `TOKENCRAFT_ARTIFACT_S3_REGION` | Bucket signing region. |
| `TOKENCRAFT_ARTIFACT_S3_ACCESS_KEY`, `TOKENCRAFT_ARTIFACT_S3_SECRET_KEY` | Credentials with `s3:GetObject`, `s3:PutObject`, and `s3:DeleteObject` for the artifact prefix. |
| `TOKENCRAFT_ARTIFACT_S3_SESSION_TOKEN` | Optional temporary credentials. Restart with fresh credentials on rotation. |
| `TOKENCRAFT_ARTIFACT_S3_PREFIX` | Defaults to `tokencraft-artifacts/v1`. Use a dedicated prefix for active media. |

Replicas serving the same database need the same artifact target and persistent payload key. TokenCraft namespaces objects by its database identity, verifies encrypted uploads before committing the result, and serves bytes through the same scoped artifact endpoint. Workers reclaim abandoned uploads and delete live objects after expiry, early removal, or verified archival. The **Gateway operations** readiness check reads a small marker in the configured prefix.

An S3 outage keeps API-serving processes in rotation while their core readiness checks pass. Request status, catalog, and usage stay reachable. A download that needs the bucket returns HTTP `503`, and pending media results await verified storage. The administrator's setup view still reports the artifact failure, and worker-only `/ready` still requires storage. Filesystem readiness can withdraw a replica whose local storage fails.

Keep the active prefix readable for the lifetime of the saved results. Use the separate [request content archive](chat-history.md) for lifecycle transitions to cold storage. Bucket deletion must permanently remove the current object for cleanup to complete. [S3 object deletion](https://docs.aws.amazon.com/AmazonS3/latest/API/API_DeleteObject.html)

## Gravatar imports

Give the sweeper the same `DATABASE_URL` as the gateway. Set `R2_ENDPOINT`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, and `R2_BUCKET_NAME` on the sweeper for S3-compatible avatar storage. Set `R2_REGION` if your storage service requires a region other than `auto`. Set `CDN_BASE_URL` on the gateway to the public base URL that serves those avatar objects.

Run the sweeper alongside the gateway after the database migrations are applied.

```sh
cargo run --locked -p sweeper
```

The sweeper imports Gravatars after signup and email changes, and when a signed-in user selects **Check** or **Refresh** in Settings. The console loads the mirrored image from the configured CDN. Users can also choose initials or remove the mirror. The credential-free quickstart only displays initials until avatar storage and the sweeper are configured.

## Successful result retention

By default, the gateway stores successful chat results for one hour and generated media for 24 hours. For newly admitted connected chat and media, operators can opt into [30-day warm history and archival through day 395](chat-history.md) with a filesystem directory or an S3-compatible bucket. Result and artifact reads enforce the saved deadline. At expiry the worker clears the encrypted result, stream chunks, and artifact references and publishes a scoped `request.content_removed` event. This is the same removal path used for early project administrator deletion. Request state, trace timing, and accounting stay queryable. File cleanup removes unreferenced artifacts asynchronously.

After upgrading, a worker also publishes the removal event for expired successful results whose bytes were cleared by an earlier gateway. A [customer data consumer](customer-data.md) can use the event to reconcile its own content cache.

## Check a connector rollout

Run the drain check against the deployment database with the candidate catalog and the candidate binding environment. `DATABASE_URL` can use a role with read access to requests, provider runs, connector completion state, and provider credential state. Use verified PostgreSQL TLS for a remote database. The command reads a point-in-time snapshot in a read-only transaction.

```sh
api-gateway catalog check-drain --catalog catalog.next.json
```

The JSON report gives the candidate base revision and counts by offer, request phase, and pinned binding digest. A blocker makes the command exit nonzero. Wait for outstanding work with the previous workers in place, then rerun the check close to cutover.

| Blocker | What to preserve |
| --- | --- |
| `queued_offer_changed` | Keep the original offer and its active binding until queued requests dispatch. The check uses the saved provider, upstream model, and binding identity. Rate-only changes preserve queued pins. |
| `binding_unavailable` | Keep the original binding for in-flight work, submission recovery, and eligible rechecks. For queued media, a retained `drain_only` entry serves polling and recovery after the original offer stops accepting new work. |
| `legacy_adapter` | Keep a worker that supports the original built-in adapter until its outstanding work finishes. |

When replacing a queue connector, provision a new binding name. Keep the old origin, revision, and completion settings available until the old work drains. A drain-only worker polls and recovers existing jobs on that binding. Workers with the active binding continue to claim submissions during cutover. Continue to coordinate admissions during cutover and review provider access and pricing for the candidate. [Connector protocol](provider-connectors.md#queued-media-protocol-version-1)

## Move from a deployment with built-in provider adapters

The migration history keeps the original SQL files and checksums. The core workers claim new `connector_queue` and `openai_compatible_chat` runs. Old jobs carry their original adapter and binding, so drain them using the previous worker and credential configuration before cutting over. Create fresh bindings and catalog offers for the plugin deployments. Use stable binding names and revisions while queued jobs are in flight.

## Routing policy precedence

For automatic routing, preferences and minimum model quality or usage take the first configured value from the request, project, organization, or gateway default, where available. For example, a project policy can set minimum community quality to 6 out of 10, equivalent to 3 out of 5 stars. Hard budgets, provider allowlists, and data-handling requirements apply across scopes.

## Balance API replicas within a cell

Run the [HAProxy 3.2+ configuration](../deploy/haproxy/cell-api.cfg) on a private network between trusted HTTPS ingress and the API replicas for one cell. It works with one replica and discovers additional replicas from a private DNS record. A future organization router forwards to the selected cell’s proxy tier. Each proxy has one cell’s DNS target, so an unavailable API pool fails within that cell.

```text
Client → HTTPS ingress → optional organization router → cell HAProxy → ready Axum replica
                                                                            ↓
                                                                     cell PostgreSQL
```

### Configure the private proxy

The HTTPS ingress must replace incoming `X-Forwarded-For` with one client IP it verified, preserve the original Host and application headers, and stream responses without buffering. Configure any intermediate organization router to preserve that single verified address. Restrict the proxy listener to trusted ingress peers at the network boundary and restrict API listeners to the cell’s proxy peers. The proxy also checks the source allowlist. Use a private resolver that returns only API-role backends belonging to this cell. Keep every replica on the cell’s PostgreSQL database and shared artifact storage with matching session and payload keys.

| Setting | Example or meaning |
| --- | --- |
| `TOKENCRAFT_LB_BIND` | Private listen address and port, such as `10.20.1.10:8080`. |
| `TOKENCRAFT_LB_TRUSTED_INGRESS_FILE` | Absolute path to a file with one trusted ingress IP or CIDR per line. Include only the ingress and probe sources the proxy will actually see. |
| `TOKENCRAFT_LB_API_ADDRESS` | Use a cell-specific DNS name and common port, such as `api.cell-a.internal:4000`, or an SRV service name without an appended port, such as `_tokencraft._tcp.cell-a.internal`. Each SRV record supplies the target hostname and port for one API replica. |
| `TOKENCRAFT_LB_API_SLOTS` | Maximum number of discovered replicas this instance can hold. Start with a ceiling that fits the cell and DNS response size, such as 32. |
| `TOKENCRAFT_LB_MAX_CONNECTIONS` | Total incoming connection limit per proxy process. |
| `TOKENCRAFT_LB_CONNECTIONS_PER_API` | Connection limit per API replica from this proxy. Size it across all proxy replicas, with headroom for PostgreSQL pools and long-lived streams. |

Set the variables above in the service environment. Validate the configuration with `haproxy -c -f deploy/haproxy/cell-api.cfg` and run HAProxy in the foreground with `haproxy -db -f deploy/haproxy/cell-api.cfg` under the platform service manager.

HAProxy checks each replica’s `GET /ready`. Newly discovered replicas start down and become eligible after successful checks. The proxy uses least connections for a mix of short requests and longer streams, bounds how long a request waits for a connection, and gives streams a 120-second idle window. A protected `GET /_tokencraft/ingress-ready` returns 200 when the proxy has an eligible API replica and 503 otherwise. While all replicas are withdrawn, HAProxy adds `Retry-After: 5` and `Cache-Control: no-store` to its 503 responses. The gateway still serves its regular `/ready` endpoint.

The proxy forwards requests once. Configure upstream ingress to avoid automatic submission retries as well. After a lost reply, callers can retry with the original idempotency key and intent. Stream reconnects recover retained chunks from cell state. The proxy logs replica selection, response status, queue time, connect time, and response timing.

### Scale and verify the cell

The [capacity guide](capacity.md) describes the organization submission bucket, self-service quota tiers, and the opt-in, signal-driven API and worker controller.

Run redundant proxy instances across the cell’s failure domains and configure HTTPS ingress to check each instance’s private readiness endpoint. Keep an instance serving while replacing another. Drain API replicas before stopping them and give active streams time to finish. Load-test simultaneous streams, small reads, and submissions. Measure ready replica count, queue time, connection saturation, tail latency, stream disconnects, and PostgreSQL pool waits. Verify that the remaining replicas can handle peak traffic when a zone is removed. Add cells when database capacity or failure isolation calls for it.

Workers still share jobs through PostgreSQL lease claims. Self-hosted inference uses the cell’s configured compute binding, including its eligible targets and fallback rules. An internal replica service is optional for a group of interchangeable model servers in one placement and policy domain. Managed providers operate their own inference load balancing.

Each provider worker has four in-flight task slots shared by submissions and polling. A worker with multiple destinations limits each destination to three slots so another destination can keep serving during a stall. Direct connector and chat bindings with the same scheme, host, and port share that limit, including during a binding rollout. A compute stack counts as one destination. A worker with one destination can use all four slots. These limits apply per worker process.

With HAProxy 3.2+ available, run `python3 deploy/haproxy/smoke.py` in a disposable local environment. It checks configuration parsing, two fixture API replicas, readiness withdrawal and recovery, forwarding headers, SSE streaming, and a submission whose upstream disconnected after receiving the body. [HAProxy configuration reference](https://docs.haproxy.org/3.2/configuration.html)

A local integration check with HAProxy 3.4.8 used private DNS SRV records, two TokenCraft API processes, and a worker on shared PostgreSQL 17. The proxy added a replica without restarting. Killing the replica carrying an SSE connection ended that connection. Retrying the same request with the original idempotency key through the surviving replica returned the same request ID, replayed the retained chunk, and delivered the final chunk and `[DONE]`. The synthetic inference endpoint received one call. With both replicas down, readiness and API reads returned HTTP 503. In staging, repeat with the deployment's private resolver and HTTPS ingress, and exercise loss of a proxy or zone under load.
