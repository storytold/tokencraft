-- Match the lock order of request writers before extending the event contract.
LOCK TABLE gateway_requests IN SHARE ROW EXCLUSIVE MODE;

ALTER TABLE gateway_export_events
    DROP CONSTRAINT gateway_export_events_event_type_check,
    ADD CONSTRAINT gateway_export_events_event_type_check CHECK (event_type IN (
        'request.accepted', 'request.dispatched', 'request.succeeded', 'request.canceled',
        'request.failed', 'request.outcome_unconfirmed', 'request.content_removed',
        'request.recovered', 'usage.observed', 'usage.supplier_settled'
    )) NOT VALID;

CREATE OR REPLACE FUNCTION capture_gateway_request_export(request gateway_requests, kind text, happened_at timestamptz)
RETURNS void LANGUAGE sql AS $$
    INSERT INTO gateway_export_events
        (account_id, project_id, request_id, client_ref, event_type, observation_key, occurred_at, data)
    VALUES (
        request.account_id, request.project_id, request.id, request.client_ref, kind,
        -- Keep the original first-event key, including for archived history.
        CASE WHEN kind = 'request.outcome_unconfirmed' AND EXISTS (
            SELECT 1 FROM gateway_export_events first_event
            WHERE first_event.request_id = request.id AND first_event.event_type = kind
              AND first_event.observation_key = '' AND first_event.occurred_at <> happened_at
        ) THEN extract(epoch FROM happened_at)::text ELSE '' END,
        happened_at,
        jsonb_build_object(
            'model', request.model,
            'provider', request.provider,
            'offer_id', request.offer_id,
            'adapter', request.adapter,
            'estimated_usd_nanos', request.estimated_usd_nanos::text
        )
    )
    ON CONFLICT (request_id, event_type, observation_key) DO NOTHING;
$$;

CREATE OR REPLACE FUNCTION capture_gateway_request_export_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM capture_gateway_request_export(NEW, 'request.accepted', NEW.created_at);
        RETURN NULL;
    END IF;
    IF OLD.dispatched_at IS NULL AND NEW.dispatched_at IS NOT NULL THEN
        PERFORM capture_gateway_request_export(NEW, 'request.dispatched', NEW.dispatched_at);
    END IF;
    IF OLD.succeeded_at IS NULL AND NEW.succeeded_at IS NOT NULL THEN
        PERFORM capture_gateway_request_export(NEW, 'request.succeeded', NEW.succeeded_at);
    END IF;
    IF OLD.canceled_at IS NULL AND NEW.canceled_at IS NOT NULL THEN
        PERFORM capture_gateway_request_export(NEW, 'request.canceled', NEW.canceled_at);
    END IF;
    IF OLD.failed_at IS NULL AND NEW.failed_at IS NOT NULL THEN
        PERFORM capture_gateway_request_export(NEW, 'request.failed', NEW.failed_at);
    END IF;
    IF OLD.uncertain_at IS NULL AND NEW.uncertain_at IS NOT NULL THEN
        PERFORM capture_gateway_request_export(NEW, 'request.outcome_unconfirmed', NEW.uncertain_at);
    ELSIF OLD.uncertain_at IS NOT NULL AND NEW.uncertain_at IS NULL
        AND NEW.dispatched_at IS NOT NULL
        AND num_nonnulls(NEW.succeeded_at, NEW.failed_at, NEW.canceled_at, NEW.content_removed_at) = 0 THEN
        PERFORM capture_gateway_request_export(NEW, 'request.recovered', clock_timestamp());
    END IF;
    RETURN NULL;
END;
$$;

-- Repair the projection for active requests recovered before the migration.
DO $$
BEGIN
    PERFORM capture_gateway_request_export(r, transition.kind, transition.at)
    FROM gateway_connector_completions c
    JOIN gateway_requests r ON r.id = c.request_id
    CROSS JOIN LATERAL (VALUES
        ('request.recovered', c.reconciled_at),
        ('request.outcome_unconfirmed', CASE WHEN r.uncertain_at > c.reconciled_at THEN r.uncertain_at END)
    ) AS transition(kind, at)
    WHERE c.reconciled_at IS NOT NULL AND transition.at IS NOT NULL
      AND r.dispatched_at IS NOT NULL AND r.content_removed_at IS NULL
      AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at) = 0
    ORDER BY transition.at, r.sid;
END;
$$;

INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (89, 88);
