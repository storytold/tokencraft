ALTER TABLE workflow_runs
    ADD COLUMN budget_month_start timestamptz,
    ADD COLUMN budget_reserved_usd_nanos bigint NOT NULL DEFAULT 0,
    ADD COLUMN budget_remaining_usd_nanos bigint NOT NULL DEFAULT 0,
    ADD CHECK (budget_remaining_usd_nanos BETWEEN 0 AND budget_reserved_usd_nanos),
    ADD CHECK (budget_month_start IS NOT NULL OR budget_reserved_usd_nanos = 0);

CREATE INDEX workflow_runs_budget_month_idx
    ON workflow_runs (account_id, budget_month_start, project_id)
    WHERE budget_remaining_usd_nanos > 0 AND status IN ('queued', 'running', 'reconciling');

ALTER TABLE gateway_requests ADD COLUMN workflow_budget_month_start timestamptz;

CREATE INDEX gateway_requests_workflow_budget_month_idx
    ON gateway_requests (account_id, workflow_budget_month_start, project_id)
    WHERE workflow_budget_month_start IS NOT NULL;
