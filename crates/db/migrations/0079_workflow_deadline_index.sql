CREATE INDEX workflow_runs_deadline_idx
    ON workflow_runs (deadline, id)
    WHERE status IN ('queued', 'running', 'reconciling');

-- Schema-78 gateways can continue claiming or expiring runs during the worker rollout.
INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (79, 78);
