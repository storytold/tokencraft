# Cell autoscaling and request limits

Run a small self-hosted install with one combined gateway. Hosted cells can split the API and worker roles and adopt the controller independently. Each controller owns one cell, one API deployment, and one worker deployment. PostgreSQL remains authoritative for admissions, eligibility, and leases. Deployments can place those replicas across availability zones while keeping the same cell database and artifact namespace.

[TokenCraft Operations](../deploy/monitoring/README.md#feed-the-replica-controller) can supply the probe, queue and connection signals to the replica controller from its existing sample. It uses the same deployment and provider headroom inputs as the standalone collector.

## Scale the saturated tier

| Tier | Signal | Action |
| --- | --- | --- |
| API | CPU utilization reaches 70% or concurrent SSE readers reach 75% of slots | Add one API replica after 30 seconds of sustained pressure. HAProxy admits it after DNS discovery and readiness. |
| Worker | Oldest eligible submission reaches 15 seconds, 75% of task slots are busy, and the provider has capacity | Add one worker after 30 seconds of sustained pressure. All workers continue claiming from the same PostgreSQL queue. |
| API scale-in | CPU stays at or below 35% and the API fleet has zero active SSE readers for five minutes | With scale-in enabled, remove one API replica above the minimum. Axum handles SIGTERM with graceful HTTP shutdown. Set the platform termination grace to at least 60 seconds and verify the full ingress drain. |
| PostgreSQL | High pool waits, writer latency, or depleted connection reserves | Hold replica changes when database health is degraded. Scale-out also needs headroom for another replica's pool and must fit the cell's configured total connection budget. |
| Inference and storage | Provider quotas, model queue latency, artifact delay, or network saturation | Use those tiers' own capacity or delivery controls. A blocked or cooling-down route cannot increase the eligible-worker backlog signal. |
| Ingress, zones, cells, and regions | Sustained proxy saturation, insufficient capacity after zone loss, writer saturation across tenants, or residency requirements | Expand placement or the data plane through deployment operations. A replica controller changes neither tenant ownership nor provider placement. |

The controller makes one replica change per observation. Scale-outs have a 60-second cooldown. Scale-ins have a 10-minute cooldown. Missing or stale data, replica count changes, and rollouts interrupt the pressure window. It holds while either deployment is changing. The initial worker policy scales out only, since provider, webhook, and archive loops need a coordinated drain before unattended scale-in. Keep the role minima high enough for the intended zone-loss scenario.

### Wire the controller into a hosted cell

1. Run separate API and worker deployments using the same PostgreSQL database, catalog, bindings, payload key, and artifact storage. Set an independent, generated 32-byte hex `TOKENCRAFT_CAPACITY_TOKEN` in both deployments. Collect token-authenticated `GET /internal/capacity` directly from each replica using validated HTTPS, or literal loopback HTTP for local operation. The supplied HAProxy configuration blocks public access to that path.
2. Give the collector a read-only PostgreSQL credential used through libpq `PG*` settings. The SQL sample reads eligible submissions for the deployed binding digests and excludes scheduled work, paused dispatch, and queue cooldowns. It reads a bounded sample of up to 256 eligible submissions and times out after two seconds. Use the writer or another equally current observation path. The probe supplies replica readiness, SSE occupancy, task slots, and binding digests.
3. Supply deployment state, direct replica probe addresses, CPU utilization, and database and provider health from your monitor. Mark a deployment stable after the desired generation is ready, updated, and available. For Kubernetes, use its deployment `metadata.resourceVersion` for `deployment_version`. Record `observed_at` as the oldest contributing observation timestamp in Unix seconds. Mark `database_healthy` true only with available pool headroom and healthy writer latency. Mark `provider_headroom` true only when the eligible supplier and self-hosted routes have spare capacity and their telemetry is fresh. Missing provider headroom disables worker scale-out. Use a 15-second sampling interval.

Example controller policy for a cell with at least 100 connections dedicated to these two fleets. Reserve more connections separately for maintenance, rollout surge, failover, and other users. Set the final values from the actual PostgreSQL budget and tested zone placement.

```json
{
  "cell": "cell-a",
  "context": "staging",
  "namespace": "tokencraft-cell-a",
  "connections_per_replica": 10,
  "connection_budget": 100,
  "api": {"deployment": "tokencraft-api", "min": 2, "max": 8, "allow_scale_in": false},
  "worker": {"deployment": "tokencraft-worker", "min": 2, "max": 6}
}
```

The monitor writes a fresh `source.json` with these fields. The example timestamp is a placeholder. Each URL points at one replica, and the collector rejects duplicate URLs and mixed binding revisions in the worker fleet.

```json
{
  "cell": "cell-a",
  "observed_at": 0,
  "database_healthy": true,
  "provider_headroom": false,
  "reserve_connections": 20,
  "api": {
    "desired": 2,
    "stable": true,
    "deployment_version": "8102",
    "cpu_fraction": 0.42,
    "probe_urls": ["https://api-0.internal/internal/capacity", "https://api-1.internal/internal/capacity"]
  },
  "worker": {
    "desired": 2,
    "stable": true,
    "deployment_version": "8104",
    "probe_urls": ["https://worker-0.internal/internal/capacity", "https://worker-1.internal/internal/capacity"]
  }
}
```

Use `/internal/capacity` as the probe path on API and worker replicas. Run the collector and controller in order on each tick. For custom schedulers, the collector writes a portable JSON snapshot and the controller prints the requested scaling decision. Use a single controller and persistent state file per cell. The Kubernetes actuator checks both the observed version and expected replica count. Give it access only to the named deployments and their scale subresources. Keep HPA or any other replica writer disabled for those same deployments. [Kubernetes scale preconditions](https://kubernetes.io/docs/reference/kubectl/generated/kubectl_scale/)

```sh
python3 deploy/autoscaling/collect.py --source source.json --output snapshot.json && \
python3 deploy/autoscaling/controller.py --config policy.json \
  --snapshot snapshot.json --state plan-state.json
```

Use plan mode during a pilot. When enabling changes, add `--apply` and choose a separate state file such as `live-state.json` so live scaling starts with a fresh observation window. Use the deployment context and namespace explicitly. An external process supervisor can schedule the commands, and a different orchestrator can consume the same decision through its guarded replica API.

## Enforce limits where identity and state are available

| Boundary | Policy |
| --- | --- |
| Trusted cell ingress | Optional per-proxy threshold of 1,000 requests per 10 seconds per verified client address. Enable `TOKENCRAFT_LB_ENABLE_IP_RATE_LIMIT=true` after measuring shared NAT and callback traffic. Returns `429 ingress_rate_limit`. |
| Durable submission | Each organization starts at **120 new submissions/minute** and a **20-submission burst** shared across all its projects and replicas. This covers native and model submissions, media, text, MCP, workflow draft tests and published runs, and their generation requests. |
| Active work | Existing organization, project, and provider concurrency caps still apply. Queued, running, and unconfirmed requests occupy slots. |
| Streams and reads | Existing per-replica 32-reader SSE limit still applies. Existing receipt replay, results, status, and quote operations consume no new-work credits. |
| Worker and inference provider | Workers honor the existing submit, cooldown, and retry contracts. Provider and model headroom gate autoscaling. Organizations' submission limits and supplier quotas have independent ownership. |

A new admission checks idempotency, routing, active-work limits, and spend policy before debiting the token bucket. PostgreSQL serializes the bucket row per organization, uses its own clock, and commits the debit together with the request or workflow run. A failed admission rolls back the debit. A matching accepted replay returns the original receipt. Exhaustion returns `429 submission_rate_limit` and a numeric `Retry-After`. Retry the same intent and idempotency key. An SSE replay still needs an available reader slot.

When enabling workflow submission metering on an existing cell, move workflow submissions onto the updated API fleet together. Old API replicas can still admit unmetered workflow runs. Workers can roll independently since generation requests already share the quota.

The per-organization row provides exact limits across API replicas and survives restart and database failover. Add cells to distribute organizations and their rows. If an organization moves, carry the quota and audit state with its requests and fence the old cell before admitting on the new writer. A hot organization can still contend on its row and surrounding admission work. Measure lock waits and rejected attempts before considering reserved quota leases or another shared limiter. Model-token quotas need a token reservation and reconciliation contract. Cross-cell supplier quotas need a separate pre-dispatch authority keyed to the supplier credential and workload.

### Request a higher limit

Organization owners and admins can read `GET /v1/rate-limits?account_id=...` and request the next tier using `POST /v1/rate-limits/increase?account_id=...` with an authenticated console session. Project bearer keys cannot grant increases. Use the currently advertised tier as the precondition.

```json
{
  "expected_requests_per_minute": 120,
  "requested_requests_per_minute": 240
}
```

The first eligible step can be approved immediately. Further steps are available every 24 hours. The ladder is **120 → 240 → 480 → 600 requests/minute**, corresponding to burst caps of **20 → 40 → 80 → 100**. The increase records the organization, requesting admin, prior tier, and time. Concurrent repeats of the current successful step return the approved policy without duplicating the audit event. A stale request or an early increase returns `409`. Review larger exceptions through the operator-managed path. Operators can configure a different RPM in `gateway_submission_limits` and set `operator_locked`. Preserve existing credits when adjusting a tier and clamp them to the new burst. Raising the RPM leaves spend, active work, provider limits, and infrastructure budgets in force.

### Verify before rollout

Apply migration `0070` on a disposable database before replacing the gateway. Drain the old API replicas before changing the schema. Bring the new gateways up and verify readiness before reopening admissions so every API applies the same organization rate limit. Test a two-replica burst and accepted replay on the HTTP interface, SSE exhaustion, and org admin role checks. Run the bundled policy simulation and PostgreSQL verification. Use a staging cell to verify the metrics adapter, DNS discovery of a new API replica, database guards, provider throttling, actuation, scale-in drain, and zone loss at load.

```sh
python3 -B -m unittest discover -s deploy/autoscaling -p test_policy.py
python3 -B deploy/autoscaling/verify_postgres.py --pg-bin-dir /path/to/postgresql/bin
```

The proxy threshold, request bucket, active caps, SSE limits, and downstream throttles have distinct reasons. Track them separately from eligible queue age, PostgreSQL waits, and the controller's scale and hold decisions. [HAProxy rate tracking](https://docs.haproxy.org/3.2/configuration.html)

Local verification used PostgreSQL 17.11 and HAProxy 3.4.8. The migration preserved the default for an existing organization. Concurrent SQL attempts shared 20 burst credits, rejected and rolled-back admissions preserved the bucket, and simultaneous tier retries produced one audit entry. The sampler excluded delayed, paused, and cooling-down work. Six controller simulation tests and the HAProxy fixture smoke passed.

The gateway and migration CLI also compiled offline and ran against freshly migrated PostgreSQL. Two API processes behind the cell proxy shared the organization burst across projects. With test-controlled refill, 20 of 35 fresh HTTP submissions were admitted and 15 returned `429 submission_rate_limit` with `Retry-After: 1`. Replica B replayed a receipt from A without a second job. A different organization retained capacity, and a separate refill check admitted new work. A request for an increase using a project key returned `401`. Concurrent owner retries advanced one tier and wrote one audit row.

A disposable K3s `v1.37.2+k3s1` cluster tested the real Kubernetes actuator with synthetic pressure signals and Alpine fixture workloads. The API and worker deployments each scaled from two to three running, Ready pods. Kubernetes rejected a stale deployment version and kept the replica count unchanged. Staging still needs to exercise the full path from deployment monitoring to actual TokenCraft pods, including drain, zone loss, and load.
