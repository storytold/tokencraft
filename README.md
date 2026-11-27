# TokenCraft

TokenCraft is ArtCraft’s open-source AI provider aggregator. Route image, video, audio, language model, and 3D requests through one gateway. Compare prices, configure routing, and trace requests across providers. The Rust gateway handles accounts, project keys, quotes, budgets, asynchronous requests, and artifact delivery. Providers connect through versioned HTTP bindings and operator-reviewed catalog entries.

The [ArtCraft reference catalog](catalog/artcraft-v1.json) describes supported provider plugins, model capabilities, and supplier list prices for each snapshot. A self-hosted gateway loads the operator's own catalog. Start with your own models or import selected ArtCraft presets with local bindings and rates. [Catalog setup](docs/provider-connectors.md#curate-your-own-catalog)

[TokenCraft Operations](deploy/monitoring/README.md) is the operator console for self-hosted cells, provider plugins, compute targets, queues and delivery.

## Backend design

[Open the interactive design source](frontend/public/design/index.html). The console build serves it at `/design/`, with shareable links such as `/design/#overview/caller`. The diagrams identify observed code and proposed hosted architecture.

Follow the [API schema walkthrough](docs/api-schema.md) and use the console’s `/docs/api` for a searchable reference with one page per operation.

Build the browser console from `frontend/` with `npm ci` and `npm run build`. For Vercel, use `frontend` as the project root. The Vite build includes the design page and the application routes.

## Quickstart

Use a [native package](docs/setup.md#start-from-a-native-package) to run the bundled launcher, or use the steps below to build and run from source.

Install Rust using the toolchain in [rust-toolchain.toml](rust-toolchain.toml) and the PostgreSQL 17 server tools. Put the server's bin directory or its `pg_config` on `PATH`.

```sh
cargo run --locked -p tokencraft -- dev
curl -fsS http://localhost:4000/ready
curl -fsS http://localhost:4000/v1/catalog/models
```

The launcher starts a private PostgreSQL cluster, applies migrations, starts the gateway, and runs the bundled synthetic connector. Commercial providers are installed as separate services and appear after their bindings and offers are added to the deployment catalog. The example generates synthetic PNGs without provider credentials, Python, or Node.js. Press Ctrl+C to stop the stack.

- [Set up the gateway and submit an image](docs/setup.md)
- [Start with your own catalog and provider bindings](docs/setup.md#start-with-a-reviewed-catalog)
- [Browse all documentation](docs/README.md)
- [Report a security issue](SECURITY.md)
