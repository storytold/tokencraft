# Local setup

The Rust quickstart supports macOS and Linux. Install the PostgreSQL 17 server tools and Rust with rustup. Put the server's bin directory on `PATH`. The launcher looks for `postgres`, `initdb`, `pg_ctl`, `pg_isready`, and `createdb` in the same directory. It can also find the directory through `pg_config --bindir`.

## Backend

From the core repository root, run:

```sh
cargo run --locked -p tokencraft -- dev
```

The gateway defaults to `http://localhost:4000`, PostgreSQL to `127.0.0.1:55439`, and the bundled connector to `127.0.0.1:4019`. The launcher stores local data in `.tokencraft-dev/`. Use `--state-dir` to choose a new location or reopen an existing private state directory. The same command accepts `--api-port`, `--db-port`, `--connector-port`, and `--web-port`.

First start migrates PostgreSQL and builds the gateway against that schema for SQLx. For an offline start with cached dependencies, pass `--offline` to both Cargo and the dev command. For a prebuilt installation, put matching `tokencraft` and `api-gateway` binaries in the same directory on `PATH`, then run `tokencraft dev`.

### Start with Docker Compose

From the core repository root, build and start the demo using Docker Engine 28 or newer and Docker Compose. The image builds through the same packaging check as the native distribution. At runtime it starts PostgreSQL 17, the gateway, and the synthetic image connector on a single machine. [Docker port publishing](https://docs.docker.com/engine/network/port-publishing/)

```sh
docker compose -f deploy/container/compose.yaml up --build --wait
curl -fsS http://localhost:4000/ready
bash examples/connector/smoke.sh http://localhost:4000
```

The last command uses the [local smoke test tools](#submit-an-example-image-from-the-terminal).

The host publishes the API on IPv4 loopback. PostgreSQL and the connector listen on loopback inside the container. The named volume holds accounts, requests, artifacts, and the launcher's generated keys. `docker compose -f deploy/container/compose.yaml down` stops the demo and leaves that volume available for the next start.

To add the browser console, first [compile the frontend](#browser-console). Pass the absolute path to its `dist` directory as a read-only mount and open [localhost:3000](http://localhost:3000).

```sh
TOKENCRAFT_CONSOLE_DIR=/absolute/path/to/dist \
  docker compose -f deploy/container/compose.yaml -f deploy/container/console.compose.yaml up --build --wait
```

For an operator catalog and externally managed PostgreSQL, use the [gateway runtime image](deployment.md#build-the-gateway-runtime-image).

### Start from a native package

Choose the `.tar.gz` for your OS and architecture and keep its `.sha256` file alongside it. The archive includes the matching launcher, gateway, migration tool, OpenAPI contract, public pricing catalog, and deployment files. It also includes the [optional media worker](../plugins/tools/media/README.md) with the [workflow](workflows.md) and [media setup](media-tools.md) guides. Install the PostgreSQL 17 server tools before starting the launcher. [PostgreSQL for Ubuntu](https://www.postgresql.org/download/linux/ubuntu/) · [PostgreSQL 17 for macOS](https://formulae.brew.sh/formula/postgresql%4017)

```sh
shasum -a 256 -c tokencraft-0.1.0-darwin-arm64.tar.gz.sha256
tar -xzf tokencraft-0.1.0-darwin-arm64.tar.gz
cd tokencraft-0.1.0-darwin-arm64
./bin/tokencraft dev
```

Use `sha256sum -c` on systems with GNU coreutils. From a second terminal in the package directory, run `bash examples/connector/smoke.sh` to run the synthetic image walkthrough. To start with your own catalog, use `./bin/tokencraft dev --profile ./reviewed-models`.

Archives ending in `-console.tar.gz` include a `console/` directory. Run `./bin/tokencraft dev --console-dir ./console` and open the localhost console URL printed by the launcher. The same command accepts `--profile` for a reviewed catalog. Use `--web-port` when the default port 3000 is occupied.

Maintainers can run the **Native packages** workflow to build Ubuntu x86-64 and Apple Silicon archives as workflow artifacts. The workflow records the source commit in `VERSION.json`. To build on your own host, install the pinned Rust toolchain, PostgreSQL 17 and Python 3.12 or newer, then run the packaging script from the source checkout. It builds against disposable PostgreSQL, tests the extracted package, and writes the archive and checksum to your chosen directory. The package test uses Bash, curl, jq, OpenSSL, and a SHA-256 command. [Workflow artifacts](https://github.com/actions/upload-artifact/blob/v4/README.md)

```sh
python3 scripts/native_package.py --output ./native-packages
```

### Submit an example image from the terminal

In a second terminal, run the smoke test against the running gateway. It needs Bash, curl, jq, OpenSSL and `shasum` or `sha256sum`.

```sh
bash examples/connector/smoke.sh http://localhost:4000
```

The smoke test creates a local account, project, and service principal. It calls `POST /v1/image` for `example_image`, polls the returned `status_url`, validates the downloaded PNGs, and restores a [sealed event export](event-archives.md). Run it against a disposable dev directory to keep the generated account and images out of a shared database.

### Measure local connector capacity

Start `tokencraft dev` with a disposable state directory and the bundled connector. From the core checkout, run the bounded load check with Python 3.

```sh
python3 -B examples/connector/load.py --requests 128 --concurrency 24
```

The check provisions a project and submits synthetic image requests for two client references using one key. It reports admission and observed completion latency separately, samples the project queue, and verifies the filtered history and usage totals after the queue drains. It polls with the response's `Retry-After` and `ETag` headers when present, so observed completion includes the polling interval. Compare runs with the same concurrency, fixture, hardware, and build profile.

### Start with a reviewed catalog

Run the same local gateway with a catalog you curate and provider services you already started. Put the reviewed files in a directory separate from dev state.

| Profile file | Contents |
| --- | --- |
| `catalog.json` | Required version 1 [routing catalog](provider-connectors.md), with your model IDs, capabilities and rates. |
| `connector-bindings.json` | Queue connector bindings for services such as fal and Kinovi. |
| `chat-bindings.json` | OpenAI-compatible chat bindings for an OpenRouter bridge or a local model. |
| `rates.json` | [Reviewed rate feed](deployment.md#refresh-reviewed-rates) for the configured offers. Required if the catalog sets `rate_feed_offers`. |

Set the service credentials referenced by the two binding files in the shell. Keep vendor credentials in their provider services. Use the [binding preflight](provider-connectors.md#check-catalog-bindings) to check the profile's files before starting the gateway. Use the prebuilt launcher to start the reviewed profile.

```sh
tokencraft dev --profile ./reviewed-models --state-dir ./.tokencraft-reviewed
```

The profile starts a private PostgreSQL cluster and gateway. It uses the supplied connector services in place of the bundled image connector. The launcher reads the catalog and binding files once at startup and supplies the referenced binding credentials. Restart to apply catalog or binding changes or to add a rate feed to a profile that started without one. When `rates.json` is present at startup, the gateway checks it every 15 seconds. Use an atomic file replacement after validating it with `catalog check-rate-feed`. Reuse the state directory to keep accounts, encryption keys, and request history. Let in-flight work finish before retiring its original binding.

Check the loaded models at `GET /v1/catalog/models`. Use `GET /v1/catalog/models/scoped` with a project credential that has `requests:submit` to discover private models shared with that project. [Import ArtCraft presets into the catalog](provider-connectors.md#prepare-offers-from-the-artcraft-catalog)

### Browser console

The browser console is built from the frontend checkout. For a console you can run with the native launcher, build the frontend from its `frontend/` directory with Node.js 22.

```sh
npm ci --ignore-scripts
VITE_TOKENCRAFT_DEMO=true VITE_TOKENCRAFT_PACKAGED=true VITE_API_BASE_URL= npm run build
```

Point `tokencraft dev --console-dir` at the resulting `dist/` directory. The launcher serves it on the web port and supplies the active API port to the frontend. Use that compiled directory with `scripts/native_package.py --console-dir ./path/to/dist` to include the console in a native package.

For frontend development, run these commands from `frontend/` with the gateway already running.

```sh
npm ci --ignore-scripts
VITE_TOKENCRAFT_DEMO=true VITE_API_BASE_URL=http://localhost:4000 \
  npm run dev -- --host 127.0.0.1 --port 3000 --strictPort
```

Open [local sign-up](http://localhost:3000/signup). Use `localhost` for both browser-facing URLs so the session cookie works across ports. If you change Vite's port, pass the same `--web-port` to `tokencraft dev`. Provider inventory and operational views use the gateway's configured offers. The browser console checkout and its distribution are separate from this backend repository.

In the signed-in Playground, choose **Compare routes** to compare prices for the selected project. The model picker includes custom models shared with that project. Each route shows its price deadline when it uses a reviewed rate feed. When a quote expires, choose **Refresh quote** to see the currently eligible routes before submitting. Submit from the comparison to follow the request trace in the same workspace.

## Back up and restore dev state

Use the native launcher to save a logical PostgreSQL dump together with the local encrypted artifacts, event archives, connector job records, and the keys needed to reopen them. Finish queued and running requests and stop the launcher first.

```sh
tokencraft backup .tokencraft-dev ./tokencraft-backup
tokencraft restore ./tokencraft-backup ./restored-dev
tokencraft dev --state-dir ./restored-dev
```

Use `cargo run --locked -p tokencraft --` before the subcommand when running from source. The commands use `pg_dump` and `pg_restore` from the selected PostgreSQL server installation. Use matching PostgreSQL versions for the source and recovery environment.

Use a new destination each time. Restore verifies the file checksums before loading the database in one transaction. A successful restore makes the state available at the new path, and the next `dev` launch applies any pending migrations. Keep backups in private storage and restore from trusted copies. The backup holds the keys that protect local content as well as the database.

Keep the reviewed profile and its service credentials with your deployment configuration. Use `dev --profile` to reopen restored state with the same bindings.

These commands cover state managed by `tokencraft dev`. Restore recovers the contents as of the backup. For a separately deployed gateway, coordinate PostgreSQL, object storage, connector state, and secret recovery with the deployment's retention and content-removal records.

### Keep later content removals during recovery

Capture a checkpoint from the latest stopped state. Keep it with your recovery materials and pass it when restoring an older backup.

```sh
tokencraft checkpoint .tokencraft-dev ./content-removals
tokencraft restore ./tokencraft-backup ./restored-dev --content-removals ./content-removals
```

The checkpoint authenticates removal records with the original state's payload key. Restore applies the records to matching requests, preserves their original removal timestamps and event IDs, and rebuilds the PostgreSQL cluster from a scrubbed logical dump. The new state retains request history and usage and excludes removed local media bytes. A checkpoint covers the removals recorded when it was captured.

## Connect a provider

Use [provider bindings](provider-connectors.md) to add queue-based media or OpenAI-compatible chat services. [Deployment](deployment.md) covers starting the gateway with your catalog instead of the bundled example.

Create and test reviewed field translations with the [provider mapping guide](provider-mapping.md).
