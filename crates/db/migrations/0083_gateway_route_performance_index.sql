-- Index a fixed-width workload key. The query checks JSONB equality after lookup.
CREATE INDEX gateway_requests_route_performance_idx
    ON public.gateway_requests (
        account_id, project_id, (coalesce(client_ref, '')),
        (jsonb_hash_extended(quote #> '{input,workload}', 0)),
        created_at DESC, sid DESC
    )
    WHERE adapter IN ('fal_queue', 'kinovi_queue', 'connector_queue')
      AND (succeeded_at IS NOT NULL OR uncertain_at IS NOT NULL OR failed_at IS NOT NULL);

-- The index changes neither stored requests nor the schema-82 gateway query.
INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (83, 82);
