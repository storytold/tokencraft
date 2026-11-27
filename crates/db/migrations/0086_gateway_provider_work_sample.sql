-- Sample the provider worker's three lanes without taking leases or changing queue order.
CREATE FUNCTION public.gateway_provider_work_sample(
    capacity jsonb, providers text[] DEFAULT NULL, scope_account text DEFAULT NULL, scope_project text DEFAULT NULL
) RETURNS TABLE (lane text, eligible bigint, oldest_seconds numeric)
LANGUAGE sql STABLE SECURITY INVOKER
SET search_path = pg_catalog, pg_temp
AS $sample$
WITH configured AS MATERIALIZED (
    SELECT now() AS sampled_at,
           ARRAY(SELECT jsonb_array_elements_text(capacity->'local')) AS local_bindings,
           ARRAY(SELECT jsonb_array_elements_text(capacity->'connector')) AS submit_bindings,
           ARRAY(SELECT jsonb_array_elements_text(capacity->'poll')) AS poll_bindings,
           ARRAY(SELECT jsonb_array_elements_text(capacity->'lookup')) AS lookup_bindings
), submissions AS (
    SELECT r.dispatch_after AS eligible_at
    FROM public.gateway_requests r
    JOIN public.gateway_provider_runs p ON p.request_id = r.id
    CROSS JOIN configured c
    WHERE ((r.adapter = 'openai_compatible_chat' AND p.binding_digest = ANY(c.local_bindings))
        OR (r.adapter = 'connector_queue' AND p.binding_digest = ANY(c.submit_bindings)))
      AND (providers IS NULL OR r.provider = ANY(providers))
      AND (scope_account IS NULL OR r.account_id = scope_account)
      AND (scope_project IS NULL OR r.project_id = scope_project)
      AND r.dispatched_at IS NULL AND r.failed_at IS NULL AND r.canceled_at IS NULL
      AND r.uncertain_at IS NULL AND (p.lease_until IS NULL OR p.lease_until <= c.sampled_at)
      AND r.dispatch_after <= c.sampled_at AND r.dispatch_after > c.sampled_at - interval '15 minutes'
      AND r.created_at > c.sampled_at - interval '15 minutes'
      AND p.request_payload IS NOT NULL
      AND NOT EXISTS (
          SELECT 1 FROM public.gateway_dispatch_controls d
          WHERE d.account_id = r.account_id AND (d.pause_all OR r.provider = ANY(d.paused_providers))
      )
      AND (r.adapter <> 'connector_queue' OR NOT EXISTS (
          SELECT 1 FROM public.gateway_queue_cooldowns q
          WHERE q.account_id = r.account_id AND q.project_id = r.project_id
            AND q.binding_digest = p.binding_digest AND q.upstream_model = p.upstream_model
            AND q.available_after > c.sampled_at
      ))
    ORDER BY eligible_at, r.sid
    LIMIT 256
), polls AS (
    SELECT greatest(p.poll_after, p.lease_until) AS eligible_at
    FROM public.gateway_provider_runs p
    JOIN public.gateway_requests r ON r.id = p.request_id
    CROSS JOIN configured c
    WHERE r.adapter = 'connector_queue' AND p.binding_digest = ANY(c.poll_bindings)
      AND (providers IS NULL OR r.provider = ANY(providers))
      AND (scope_account IS NULL OR r.account_id = scope_account)
      AND (scope_project IS NULL OR r.project_id = scope_project)
      AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at) = 0
      AND (r.uncertain_at IS NULL OR p.recheck_requested_at IS NOT NULL)
      AND p.poll_after <= c.sampled_at AND p.poll_deadline > c.sampled_at
      AND (p.lease_until IS NULL OR p.lease_until <= c.sampled_at)
    ORDER BY eligible_at, r.sid
    LIMIT 256
), lookups AS (
    SELECT coalesce(h.next_lookup_at, r.uncertain_at) AS eligible_at
    FROM public.gateway_requests r
    JOIN public.gateway_provider_runs p ON p.request_id = r.id
    LEFT JOIN public.gateway_connector_completions h ON h.request_id = r.id
    CROSS JOIN configured c
    WHERE r.adapter = 'connector_queue' AND p.binding_digest = ANY(c.lookup_bindings)
      AND (providers IS NULL OR r.provider = ANY(providers))
      AND (scope_account IS NULL OR r.account_id = scope_account)
      AND (scope_project IS NULL OR r.project_id = scope_project)
      AND r.uncertain_at > c.sampled_at - interval '15 minutes'
      AND (h.next_lookup_at IS NULL OR h.next_lookup_at <= c.sampled_at)
      AND p.upstream_handle IS NULL
      AND p.error_code IN ('connector_submission_unconfirmed', 'submission_interrupted')
      AND r.dispatched_at > c.sampled_at - interval '15 minutes'
      AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at, r.content_removed_at) = 0
    ORDER BY eligible_at, r.id
    LIMIT 256
)
SELECT 'submit', count(*), extract(epoch FROM now() - min(eligible_at)) FROM submissions
UNION ALL
SELECT 'poll', count(*), extract(epoch FROM now() - min(eligible_at)) FROM polls
UNION ALL
SELECT 'lookup', count(*), extract(epoch FROM now() - min(eligible_at)) FROM lookups;
$sample$;

INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (86, 85);
