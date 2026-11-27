-- Use the admitted request's client. Existing deliveries retain their original signed bytes.
CREATE OR REPLACE FUNCTION public.enqueue_gateway_webhook() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    event_id text := 'whd_' || substr(NEW.id, 5);
    terminal_time timestamptz := coalesce(NEW.succeeded_at, NEW.canceled_at, NEW.failed_at);
    terminal_status text := CASE
        WHEN NEW.succeeded_at IS NOT NULL THEN 'succeeded'
        WHEN NEW.canceled_at IS NOT NULL THEN 'canceled'
        ELSE 'failed'
    END;
    event_type text := 'request.' || terminal_status;
    terminal_sequence integer := CASE
        WHEN NEW.dispatched_at IS NULL THEN 2
        WHEN EXISTS (
            SELECT 1 FROM public.gateway_provider_callback_receipts c
            WHERE c.request_id = NEW.id AND c.recovered_from IS NOT NULL
        ) THEN 5
        ELSE 3
    END;
BEGIN
    INSERT INTO public.gateway_webhook_deliveries (id, request_id, event_type, occurred_at, payload)
    VALUES (event_id, NEW.id, event_type, terminal_time, jsonb_build_object(
        'id', event_id,
        'type', event_type,
        'schema_version', 1,
        'occurred_at', to_char(terminal_time AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
        'sequence', terminal_sequence,
        'data', jsonb_build_object(
            'request_id', NEW.id,
            'project_id', NEW.project_id,
            'status', terminal_status,
            'fetch_url', '/v1/requests/' || NEW.id
        ) || CASE WHEN NEW.client_ref IS NULL THEN '{}'::jsonb
                  ELSE jsonb_build_object('client_ref', NEW.client_ref) END
    )::text);
    RETURN NULL;
END;
$$;

-- Schema-84 gateway and worker processes deliver the stored payload verbatim.
INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (85, 84);
