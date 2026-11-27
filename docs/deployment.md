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

Each worker process advances up to two workflow runs at a time and processes deadlines and retention cleanup independently. PostgreSQL leases and admission limits continue to apply across all replicas.

## Roll out compatible database changes

Deploy a gateway that supports migration overlap on the current schema before applying a newer migration. Each process checks the version, success flag, and checksum of every migration embedded in its binary. A newer binary becomes ready once its migrations are applied. An older binary can stay ready only if every later applied migration declares compatibility with its schema version.

Migration 77 adds workflow operations indexes and creates the compatibility table with schema 76 as its oldest supported gateway. In the same transaction it gives the current migration ledger readers read access to the new table. Schema 76 API and worker processes can stay online while you migrate and roll out schema 77. The index builds block writes to the workflow runs table until the migration commits. Duration depends on table size and concurrent work.

Add each later declaration in the same transaction as the additive migration. For a migration 78 reviewed against schema 77 binaries, include the following SQL.

```sql
INSERT INTO public.gateway_schema_compatibility
    (migration_version, oldest_gateway_schema)
VALUES (78, 77);
```

Later compatible migrations add their own rows. Keep the table writable only by trusted migration operators and readable by gateway roles. Verify the oldest supported API and worker against the changed schema, including writes and in-flight requests, before declaring compatibility. For a breaking change, drain the affected binaries before applying it or use separate additive and cleanup migrations. Missing declarations, unsuccessful migrations, and mismatched embedded checksums all fail readiness.

Gateway background workers start after the database and schema probes pass. When a probe detects an incompatible schema, workers pause new provider claims, webhook deliveries, archive work, and scheduled cleanup. Work already started can finish under its existing lease. The next successful probe lets workers resume from durable state on their normal schedule.

The API checks the same database and schema state before handling operational requests, including direct calls that bypass ingress. During a pause it returns HTTP `503` with `Retry-After: 5`. Clients can retry a submission with the same body and idempotency key. The Rust and TypeScript SDKs offer opt-in submission retries and honor the retry hint when polling. The public health, readiness, and API contract endpoints remain reachable for diagnosis.

### Check the database before rollout

Run `admin-cli schema-status` from the target build using the gateway runtime role and the deployment's verified database connection. The command checks the public migration ledger in a read-only transaction. It exits successfully when the recorded migrations are compatible with the target build and exits nonzero when they need operator action. Recheck after applying migrations and runtime grants.

| Status | Meaning |
| --- | --- |
| `current` | The applied migrations match the target build. |
| `forward_compatible` | The build's migrations match and every later migration explicitly allows it. |
| `upgrade_required` | The applied migrations match a prefix of the target build. The command lists the pending versions. Follow the cutover or rolling procedure before starting the new fleet. |
| `diverged` | The recorded history has a gap, a failed migration, or a checksum mismatch. Verify the release history before running migrations. |
| `incompatible_newer` | One or more later migrations failed or lack a compatibility declaration for this build. Use the matching image or restore the prior recovery point before rolling back. |
| `empty` | The public schema has no ledger or application objects. |
| `unmanaged` | The public schema has objects without a TokenCraft ledger. Verify the database connection before proceeding. |

### Cut over without schema overlap

1. Pause new submissions at ingress while the old API still serves status and results. Let active provider jobs and deliveries drain, then stop the old API and workers so the backup captures a stable recovery point.
2. Retain the old image and deployment configuration. Back up PostgreSQL and record the corresponding recovery points for artifact storage, connector job state, and retained keys. Verify that the backup can be restored to a separate database before depending on it.
3. Apply the new image's migrations and [runtime grants](#separate-migration-and-runtime-roles). Check the schema with that image's `admin-cli schema-status` using the runtime role, then start the new API and workers and check readiness. Run the [public and read-only authenticated checks](#run-behind-an-https-ingress) against the live deployment before reopening submissions.
4. If recovery is needed, start the old image against the separately restored database and verify the original request and artifact before switching traffic. Preserve the upgraded database and reconcile any writes accepted after the backup before resuming admissions on the restored database.

A schema-70 fleet can move directly to schema 87 in one coordinated cutover. Apply all pending migrations with the target build, then reapply the runtime grants before restarting the fleet.

Deploy client-bound quote signing to the API replicas together. Refresh unused quotes issued before the upgrade. Requests already admitted replay with their original quote, payload, and idempotency key.

Use a drain and restart for the schema 64 to 70 upgrade and upgrades from schema 70, 74, or 75 to schema 76 or 77. Migrations 71 through 74 add the workflow tables, migration 75 adds app-scoped terminal events, and migration 76 adds workflow budget reservations. Reapply the runtime grants after migrating. If you enable [Workflows](workflows.md), set the same persistent workflow key on both roles and verify a draft run and a published run with a scoped app key. Verify a signed terminal event when workflow webhook delivery is enabled.

Use the same drain and restart when moving to schema 78. It adds a separate encrypted attachment for queued workflow first frames. The new worker can still read inline frames queued by earlier versions. Reapply the runtime grants before starting the new fleet.

Migration 79 adds the active workflow deadline index and supports overlap with schema-78 API and worker binaries. The index build pauses writes to the workflow runs table until it commits. On the new worker, deadline processing uses batches of up to 64 overdue runs and works while execution slots are busy. A row held by another transaction is picked up after that transaction releases it.

Use a coordinated drain and restart when moving to schema 80. Published workflows can supply a customer reference, and workers store it on each child alongside the application budget identity. Upgrade all API servers and workers in the cell before reopening admissions. The migration adds an application spending index to the request table.

Migration 81 adds an optional customer reference to new terminal workflow webhook events. Schema-80 API and worker binaries can overlap with the new fleet. Existing queued deliveries keep their original payload bytes and runtime grants carry forward.

Migration 82 indexes successful requests for scoped latency baselines. Schema-81 API and worker binaries can overlap with the new fleet after the index commits. The index build pauses writes to the request table, so apply it during the planned cutover window.

Migration 83 indexes recent terminal media outcomes by scope and workload. Schema-82 gateway binaries can overlap after the index commits. Apply it during the cutover window because the index build pauses writes to the request table.

Migration 84 aligns the database's WAV limit with the connector's 8 MiB limit. Schema-83 binaries can overlap after it commits. Apply it during the cutover window because validating the replacement constraint locks the artifact table.

Migration 85 includes the optional client reference in newly queued native request webhooks. Schema-84 binaries can overlap with the new fleet. Existing deliveries keep their original event IDs and payload bytes, and runtime grants carry forward.

Migration 86 adds the read-only provider work sampler and supports overlap with schema-85 binaries. Reapply the Operations SQL after migration. Start the updated collectors after the probed workers report binding digests for all three provider lanes.

Migration 87 adds a receipt digest for recoverable webhook key rotations. Schema-86 workers can overlap with the new fleet, and queued event bytes remain intact. Workers use the active signing key and the overlapping key when applicable. Upgrade the API replicas before enabling the new rotation action in the console.

### Rehearse the cutover before release

The **Release cutover** workflow builds the pinned schema-70 release (`06276c9`) and the candidate on Linux. It uses an isolated PostgreSQL 17 cluster with hostname-verified TLS and a restricted runtime role. The binding preflight distinguishes an undispatched request from a job already accepted by its provider. Both the upgrade and rollback replay receipts, resume the accepted job under its existing handle without resubmission, finish queued and new work, and verify artifact and signed webhook bytes. Rollback uses independent snapshots of PostgreSQL, connector state, and the filesystem artifact store.

Run the same rehearsal from a source checkout with matching binaries, PostgreSQL 17, OpenSSL, and loopback port 4019 available.

```sh
python3 -B scripts/smoke_release_cutover.py \
  --baseline-admin ./baseline/bin/admin-cli \
  --baseline-gateway ./baseline/bin/api-gateway \
  --baseline-connector ./baseline/bin/tokencraft \
  --candidate-admin ./target/debug/admin-cli \
  --candidate-gateway ./target/debug/api-gateway \
  --candidate-connector ./target/debug/tokencraft
```

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

From the core repository, build the gateway target and tag it with the release version. The image includes the matching migration CLI and records the Git commit in `VERSION.json` when supplied at build time. The public catalog, OpenAPI contract, connector primitive registry, and PostgreSQL runtime grant script are in `/opt/tokencraft`.

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

Replace the file atomically with a complete snapshot. API and worker processes reload it independently every 15 seconds. Workers use their loaded rates for new workflow children and dispatch fallback. A validity window can span up to seven days. A running process keeps its last accepted snapshot after a failed refresh. Once a rate expires, quotes omit its estimate and can select another eligible offer under the same spend cap.

Before committing a new admission, the process rechecks the quote deadline and the catalog revision it used. If its loaded revision changed or the deadline passed during a budget or quota wait, it returns `409 quote_stale` and rolls back the request and its reservation. For a sealed submission, fetch a fresh quote and retry with the same idempotency key. Automatic submissions can retry directly with that key. Retrying an already accepted request still returns its original receipt. A workflow step can retry pricing within its existing execution deadline and spend cap. The offers API reports the effective `revision` and the `rate_valid_until` timestamps that the console uses to label expired prices.

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

The Vercel configs restrict browser scripts to the console, Google Identity Services, and the hashed standalone design script. They also block framing and send explicit content-type and referrer policies. The gateway sends `Cache-Control: private, no-store` on scoped API responses and errors, including authentication, usage, artifacts, and conditional workflow reads. The static gateway OpenAPI and primitive schema documents opt in to a five-minute public TTL. Both Vercel configs disable edge caching on the API and readiness rewrites. After editing the standalone design HTML, refresh its hashes in both Vercel configs with `node scripts/console_security_headers.mjs --write`. [Vercel cache-control headers](https://vercel.com/docs/caching/cache-control-headers)

Pass the expected Git commit to verify source revisions as well as the contract. Release archives built with `native_package.py --source-revision` and container builds with `SOURCE_REVISION` stamp the gateway binary. Other gateway builds can set `TOKENCRAFT_SOURCE_REVISION` when compiling. The gateway exposes the stamp in the `TokenCraft-Source-Revision` header on `/ready`. Set the same build variable when building the console, or enable Vercel's Git system environment variables, to emit `/release.json`. [Vercel system environment variables](https://vercel.com/docs/environment-variables/system-environment-variables#system-environment-variables)

```sh
source_revision="$(git rev-parse HEAD)"
bash scripts/smoke_hosted_console.sh https://console.example.com https://api.example.com "$source_revision"
```

The check compares both console copies and the running gateway with the release OpenAPI snapshot. It also compares the connector primitive registry at both origins and verifies the console catalog bytes. An unauthenticated request to `/v1/rate-limits` at both origins must return the gateway's `401` JSON response. This confirms that the route is mounted and that the console forwards it to an API, without needing an account or creating a job. Treat a mismatch as a failed release check even when both `/ready` probes pass.

Before reopening submissions, pick an existing successful request with a gateway trace and a readable warm result. Inject a project key with `requests:read` and `usage:read` into `TOKENCRAFT_SMOKE_API_KEY`.

```sh
python3 -B scripts/smoke_authenticated_gateway.py \
  --api https://api.example.com --peer https://console.example.com \
  --revision "$source_revision" --request-id "$previous_request_id"
```

This mode uses GET requests to check the saved result, media digests, trace, scoped history, and usage for the UTC day of admission. With `--peer`, both origins must agree on the receipt identity and result. It repeats the scoped reads without a key and requires private, non-storable gateway denials. Omit `--peer` for a console that connects directly to the gateway rather than proxying API routes.

For deployments with the bundled `example_image` connector, run the authenticated check with a dedicated project key in `TOKENCRAFT_SMOKE_API_KEY`. Grant it `requests:submit`, `requests:read`, and `usage:read`. Supply `--peer` only if that trusted origin serves the same deployment.

```sh
python3 -B scripts/smoke_authenticated_gateway.py \
  --api https://api.example.com --peer https://console.example.com --revision "$source_revision"
```

With the revision set, the check verifies both origins without sending the scoped key. It then submits one synthetic image with a zero-cost cap and replays its receipt. It verifies the PNG digest, connector trace, and one usage record scoped to the run. Inject the scoped project key into the process environment from your secret manager. For a private HTTPS ingress, supply its trusted CA bundle with `--ca`.

To deliver signed customer webhooks, set `TOKENCRAFT_DEMO_WEBHOOK_CONFIG` to an operator-maintained JSON registry such as `{"destinations":{"customer_updates":"https://events.example.com/tokencraft"}}`. Give the API and workers the same independent 32-byte hex `TOKENCRAFT_DEMO_WEBHOOK_KEY`. Hosts use the standard public certificate roots by default. Set `TOKENCRAFT_WEBHOOK_CA_FILE` to an operator-managed PEM bundle to use a private trust store for all webhook destinations. Tenants select a registry entry by ID. Register reviewed HTTPS destinations and set egress policy for the hosts you allow. TLS verifies the destination hostname and the client rejects redirects.

Each worker runs up to four delivery attempts at a time. When three attempts target the same origin and other origins are configured, the fourth slot stays available for a different origin. Destination aliases with the same scheme, host, and port share capacity. A deployment with one receiver origin can use all four slots per worker.

A worker taking over an expired lease records that attempt and retries with the same event ID and body. Receivers should deduplicate by `webhook-id` because a successful delivery can be retried after a lost response or worker restart. Revocation prevents new attempts and redrives. An attempt already leased when revocation commits may finish.

For retryable responses, the worker honors `Retry-After` in seconds or as an HTTP date, up to 60 seconds. It waits at least the normal backoff and retains the scheduled retry in PostgreSQL. [HTTP Retry-After](https://www.rfc-editor.org/rfc/rfc9110.html#name-retry-after)

### Separate migration and runtime roles

Use a dedicated PostgreSQL 17 database whose owner applies migrations and owns the TokenCraft objects in the `public` schema. Give the API and worker a separate login with an independent credential. The runtime login needs no database ownership, elevated role membership, or permission to create objects in `public`. In clusters upgraded from an older PostgreSQL release, check whether `PUBLIC` still has `CREATE` on that schema and revoke it in the dedicated database if needed.

With the migration owner's verified `DATABASE_URL` configured for `admin-cli`, apply migrations. In staging and production the CLI requires a TCP connection with `sslmode=verify-full` before it connects. Run the grant script against the same database as the migration owner. The example uses a PostgreSQL service definition called `tokencraft_migration` with `sslmode=verify-full`. Keep the password in your secret configuration. The grant script is included in the source checkout and native package. When deploying only the runtime image, extract its matching copy before running `psql`.

```sh
docker run --rm --network none --entrypoint cat tokencraft-gateway:0.1.0 \
  /opt/tokencraft/deploy/postgres/grant-runtime.sql > grant-runtime.sql
```

Use `-f grant-runtime.sql` below when using the extracted copy.

```sh
FOUNDATION_ENV=production admin-cli migrate
psql -X "service=tokencraft_migration" -v runtime_role=tokencraft_runtime \
  -f deploy/postgres/grant-runtime.sql
```

The [grant script](../deploy/postgres/grant-runtime.sql) grants access to the owner's application tables, views, sequences, and invoker functions. It gives the runtime role read-only access to the migration ledger and any schema compatibility declarations. Reapply it after each migration before sending traffic to the new replicas. Configure the API and worker with the runtime login in `DATABASE_URL`. Keep the migration login in the one-off deployment job.

The Render launcher defaults `TOKENCRAFT_RENDER_MIGRATE_ON_START` to `false`. Apply the image's migrations and runtime grants in a separate step, then start the service with the runtime login. The gateway opens its listener after confirming schema compatibility. Use `/ready` as the Render health check.

For a new disposable Render demo, set `TOKENCRAFT_RENDER_MIGRATE_ON_START=true`. Startup can initialize an empty `public` schema and can verify an already compatible schema. Upgrading an existing database uses `admin-cli migrate` during the [deployment cutover](#cut-over-without-schema-overlap).

## Shared artifact storage

Each API replica permits eight concurrent warm downloads of generated media, including generated workflow outputs. It reserves two additional slots for retained media and two for workflow tool outputs. The replica holds each slot until it releases the response buffer. When a limit is reached, retry the same download after the two-second `Retry-After` from the `503 artifact_download_capacity` response.

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

Run the drain check against the deployment database with the candidate catalog and the candidate binding environment. Include `TOKENCRAFT_WORKFLOW_TOOL_BINDINGS` and the referenced service token variables if the deployment uses workflow tools. Include `TOKENCRAFT_DEMO_RATE_FEED` when the candidate uses a reviewed rate feed. `DATABASE_URL` can use a role with read access to requests, provider runs, connector completion state, provider credential state, workflow runs, and routing policies. Use verified PostgreSQL TLS for a remote database. The command reads a point-in-time snapshot in a read-only transaction.

```sh
api-gateway catalog check-drain --catalog catalog.next.json
```

The JSON report gives the candidate base revision and counts by offer, request phase, and pinned binding digest. It also reports `checked_workflow_tool_references` and `workflow_tool_blockers`. One reference represents an active, unexpired run and one pinned tool operation revision. An active run can contribute more than one reference. The check uses the saved graph, including tool steps the run may already have completed. It reports counts without account, project, or run IDs, and checks configuration without contacting tool services. A blocker makes the command exit nonzero. Wait for outstanding work with the previous workers in place, then rerun the check close to cutover.

`checked_workflow_generation_references` and `workflow_generation_blockers` cover saved generation requirements in those same runs. One reference represents an active run and one distinct saved generation requirement. The checker uses the candidate bindings, first-frame support, fixed dimensions or token limits, current routing policy, and reviewed rates. For dynamic values it looks for a possible compatible workload and checks the minimum cost implied by the saved settings. For text, PostgreSQL returns the byte lengths of fixed messages. The checker receives no message content or workflow inputs. Provider credentials, temporary availability, and the resolved workload are evaluated again during execution.

| Blocker | What to preserve |
| --- | --- |
| `queued_offer_changed` | Keep the original offer and its active binding until queued requests dispatch. The check uses the saved provider, upstream model, and binding identity. Rate-only changes preserve queued pins. |
| `binding_unavailable` | Keep the original binding for in-flight work, submission recovery, and eligible rechecks. For queued media, a retained `drain_only` entry serves polling and recovery after the original offer stops accepting new work. |
| `legacy_adapter` | Keep a worker that supports the original built-in adapter until its outstanding work finishes. |
| `tool_binding_unavailable` | Keep the original workflow tool binding. Its name, revision, origin, and manifest determine its saved digest. |
| `tool_scope_unavailable` | Retain the account and project authorization required by the active workflow run. |
| `tool_operation_unavailable` | Retain the pinned tool operation and revision in the reviewed manifest. |
| `generation_route_unavailable` | Retain a usable route for the saved model and workload in the affected projects. Check active bindings, connector capabilities, policy, and estimated cost against the step's cap. |

When replacing a queue connector, provision a new binding name. Keep the old origin, revision, and completion settings available until the old work drains. A drain-only worker polls and recovers existing jobs on that binding. Workers with the active binding continue to claim submissions during cutover. Continue to coordinate admissions during cutover and review provider access and pricing for the candidate. [Connector protocol](provider-connectors.md#queued-media-protocol-version-1)

For a workflow tool, a `drain_only` binding remains eligible for admitted runs. Upgrade all workflow workers before rolling out different tool registries across the worker pool. Workers claim live runs only when their local configuration authorizes every pinned tool binding for that run's project. Any workflow worker can expire a due run at its deadline. Publish the revised workflow against the replacement binding before resuming new runs. [Workflow tool bindings](media-tools.md)

## Move from a deployment with built-in provider adapters

The migration history keeps the original SQL files and checksums. The core workers claim new `connector_queue` and `openai_compatible_chat` runs. Old jobs carry their original adapter and binding, so drain them using the previous worker and credential configuration before cutting over. Create fresh bindings and catalog offers for the plugin deployments. Use stable binding names and revisions while queued jobs are in flight.

## Routing policy precedence

Provider preferences take the first configured value from the request, project, or organization. They apply to named models and to `model: "auto"`. Eligible offers are ranked by provider preference first and then by the configured ranking within that group. Send `preferred_providers: []` to rank by the lowest estimate. Automatic model selection also resolves minimum community quality and recent usage from the request, project, or organization. Hard budgets, provider allowlists, and data-handling requirements still apply.

Reliability and settled-cost ranking use up to 100 matching outcomes per offer from the previous seven days in the same project and client scope. They match the workload, accepted binding, upstream model, served offer, and saved output shape. Both strategies require 20 samples per eligible offer in the group. Settled-cost ranking uses completed UTC days and compares outcomes served on the originally quoted offer at the current estimate. It requires a settlement for each sample, comparable output shapes, and at least one completion in the group.

On scoped media endpoints, set `model: "advise"` to choose a concrete model using the gateway's catalog, eligibility policy, and scoped ranking. The project or organization can also enable this path when callers omit the model. To submit an advised preview, use the model returned in `input.model` and its `quote_id`.

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

Each provider worker has four in-flight task slots shared by submissions, polling, and connector receipt lookup. Workers rotate the first choice among these queues when they claim work. A worker with multiple destinations limits each destination to three slots so another destination can keep serving during a stall. Direct connector and chat bindings with the same scheme, host, and port share that limit, including during a binding rollout. A compute stack counts as one destination. A worker with one destination can use all four slots. These limits apply per worker process.

With HAProxy 3.2+ available, run `python3 deploy/haproxy/smoke.py` in a disposable local environment. It checks configuration parsing, two fixture API replicas, readiness withdrawal and recovery, forwarding headers, SSE streaming, and a submission whose upstream disconnected after receiving the body. [HAProxy configuration reference](https://docs.haproxy.org/3.2/configuration.html)

The gateway CI rehearses live DNS discovery and chat streaming with HAProxy 3.2, two TokenCraft API processes, and a worker on shared PostgreSQL 17. The fixture publishes a new SRV record while a stream is open, then stops the replica carrying it. The surviving replica must replay the same request ID and retained chunk, then deliver the final chunk, usage, and `[DONE]`. The rehearsal also checks sibling-client isolation, one upstream inference call, and removal of the retired replica from discovery. Run the same check locally with HAProxy 3.2 or newer and PostgreSQL 17 on the path.

```sh
python3 -B scripts/smoke_stream_failover.py target/debug/api-gateway target/debug/admin-cli
```

Repeat in staging with the deployment's private resolver and HTTPS ingress, and exercise loss of a proxy or zone under load.
