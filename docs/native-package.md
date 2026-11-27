<!-- The packager installs this file at the root of the archive. -->

# TokenCraft native package

TokenCraft routes requests across AI providers and records the quotes, execution history, and usage for each project. This archive contains the launcher, gateway, migration tool, and a synthetic image connector.

## Run the demo

Install the PostgreSQL 17 server tools and put their bin directory or `pg_config` on `PATH`. From the extracted package directory, start TokenCraft.

```sh
./bin/tokencraft dev
```

In a second terminal, check readiness and submit a synthetic image. The walkthrough uses Bash, curl, jq, OpenSSL, and a SHA-256 command. It creates a fixture account and project in the local database.

```sh
curl -fsS http://localhost:4000/ready
bash examples/connector/smoke.sh http://localhost:4000
```

The demo starts a private PostgreSQL database and stores its data and encryption keys in `.tokencraft-dev/`. Stop it with Ctrl+C. Use `--state-dir` to choose a location or reopen the same state. [Back up and restore dev state](docs/setup.md#back-up-and-restore-dev-state)

If this archive includes a `console/` directory, start the launcher with `./bin/tokencraft dev --console-dir ./console` and open the localhost URL printed by the launcher.

## Configure your gateway

From the package directory, run `export PATH="$PWD/bin:$PATH"` before following guides that invoke TokenCraft commands by name.

- [Run a reviewed catalog with your own models and bindings](docs/setup.md#start-with-a-reviewed-catalog)
- [Compare provider prices in the ArtCraft reference catalog](catalog/artcraft-v1.json)
- [Run fal, Kinovi, OpenRouter, or your own compute endpoints](plugins/providers/README.md)
- [Preview and validate provider mappings with the bundled CLI](docs/provider-mapping.md#try-the-example)
- [Deploy, check migrations, and roll out changes](docs/deployment.md)

## Develop and operate

- [Follow a request from quote to delivery](docs/api-schema.md)
- [Read the OpenAPI contract](contracts/openapi.json) and [connector primitives](contracts/connector-primitives.json)
- [Browse the documentation](docs/README.md) and [interactive backend design](frontend/public/design/index.html)
- [Inspect the Operations console](deploy/monitoring/README.md)
- [Get SDK and gateway source](https://github.com/storytold/tokencraft)
