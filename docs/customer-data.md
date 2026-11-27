# Export project data to PostgreSQL

TokenCraft exposes a versioned event feed for request and usage metadata. It includes request state, model and offer IDs, provider lineage where reported, token counts, and exact costs in nano-USD. Events can carry an opaque client reference and an app reference. The feed excludes prompts, response bodies, credentials, and artifact URLs.

The [reference importer](../examples/postgres/export_to_postgres.py) runs separately with Python 3.11 or later and the PostgreSQL client `psql`. Its only access to the gateway is a project key scoped to `exports:read`. The destination credentials stay with the importer.

## Set up the destination

1. Point libpq at your destination with `PGHOST`, `PGPORT`, `PGDATABASE`, and `PGUSER`. Use your normal libpq credential configuration, such as `PGPASSFILE`. The initial destination role needs permission to create the `tokencraft_export` schema and tables.
2. Set `TOKENCRAFT_EXPORT_API_URL` to the gateway address. This defaults to `http://localhost:4000`; remote gateways use HTTPS. Set `TOKENCRAFT_EXPORT_PROJECT` to the project ID and supply the scoped key through `TOKENCRAFT_EXPORT_API_KEY`.
3. For a client-only destination, issue a client-bound key and set `TOKENCRAFT_EXPORT_CLIENT_REF` to that client. A project-wide key can also explicitly filter by client. Use a separate destination database or PostgreSQL grants when consumers require isolation from other imported feeds.

Run these commands from the repository root.

```sh
python3 -B examples/postgres/export_to_postgres.py
python3 -B examples/postgres/export_to_postgres.py --status
psql -X -w -v ON_ERROR_STOP=1 -f examples/postgres/postgres_usage_views.sql
```

The first command pulls a fixed snapshot. Run it again to continue from the last committed event or pick up events published later. Each page checks the checksum and event scope, stages validated events in a session-local temporary table, then commits the events and checkpoint in one transaction. The staging table is dropped when the transaction ends. Events deduplicate by gateway URL, project ID, and event ID. Checkpoints also include the client filter. Keep the same gateway URL and scope between runs.

An event ID keeps the same payload across snapshots, the live feed, and archive restores. Identical events deduplicate when a project-wide feed overlaps a client feed. If a stored event's payload differs, the importer stops and rolls back the page and its checkpoint update together.

After preparing the tables, use a dedicated destination login for continuous sync. Run the [grant script](../examples/postgres/grant_sync_role.sql) with that login's role name. Use the import table owner with permission to grant temporary table access on the destination database.

```sh
psql -X -w -v sync_role=tokencraft_export_sync -f examples/postgres/grant_sync_role.sql
```

Point libpq at the sync login and use `--no-init`. The grants let it insert and check events, advance checkpoints, and create temporary tables in the destination. Scope reporting access separately.

## Follow the live feed

```sh
python3 -B examples/postgres/export_to_postgres.py --no-init --watch --interval 30
```

The watcher drains available pages, polls when caught up, and retries temporary failures with backoff. It waits for the longer of its local backoff and the gateway's `Retry-After` hint, which may be a delay in seconds or an HTTP date. The watcher caps a server hint at one hour. It retries from the last committed cursor after temporary failures, including interrupted responses. A complete page with a mismatched checksum or unsupported schema version fails before import. If the next events are in cold storage, restore the archive to let the watcher continue from the same checkpoint. `--status` needs only the destination connection and feed identity. It returns the last committed event ID, the last successful feed observation, the last observation with no outstanding work, and pagination and publication flags. [Recover an export or sync](event-archives.md#recover-an-export-or-sync)

Creating snapshots, polling the live feed, and downloading snapshot pages share four slots per API replica. A busy replica returns `503 export_capacity` with `Retry-After: 2`. Retry the same scoped cursor after the delay. The replica reserves the slot until it releases the response bytes.

Live feed and snapshot pages contain up to 8 MiB of complete NDJSON records, including line endings. Byte limits can shorten a page before the requested event count. Follow `X-TokenCraft-Has-More` and resume at `X-TokenCraft-Next-After`, which identifies the last complete event in the page. The PostgreSQL importer accepts this limit for both live and restored history.

To implement another destination, use the same contract.

| Operation | Endpoint |
| --- | --- |
| Follow published events | `GET /v1/exports/events` |
| Create a fixed cutoff | `POST /v1/exports` |
| Read a snapshot page | `GET /v1/exports/{id}/data` |

For the feed, pass the `project_id` and optional `client_ref` and `after`. Validate `X-TokenCraft-Content-Sha256` against the exact NDJSON bytes and verify the effective project, client scope, and event schema version. Persist `X-TokenCraft-Next-After` together with the page data. Continue paging while `X-TokenCraft-Has-More` is true. Use the last cursor for this same feed and follow publication order, which can differ from an event's occurrence time. The OpenAPI specification documents the full response headers.

## Reconcile costs

The reporting SQL installs indexes for scoped request lifecycle and cost lookups. Run it as the destination table owner. On an existing destination, PostgreSQL holds a write lock on the events table while it builds the indexes, so use a quiet sync window for a large table. Rerun the reporting SQL to add the lifecycle view to an existing importer database.

`tokencraft_export.request_usage` shows the admitted model and offer, attribution, estimate, provider observations, and latest supplier settlement for each imported request. `tokencraft_export.supplier_settlement_postings` keeps every supplier revision and its signed delta. A zero settlement stays zero, and an unknown amount stays null. Older events may have no app attribution.

`tokencraft_export.provider_charge_observations` exposes connector charges in their original billing units. They arrive as `usage.observed` events with `data.source` set to `connector_charge` and a decimal string at `data.provider_charge.quantity`. For example, Kinovi reports credits. Group amounts by provider and unit, and use the separate USD fields for dollar totals. The request and usage APIs expose the same observations while delivery is pending and after a failed request.

`tokencraft_export.request_lifecycle` shows each imported request's status and the time that state was recorded. A terminal event supersedes an unconfirmed outcome even if the unconfirmed event arrives later. The view uses the events committed to the destination, so the status reflects the importer's current checkpoint.

Join the lifecycle and charge views to report charged requests that failed to deliver an output.

```sql
SELECT lifecycle.request_id, lifecycle.client_ref, lifecycle.status_at,
       charge.provider, charge.unit, charge.quantity
FROM tokencraft_export.request_lifecycle AS lifecycle
JOIN tokencraft_export.provider_charge_observations AS charge
USING (source_url, project_id, request_id)
WHERE lifecycle.source_url = 'http://localhost:4000'
  AND lifecycle.project_id = 'proj_REPLACE_WITH_YOUR_PROJECT_ID'
  AND lifecycle.status = 'failed';
```

Summarize estimates and costs by app and client.

```sql
SELECT app_ref, client_ref, count(*) AS requests,
       sum(estimated_usd_nanos) AS estimated_usd_nanos,
       sum(provider_reported_usd_nanos) AS provider_reported_usd_nanos,
       sum(supplier_settled_usd_nanos) AS supplier_settled_usd_nanos
FROM tokencraft_export.request_usage
WHERE source_url = 'http://localhost:4000'
  AND project_id = 'proj_REPLACE_WITH_YOUR_PROJECT_ID'
GROUP BY app_ref, client_ref;
```

The destination owns retention and access for its copy. If your application stores request content separately, consume `request.content_removed` idempotently by event ID and use the source, project, and request ID to locate the content. The gateway emits the event for both administrator removal and automatic expiry of a successful provider result. The event preserves accounting metadata.

For local verification against the bundled connector, point libpq at a fresh disposable destination database and run `python3 -B examples/postgres/smoke.py`. The smoke creates synthetic requests and settlements on a loopback gateway and installs the views in the destination.
