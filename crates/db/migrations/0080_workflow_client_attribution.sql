ALTER TABLE workflow_runs ADD COLUMN client_ref text
    CHECK (client_ref IS NULL OR (version IS NOT NULL AND client_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$'));

-- Published workflow children carry their verified application separately from customer attribution.
ALTER TABLE gateway_requests ADD COLUMN workflow_app_id text;
CREATE INDEX gateway_requests_workflow_app_budget_idx
    ON gateway_requests (account_id, project_id, workflow_app_id, workflow_budget_month_start)
    WHERE workflow_app_id IS NOT NULL;
