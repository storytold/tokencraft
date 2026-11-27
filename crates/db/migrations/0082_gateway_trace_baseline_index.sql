-- Empty references are excluded by the gateway_requests client and app checks.
CREATE INDEX gateway_requests_trace_baseline_idx
    ON public.gateway_requests (
        account_id, project_id, (coalesce(client_ref, '')), (coalesce(app_ref, '')),
        model, provider, adapter, created_at DESC, sid DESC
    )
    WHERE succeeded_at IS NOT NULL;

-- Schema-81 gateways can continue using the earlier baseline query.
INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (82, 81);
