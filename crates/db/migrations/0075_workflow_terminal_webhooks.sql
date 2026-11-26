ALTER TABLE workflow_apps ADD UNIQUE (account_id, project_id, id);

ALTER TABLE gateway_webhook_subscriptions
    ADD COLUMN workflow_app_id text,
    ADD FOREIGN KEY (account_id, project_id, workflow_app_id)
        REFERENCES workflow_apps (account_id, project_id, id),
    ADD UNIQUE (account_id, project_id, id, workflow_app_id),
    ADD CHECK (workflow_app_id IS NULL OR client_ref IS NULL);

CREATE UNIQUE INDEX gateway_webhook_active_workflow_app
    ON gateway_webhook_subscriptions (workflow_app_id)
    WHERE workflow_app_id IS NOT NULL AND revoked_at IS NULL;

ALTER TABLE workflow_runs
    ADD COLUMN webhook_subscription_id text,
    ADD FOREIGN KEY (account_id, project_id, webhook_subscription_id, actor_id)
        REFERENCES gateway_webhook_subscriptions (account_id, project_id, id, workflow_app_id),
    ADD CHECK (webhook_subscription_id IS NULL OR version IS NOT NULL);

ALTER TABLE gateway_webhook_deliveries
    ALTER COLUMN request_id DROP NOT NULL,
    ADD COLUMN workflow_run_id text UNIQUE REFERENCES workflow_runs (id),
    DROP CONSTRAINT gateway_webhook_deliveries_event_type_check,
    ADD CHECK (num_nonnulls(request_id, workflow_run_id) = 1),
    ADD CHECK (
        (request_id IS NOT NULL AND event_type IN ('request.succeeded', 'request.canceled', 'request.failed'))
        OR (workflow_run_id IS NOT NULL AND event_type IN (
            'workflow.run.succeeded', 'workflow.run.failed', 'workflow.run.canceled', 'workflow.run.expired'
        ))
    );

CREATE FUNCTION enqueue_workflow_webhook() RETURNS trigger LANGUAGE plpgsql AS $$
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
        )
    )::text);
    RETURN NULL;
END;
$$;

CREATE TRIGGER workflow_run_terminal_webhook
    AFTER UPDATE OF status ON workflow_runs
    FOR EACH ROW WHEN (
        NEW.webhook_subscription_id IS NOT NULL
        AND OLD.status IN ('queued', 'running', 'reconciling')
        AND NEW.status IN ('succeeded', 'failed', 'canceled', 'expired')
    ) EXECUTE FUNCTION enqueue_workflow_webhook();
