CREATE INDEX workflow_runs_activity_idx
    ON workflow_runs (account_id, project_id, created_at DESC, id DESC);
