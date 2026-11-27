# TokenCraft Operations

Run this console from a native package or source checkout on an operator host. One collector can observe multiple TokenCraft cells, provider plugin instances, self-hosted compute targets, and GPU exporters. The web console and its API run on their own loopback listener and use a dedicated operator credential. The runtime ships from this directory.

## Start the console

The collector uses Python 3.11+ with SQLite JSON support. DB sampling also needs the PostgreSQL client. A probes-only install can omit the database and use the same dashboard and incident history. Run one collector per state directory.

1. Copy [settings.example.json](settings.example.json) to a private configuration directory. Set the cell names, endpoints and providers to the values in your gateway catalog and bindings. Remove entries for services you do not operate. Use the actual gateway, worker, plugin and compute addresses.
2. Set a strong, unique URL-safe value of at least 32 characters for each operator and probe credential. For the dashboard set **TOKENCRAFT_MONITOR_PASSWORD**. For the gateway and worker use an independent 32-byte hexadecimal **TOKENCRAFT_CAPACITY_TOKEN**. Each plugin uses its own **TOKENCRAFT_PLUGIN_MONITOR_TOKEN** in the plugin process and the matching named **token_env** in the monitor process. Credentials for the dashboard, capacity probes, plugin probes, and provider submissions are separate. Prefix environment variable names where appropriate to keep cells and replicas independent.
3. To enable PostgreSQL sampling, have the database owner provision an unprivileged LOGIN role and apply the aggregation function after the gateway migrations. Configure a matching libpq service for the monitor process. Use a private passfile to hold database credentials.

   ~~~shell
   psql -X -w -d service=tokencraft_migration_owner \
     -v monitor_role=tokencraft_monitor_login \
     -f deploy/monitoring/install.sql
   ~~~

   Point **PGSERVICEFILE** at a file that your monitor service account can read. A remote service entry should use certificate verification. The collector imposes a statement and client timeout for every sample. The function returns aggregates and the monitor login needs no access to gateway tables.

   ~~~ini
   [tokencraft_ops_cell_a]
   host=db.internal.example
   dbname=tokencraft
   user=tokencraft_monitor_login
   sslmode=verify-full
   sslrootcert=/etc/tokencraft-monitor/ca.pem
   passfile=/etc/tokencraft-monitor/pgpass
   connect_timeout=3
   ~~~

4. Validate the environment and start the separate process from the checkout.

   ~~~shell
   python3 -B deploy/monitoring/monitor.py --config ./ops.json --check-config
   python3 -B deploy/monitoring/monitor.py --config ./ops.json --state-dir ./ops-state
   ~~~

   The default listener is [http://127.0.0.1:9417](http://127.0.0.1:9417). Sign in as **operator** with **TOKENCRAFT_MONITOR_PASSWORD**. For remote teams, use a private HTTPS proxy reached through your operator network. Proxy this listener on its own hostname. Use the supplied [systemd unit](tokencraft-monitor.service) as the Linux service entry point. Point its configuration and secret file at operator-owned files. Reconfigure and restart the collector to change inventory.

## Attach providers and infrastructure

| Kind | What it measures | Wiring |
| --- | --- | --- |
| **gateway**, **worker**, **combined** | Process readiness, DB/schema/worker/storage checks, occupied SSE or provider slots and available slots | Add each replica's authenticated **GET /internal/capacity** URL directly. Keep that path off public ingress. |
| **plugin** | Process reachability, in-flight work, throughput, HTTP outcomes and successful response duration | Add **GET /internal/monitoring** for fal, Kinovi, OpenRouter, or a third-party plugin implementing the same contract. |
| **upstream** | A read-only provider health or catalog API as observed by the collector | Configure a supported GET URL and dedicated credential if needed. With **expected_model**, the endpoint must return an OpenAI-compatible JSON model listing containing that exact ID. |
| **compute** | A specific model server's reachability and, with PostgreSQL, actual TokenCraft attempt outcomes and response-header timing | Use the binding's stack name, target ID, home cell and catalog provider in the inventory. Prefer **GET /v1/models** with **expected_model** when the deployment supports it. |
| **gpu** | GPU count, max utilization and temperature, and summed used/free framebuffer memory | Attach an NVIDIA DCGM Exporter **GET /metrics** URL to a compute target by **gpu_for**. Expose it on a private network. The collector reads bounded physical GPU series and omits device identifiers. |

All probes use GET, enforce verified HTTPS for remote endpoints, and decline redirects. For local development, literal loopback HTTP is supported. The plugin endpoint uses **Authorization: Bearer** with the plugin-specific monitor token. Wrong or missing tokens return **401** when enabled. A plugin should advertise readiness only after its local server and required state are initialized. The probe credential is independent of the gateway's submission and polling credential. The endpoint performs no provider call.

A reachable plugin indicates that its process answered the probe. Provider execution evidence comes from actual gateway outcomes and target attempts. Configure a separate upstream probe where the provider exposes a suitable read-only endpoint. A provider with no observed traffic has no measured success percentage. A compute attempt that begins returning HTTP headers may still fail downstream. The result and publication metrics capture later outcomes.

#### Import a deployed compute profile

Set **compute_profile** on a cell to read the same catalog and chat bindings that the gateway uses. The binding file's home cell must match the monitor cell ID. Relative file paths resolve beside the monitor config. The collector discovers each compute stack used by an allowlisted catalog provider and uses the configured stack, target, cloud, region and zone. It checks each advertised upstream model at **/v1/models**. Direct chat bindings and provider plugins can still be configured as explicit targets.

Start with the repository's [compute monitoring example](settings.compute.example.json). It already corresponds to the [primary and standby compute binding](../../plugins/providers/compute/examples/two-target-chat/chat-bindings.json).

~~~shell
python3 -B deploy/monitoring/monitor.py --config deploy/monitoring/settings.compute.example.json --check-config
python3 -B deploy/monitoring/monitor.py --config deploy/monitoring/settings.compute.example.json --state-dir ./ops-state
~~~

Set **TOKENCRAFT_MONITOR_PASSWORD** before starting the second command. The example detects compatible model servers on ports 4031 and 4032 as they come online. Start the endpoints with your platform's provisioning tools. Add the PostgreSQL source from the main configuration example to see recorded gateway traffic and fallback across those endpoints.

If a target uses a different health route or needs probe authentication, add a **probes** entry inside **compute_profile**. Match the **provider**, **stack**, and **target**. Set **id** to give it a permanent alias, such as for a GPU exporter's **gpu_for** link. Set **url** or **token_env** as needed. The importer never reads or inherits the bindings' inference **key_env** values. A probe on **/v1/models** checks catalog models by default. Use **verify_models** to choose whether an alternate URL returns a compatible model listing. A reachable listing with a missing model reports degraded health. Keep the generated identity or its explicit alias stable to preserve its history across deploys.

#### Provider plugin telemetry contract

Health-only plugins can return **204 No Content**. The collector sends **Accept: application/json**. Plugins with traffic telemetry respond **200** to that request with this versioned shape. A request without JSON negotiation can still use a bodyless **204** to check health.

~~~json
{
  "version": 1,
  "boot_id": "66b50c1b-e058-4282-8067-5109c376abcd",
  "operations": {
    "submit": {
      "in_flight": 0,
      "responses": {"ok": 2, "throttled": 1, "client_error": 0, "server_error": 0, "interrupted": 0},
      "latency_ms": {"buckets": [0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0], "sum": 283.5}
    }
  }
}
~~~

Choose the supported operation keys that the plugin implements from **submit**, **status**, **artifact**, **lookup**, and **chat**. Generate a new **boot_id** when the process starts or its cumulative counters reset. The duration buckets are disjoint, ordered with upper bounds of **25, 50, 100, 250, 500, 1,000, 2,500, 5,000, 10,000, 30,000, 60,000, and 120,000 ms**, followed by overflow. The sum and bucket counts include completed HTTP 2xx responses, including complete streams, and must agree with **ok**. Record a stream that closes early as **interrupted**. The plugin sends no request details or dynamic labels.

The dashboard compares consecutive samples from the same process to show response rate and successful p95 by operation. A new process or observation gap leaves the interval unknown. Plugin HTTP 2xx confirms transport handling even when a status response reports a failed job. Use the provider outcome panel for completed generation success.

## Read the operational views

Use **Compute** to scan live health, open incidents, and recent performance. Filter targets by name or health, or select a target to open its probe history. Use the range buttons and hover over the chart or focus it and use the arrow keys to inspect a sample. The **Scope** filter controls database-backed views while fleet probes remain cell-wide. **Auto-refresh** can be paused during triage. The console remembers its light or dark theme locally.

- **Compute** shows current replica readiness and capacity, plugin and endpoint health, GPU readings, provider outcomes, and compute attempts. Latency and outcome rows are a rolling cohort of gateway admissions within the last 15 minutes, bounded by the most recent 10,001 records in the selected scope. The queue timing covers admission to recorded dispatch. Queued provider time covers upstream acceptance to TokenCraft observing the result, so it includes upstream queue and observation delays. Total time measures admission to a successful usable result.
- **History** stores timestamped probe states and performance observations in the local SQLite state directory. Chart points are downsampled for longer ranges. The displayed healthy percentage counts all recorded known-state probes in the period. Unknown and missing observations represent gaps in coverage. Local retention defaults to seven days.
- **Queues** tracks active requests by provider, workload, state and dispatch policy. The eligible age uses the known worker bindings, excludes scheduled delays, paused dispatch and provider cooldown, and samples up to 256 claimable jobs. Coverage is labeled current, cached or unknown. Brief worker outages can retain a recent binding observation for diagnosis.
- **Workflows** shows queued, running, and reconciling depth, due runs, and past-deadline runs. It includes the last 24 hours of terminal outcomes and p95 successful duration. **Delivery** shows artifacts observed, pending, published or unconfirmed, plus webhook and export backlog. The active queues and terminal workflow counts each cap at 5,001 records and show a cap indicator. Oldest ages refer to the sampled records.
- **Scope** switches between the whole cell and configured aggregate detail views. Add **scopes** to a PostgreSQL-backed cell with **id**, **account**, and optional **project**. For example, **{"id":"studio","account":"acct_example","project":"gprj_example"}** adds a Studio project view. The console exposes the alias. It never displays or emits the account or project mapping. Fleet probes and database capacity remain cell-wide.

The incident log persists confirmed failures and recoveries. Alerts include probe outage, missing telemetry, database headroom, aging dispatch-ready work, provider failure rate with a sample minimum, overdue workflows, pending artifacts and delayed webhooks. Consecutive checks suppress transient noise. Missing observations cannot prove recovery.

The authenticated **GET /metrics** serves Prometheus text metrics with bounded dimensions. Rolling database counts and percentiles are gauges. Plugin responses are cumulative counters, and successful response durations are histograms. Apply the [scrape example](prometheus.example.yml) and [alert rules](tokencraft-monitor.rules.yml) to route incidents and collector freshness alerts through an existing Prometheus and Alertmanager deployment. The included status page and incident history work independently. **GET /healthz** indicates whether the collector is producing fresh observations.

## Feed the replica controller

Use [autoscaling_snapshot.py](autoscaling_snapshot.py) to reuse the same monitoring sample for the [existing replica controller](../../docs/capacity.md). Its external source file uses the deployment observations documented there. For **api** and **worker**, provide a **replicas** array of monitor target IDs in place of **probe_urls**. Keep **desired**, **stable**, **deployment_version**, **api.cpu_fraction**, **reserve_connections**, **database_healthy**, **provider_headroom**, **cell**, and **observed_at**.

~~~shell
python3 -B deploy/monitoring/autoscaling_snapshot.py --state-dir ./ops-state --source source.json > snapshot.json
python3 -B deploy/autoscaling/controller.py --config policy.json --snapshot snapshot.json --state plan-state.json
~~~

The adapter checks current split replica inventory, distinct authenticated process instance IDs, matching worker binding revisions, and fresh observations. The controller uses a digest of each role’s process roster to restart its pressure window after replacement. Deployment health, CPU and supplier headroom still come from authoritative platform observations. A reachable provider or passing model probe cannot establish spare provider capacity. The adapter only emits observations. The existing controller owns scale decisions and actuation.

## Migrate the earlier operations view

Old Setup and Providers Operations browser links return users to the Providers page. The account's dispatch policy is still editable there by authorized owners and administrators. For existing operator API clients, **TOKENCRAFT_ENABLE_LEGACY_OPERATIONS=true** on a privately routed gateway temporarily mounts the three previous monitoring reads at **/v1/setup/status**, **/v1/operations/queues**, and **/v1/operations/artifact-publication**. They retain their existing account authorization. The console uses the new private probes and aggregated sampler, and these compatibility paths are omitted from the consumer API reference.

Use the new dashboard to check worker and provider failures in staging. A controlled worker pause should raise the eligible age while an intentionally unavailable fixture plugin should produce its own availability incident. Confirm that recovery resolves both after good samples. During a rollout, test alerts, fresh database sampling, steady-state database load and the deployment's own access boundaries.

The source checkout's `scripts/smoke_workflow_contract.py` exercises this console with a fresh PostgreSQL database and matching compiled gateway and admin binaries. It submits local chat fixture traffic, forces a pre-execution throttle on the primary, and checks PostgreSQL attribution and model probes for the standby and a second provider. It also checks that dashboard reads are disabled on the default gateway and excluded from the consumer contract.
