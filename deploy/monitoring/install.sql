-- Run as the migration owner against a dedicated TokenCraft database.
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL search_path = pg_catalog;
SELECT set_config('tokencraft.monitor_role', :'monitor_role', true) AS monitor_role \gset

CREATE SCHEMA IF NOT EXISTS tokencraft_monitor;
REVOKE ALL ON SCHEMA tokencraft_monitor FROM PUBLIC;

CREATE OR REPLACE FUNCTION tokencraft_monitor.maintenance() RETURNS jsonb
LANGUAGE plpgsql STABLE
SET search_path = pg_catalog, pg_temp
AS $maintenance$
DECLARE
    is_paused boolean;
BEGIN
    IF pg_is_in_recovery() OR to_regclass('public.gateway_admission_control') IS NULL THEN
        RETURN NULL;
    END IF;
    SELECT paused INTO is_paused FROM public.gateway_admission_control WHERE singleton;
    IF is_paused IS NULL THEN
        RETURN NULL;
    END IF;
    RETURN jsonb_build_object(
        'paused', is_paused,
        'drain', CASE WHEN is_paused THEN jsonb_build_object(
            'unresolved_requests', (SELECT count(*) FROM (
                SELECT 1 FROM public.gateway_requests
                WHERE num_nonnulls(succeeded_at, failed_at, canceled_at) = 0 LIMIT 5001
            ) AS requests),
            'active_workflows', (SELECT count(*) FROM (
                SELECT 1 FROM public.workflow_runs
                WHERE status IN ('queued', 'running', 'reconciling') LIMIT 5001
            ) AS workflows),
            'pending_webhooks', (SELECT count(*) FROM (
                SELECT 1 FROM public.gateway_webhook_deliveries
                WHERE state IN ('pending', 'sending') LIMIT 5001
            ) AS webhooks)
        ) END
    );
END;
$maintenance$;

REVOKE ALL ON FUNCTION tokencraft_monitor.maintenance() FROM PUBLIC;

CREATE OR REPLACE FUNCTION tokencraft_monitor.sample(
    providers jsonb, targets jsonb, capacity jsonb, reserve integer, scope_account text, scope_project text
) RETURNS jsonb
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $sample$
WITH parameters AS MATERIALIZED (
    SELECT now() AS sampled_at,
           NOT pg_is_in_recovery() AS database_primary,
           ARRAY(SELECT jsonb_array_elements_text(providers)) AS providers
    WHERE jsonb_array_length(providers) BETWEEN 1 AND 64
      AND jsonb_array_length(targets) <= 64
      AND jsonb_array_length(capacity->'local') <= 512
      AND jsonb_array_length(capacity->'connector') <= 512
      AND coalesce(jsonb_array_length(capacity->'poll'), 0) <= 512
      AND coalesce(jsonb_array_length(capacity->'lookup'), 0) <= 512
      AND reserve BETWEEN 0 AND 10000
      AND (scope_project IS NULL OR scope_account IS NOT NULL)
), newest AS MATERIALIZED (
    SELECT sid, id, provider, created_at, dispatched_at, succeeded_at, failed_at, uncertain_at, canceled_at,
           CASE WHEN quote#>>'{input,workload,kind}' IN ('text','image','video','audio','mesh')
                THEN quote#>>'{input,workload,kind}' ELSE 'other' END AS workload
    FROM public.gateway_requests
    WHERE (scope_account IS NULL OR account_id = scope_account)
      AND (scope_project IS NULL OR project_id = scope_project)
    ORDER BY sid DESC LIMIT 10001
), scoped AS MATERIALIZED (
    SELECT r.*, p.accepted_at, p.result_observed_at, p.compute_attempts
    FROM newest r CROSS JOIN parameters n
    LEFT JOIN public.gateway_provider_runs p ON p.request_id = r.id
    WHERE r.created_at >= n.sampled_at - interval '15 minutes'
      AND r.provider = ANY(n.providers)
), provider_summary AS (
    SELECT provider, jsonb_build_object(
        'admitted', count(*),
        'dispatched', count(dispatched_at),
        'succeeded', count(succeeded_at),
        'failed', count(failed_at),
        'uncertain', count(uncertain_at),
        'canceled', count(canceled_at),
        'p95_queue_seconds', percentile_cont(0.95) WITHIN GROUP
            (ORDER BY greatest(0, extract(epoch FROM dispatched_at - created_at)))
            FILTER (WHERE dispatched_at IS NOT NULL),
        'p95_total_seconds', percentile_cont(0.95) WITHIN GROUP
            (ORDER BY greatest(0, extract(epoch FROM succeeded_at - created_at)))
            FILTER (WHERE succeeded_at IS NOT NULL),
        'p95_provider_seconds', percentile_cont(0.95) WITHIN GROUP
            (ORDER BY greatest(0, extract(epoch FROM result_observed_at - accepted_at)))
            FILTER (WHERE accepted_at IS NOT NULL AND result_observed_at IS NOT NULL),
        'p95_publication_seconds', percentile_cont(0.95) WITHIN GROUP
            (ORDER BY greatest(0, extract(epoch FROM succeeded_at - result_observed_at)))
            FILTER (WHERE result_observed_at IS NOT NULL AND succeeded_at IS NOT NULL)
    ) AS metrics
    FROM scoped GROUP BY provider
), publication AS (
    SELECT provider, workload,
           count(*) AS observed,
           count(succeeded_at) AS published,
           count(uncertain_at) AS unconfirmed,
           count(*) FILTER (WHERE num_nonnulls(succeeded_at, failed_at, canceled_at, uncertain_at) = 0) AS pending,
           max(extract(epoch FROM now() - result_observed_at))
               FILTER (WHERE num_nonnulls(succeeded_at, failed_at, canceled_at, uncertain_at) = 0) AS oldest_pending_seconds,
           percentile_cont(0.95) WITHIN GROUP
               (ORDER BY greatest(0, extract(epoch FROM succeeded_at - result_observed_at)))
               FILTER (WHERE succeeded_at IS NOT NULL) AS p95_seconds
    FROM scoped WHERE result_observed_at IS NOT NULL
    GROUP BY provider, workload
), active_requests AS MATERIALIZED (
    SELECT r.provider,
           CASE WHEN quote#>>'{input,workload,kind}' IN ('text','image','video','audio','mesh')
                THEN quote#>>'{input,workload,kind}' ELSE 'other' END AS workload,
           CASE WHEN r.uncertain_at IS NOT NULL THEN 'reconciling'
                            WHEN r.dispatched_at IS NULL THEN 'queued' ELSE 'running' END AS state,
           coalesce(r.uncertain_at, r.dispatched_at, r.created_at) AS entered_at,
           (r.uncertain_at IS NULL AND r.dispatched_at IS NULL
                AND coalesce(d.pause_all OR r.provider = ANY(d.paused_providers), false)) AS paused
    FROM public.gateway_requests r
    CROSS JOIN parameters n
    LEFT JOIN public.gateway_dispatch_controls d ON d.account_id = r.account_id
    WHERE r.provider = ANY(n.providers)
      AND (scope_account IS NULL OR r.account_id = scope_account)
      AND (scope_project IS NULL OR r.project_id = scope_project)
      AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at) = 0
    ORDER BY r.sid DESC LIMIT 5001
), request_queues AS (
    SELECT provider, workload, state, count(*) AS count, count(*) FILTER (WHERE paused) AS paused,
           greatest(0, extract(epoch FROM now() - min(entered_at))) AS oldest_seconds
    FROM active_requests GROUP BY provider, workload, state
), active_workflows AS MATERIALIZED (
    SELECT status, created_at, next_at, deadline
    FROM public.workflow_runs
    WHERE status IN ('queued', 'running', 'reconciling')
      AND (scope_account IS NULL OR account_id = scope_account)
      AND (scope_project IS NULL OR project_id = scope_project)
    LIMIT 5001
), workflow_queues AS (
    SELECT status, count(*) AS count, greatest(0, extract(epoch FROM now() - min(created_at))) AS oldest_seconds,
           count(*) FILTER (WHERE status = 'queued' AND next_at <= now() AND deadline > now()) AS due,
           count(*) FILTER (WHERE deadline <= now()) AS overdue
    FROM active_workflows GROUP BY status
), ended_workflows AS MATERIALIZED (
    SELECT status, created_at, updated_at FROM public.workflow_runs
    WHERE status IN ('succeeded', 'failed', 'canceled', 'expired')
      AND updated_at BETWEEN now() - interval '24 hours' AND now()
      AND (scope_account IS NULL OR account_id = scope_account)
      AND (scope_project IS NULL OR project_id = scope_project)
    ORDER BY updated_at DESC, id DESC LIMIT 5001
), workflow_outcomes AS (
    SELECT jsonb_build_object(
        'sampled', count(*),
        'succeeded', count(*) FILTER (WHERE status = 'succeeded'),
        'failed', count(*) FILTER (WHERE status = 'failed'),
        'canceled', count(*) FILTER (WHERE status = 'canceled'),
        'expired', count(*) FILTER (WHERE status = 'expired'),
        'p95_succeeded_seconds', percentile_cont(0.95) WITHIN GROUP
            (ORDER BY extract(epoch FROM updated_at - created_at))
            FILTER (WHERE status = 'succeeded' AND updated_at >= created_at)
    ) AS metrics FROM ended_workflows
), scoped_deliveries AS NOT MATERIALIZED (
    SELECT state, occurred_at, finished_at FROM public.gateway_webhook_deliveries w
    WHERE scope_account IS NULL
       OR EXISTS (SELECT 1 FROM public.gateway_requests r WHERE r.id = w.request_id AND r.account_id = scope_account
                    AND (scope_project IS NULL OR r.project_id = scope_project))
       OR EXISTS (SELECT 1 FROM public.workflow_runs r WHERE r.id = w.workflow_run_id AND r.account_id = scope_account
                    AND (scope_project IS NULL OR r.project_id = scope_project))
), delivery_queues AS (
    SELECT 'webhook_pending' AS kind, count(*) AS count,
           greatest(0, extract(epoch FROM now() - min(occurred_at))) AS oldest_seconds
    FROM (SELECT occurred_at FROM scoped_deliveries
          WHERE state IN ('pending', 'sending') LIMIT 5001) pending
    UNION ALL
    SELECT 'webhook_dead', count(*), greatest(0, extract(epoch FROM now() - min(finished_at)))
    FROM (SELECT finished_at FROM scoped_deliveries
          WHERE state = 'dead' AND finished_at >= now() - interval '24 hours' LIMIT 5001) dead
    UNION ALL
    SELECT 'export_pending', count(*), greatest(0, extract(epoch FROM now() - min(occurred_at)))
    FROM (SELECT occurred_at FROM public.gateway_export_events WHERE published_seq IS NULL
            AND (scope_account IS NULL OR account_id = scope_account)
            AND (scope_project IS NULL OR project_id = scope_project)
          LIMIT 5001) exports
), target_summary AS (
    SELECT t.id, jsonb_build_object(
        'attempted', count(a.attempt),
        'response_started', count(*) FILTER (WHERE a.attempt#>>'{result,status}' = 'response_started'),
        'rate_limited', count(*) FILTER (WHERE a.attempt#>>'{result,http_status}' = '429'),
        'connection_failed', count(*) FILTER (WHERE a.attempt#>>'{result,status}' = 'connection_failed'),
        'upstream_error', count(*) FILTER (WHERE a.attempt#>>'{result,status}' = 'upstream_error'),
        'transport_unconfirmed', count(*) FILTER (WHERE a.attempt#>>'{result,status}' = 'transport_unconfirmed'),
        'deadline_before_send', count(*) FILTER (WHERE a.attempt#>>'{result,status}' = 'deadline_before_send'),
        'pending_observation', count(*) FILTER (WHERE a.attempt IS NOT NULL AND a.attempt->'result' IS NULL),
        'p95_response_headers_seconds', percentile_cont(0.95) WITHIN GROUP
            (ORDER BY greatest(0, extract(epoch FROM
                (a.attempt#>>'{result,observed_at}')::timestamptz - (a.attempt->>'started_at')::timestamptz)))
            FILTER (WHERE a.attempt#>>'{result,status}' = 'response_started')
    ) AS metrics
    FROM parameters
    CROSS JOIN LATERAL jsonb_to_recordset(targets) AS t(id text, cell text, provider text, stack text, target text)
    LEFT JOIN (
        SELECT r.provider, a.attempt FROM scoped r
        CROSS JOIN LATERAL jsonb_array_elements(r.compute_attempts) AS a(attempt)
    ) a ON a.provider = t.provider AND a.attempt#>>'{selection,home_cell}' = t.cell
       AND a.attempt#>>'{selection,stack}' = t.stack AND a.attempt#>>'{selection,target}' = t.target
    GROUP BY t.id
),
provider_work AS MATERIALIZED (
    SELECT lanes.* FROM parameters n
    CROSS JOIN LATERAL public.gateway_provider_work_sample(capacity, n.providers, scope_account, scope_project) lanes
), connections AS (
    SELECT greatest(0, current_setting('max_connections')::integer
        - current_setting('superuser_reserved_connections')::integer
        - coalesce(current_setting('reserved_connections', true)::integer, 0)
        - reserve - count(*)) AS free
    FROM pg_stat_activity WHERE backend_type = 'client backend'
)
SELECT jsonb_build_object(
    'window_seconds', 900,
    'database_primary', n.database_primary,
    'maintenance', CASE WHEN scope_account IS NULL THEN tokencraft_monitor.maintenance() END,
    'truncated', (SELECT count(*) = 10001 AND min(created_at) >= n.sampled_at - interval '15 minutes' FROM newest),
    'provider', coalesce((SELECT jsonb_object_agg(provider, metrics) FROM provider_summary), '{}'::jsonb),
    'compute', coalesce((SELECT jsonb_object_agg(id, metrics) FROM target_summary), '{}'::jsonb),
    'eligible', (SELECT eligible FROM provider_work WHERE lane = 'submit'),
    'oldest_eligible_seconds', (SELECT oldest_seconds FROM provider_work WHERE lane = 'submit'),
    'provider_work', (SELECT jsonb_object_agg(lane, jsonb_build_object(
        'eligible', eligible, 'oldest_seconds', oldest_seconds)) FROM provider_work),
    'database_connections_available', (SELECT free FROM connections),
    'publication', coalesce((SELECT jsonb_agg(to_jsonb(p) ORDER BY provider, workload) FROM publication p), '[]'::jsonb),
    'request_queues', coalesce((SELECT jsonb_agg(to_jsonb(q) ORDER BY provider, workload, state) FROM request_queues q), '[]'::jsonb),
    'request_queues_capped', (SELECT count(*) = 5001 FROM active_requests),
    'workflow_queues', coalesce((SELECT jsonb_agg(to_jsonb(q) ORDER BY status) FROM workflow_queues q), '[]'::jsonb),
    'workflow_queues_capped', (SELECT count(*) = 5001 FROM active_workflows),
    'workflow_outcomes', (SELECT metrics FROM workflow_outcomes),
    'delivery_queues', (SELECT jsonb_agg(to_jsonb(q) ORDER BY kind) FROM delivery_queues q)
)
FROM parameters n;
$sample$;

REVOKE ALL ON FUNCTION tokencraft_monitor.sample(jsonb, jsonb, jsonb, integer, text, text) FROM PUBLIC;

DO $grant$
DECLARE
    monitor_name text := current_setting('tokencraft.monitor_role');
    monitor_oid oid;
BEGIN
    SELECT oid INTO monitor_oid FROM pg_roles WHERE rolname = monitor_name AND rolcanlogin;
    IF monitor_oid IS NULL OR EXISTS (
        SELECT 1 FROM pg_roles
        WHERE (oid = monitor_oid OR pg_has_role(monitor_oid, oid, 'MEMBER'))
          AND (oid <> monitor_oid OR rolsuper OR rolcreaterole OR rolcreatedb OR rolreplication
               OR rolbypassrls OR has_schema_privilege(oid, 'public', 'CREATE')
               OR EXISTS (
                   SELECT 1 FROM unnest(ARRAY[
                       'public.gateway_requests'::regclass, 'public.gateway_provider_runs'::regclass,
                       'public.gateway_dispatch_controls'::regclass, 'public.gateway_queue_cooldowns'::regclass,
                       'public.gateway_connector_completions'::regclass,
                       'public.workflow_runs'::regclass, 'public.gateway_webhook_deliveries'::regclass,
                       'public.gateway_export_events'::regclass, to_regclass('public.gateway_admission_control')
                   ]) AS relation(ref)
                   WHERE has_any_column_privilege(oid, relation.ref, 'SELECT,INSERT,UPDATE')
                      OR has_table_privilege(oid, relation.ref, 'DELETE,TRUNCATE,TRIGGER')
               ))
    ) THEN
        RAISE EXCEPTION 'Use a dedicated LOGIN role without inherited, elevated, or gateway data privileges';
    END IF;
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I', current_database(), monitor_name);
    EXECUTE format('GRANT USAGE ON SCHEMA tokencraft_monitor TO %I', monitor_name);
    EXECUTE format('GRANT EXECUTE ON FUNCTION tokencraft_monitor.sample(jsonb,jsonb,jsonb,integer,text,text) TO %I',
                   monitor_name);
END;
$grant$;
COMMIT;
