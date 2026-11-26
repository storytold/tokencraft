-- Apply attribution when the run becomes terminal. Queued deliveries retain their original signed bytes.
CREATE OR REPLACE FUNCTION public.enqueue_workflow_webhook() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    event_id text := 'whd_' || substr(NEW.id, 5);
    event_type text := 'workflow.run.' || NEW.status;
BEGIN
    INSERT INTO gateway_webhook_deliveries (id, workflow_run_id, event_type, occurred_at, payload)
    VALUES (event_id, NEW.id, event_type, NEW.updated_at, jsonb_build_object(
        'id', event_id,
        'type', event_type,
        'schema_version', 1,
        'occurred_at', to_char(NEW.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        'sequence', NEW.revision,
        'data', jsonb_build_object(
            'run_id', NEW.id,
            'workflow_id', NEW.workflow_id,
            'project_id', NEW.project_id,
            'workflow_version', NEW.version,
            'status', NEW.status,
            'fetch_url', format('/v1/projects/%s/workflow-endpoints/%s/runs/%s',
                NEW.project_id, NEW.workflow_id, NEW.id)
        ) || CASE WHEN NEW.client_ref IS NULL THEN '{}'::jsonb
                  ELSE jsonb_build_object('client_ref', NEW.client_ref) END
    )::text);
    RETURN NULL;
END;
$$;

-- Schema-80 workers deliver the stored payload verbatim.
INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (81, 80);
