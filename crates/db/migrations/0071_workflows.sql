CREATE TABLE workflows (
    id text PRIMARY KEY,
    account_id text NOT NULL,
    project_id text NOT NULL,
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 100),
    draft jsonb NOT NULL,
    revision bigint NOT NULL DEFAULT 1 CHECK (revision > 0),
    active_version integer CHECK (active_version > 0),
    paused boolean NOT NULL DEFAULT false,
    created_by text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_id, project_id, id),
    FOREIGN KEY (account_id, project_id) REFERENCES gateway_projects(account_id, id)
);
CREATE INDEX workflows_project_idx ON workflows (account_id, project_id, updated_at DESC, id);

CREATE TABLE workflow_versions (
    account_id text NOT NULL,
    project_id text NOT NULL,
    workflow_id text NOT NULL,
    version integer NOT NULL CHECK (version > 0),
    draft_revision bigint NOT NULL CHECK (draft_revision > 0),
    graph jsonb NOT NULL,
    published_by text NOT NULL,
    published_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, project_id, workflow_id, version),
    FOREIGN KEY (account_id, project_id, workflow_id) REFERENCES workflows(account_id, project_id, id)
);
ALTER TABLE workflows ADD FOREIGN KEY (account_id, project_id, id, active_version)
    REFERENCES workflow_versions(account_id, project_id, workflow_id, version);

CREATE TABLE workflow_apps (
    id text PRIMARY KEY,
    account_id text NOT NULL,
    project_id text NOT NULL,
    workflow_id text NOT NULL,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    scopes text[] NOT NULL CHECK (cardinality(scopes) BETWEEN 1 AND 4 AND scopes <@
        ARRAY['runs:submit', 'runs:read', 'runs:cancel', 'outputs:read']::text[]),
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_id, project_id, workflow_id, id),
    FOREIGN KEY (account_id, project_id, workflow_id) REFERENCES workflows(account_id, project_id, id)
);

CREATE TABLE workflow_keys (
    id text PRIMARY KEY,
    account_id text NOT NULL,
    project_id text NOT NULL,
    workflow_id text NOT NULL,
    app_id text NOT NULL,
    key_hash text NOT NULL UNIQUE CHECK (length(key_hash) = 64),
    prefix text NOT NULL,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (account_id, project_id, workflow_id, app_id)
        REFERENCES workflow_apps(account_id, project_id, workflow_id, id)
);
CREATE INDEX workflow_keys_app_idx ON workflow_keys (account_id, project_id, workflow_id, app_id);

CREATE TABLE workflow_runs (
    id text PRIMARY KEY,
    account_id text NOT NULL,
    project_id text NOT NULL,
    workflow_id text NOT NULL,
    actor_id text NOT NULL,
    version integer CHECK (version > 0),
    draft_revision bigint NOT NULL CHECK (draft_revision > 0),
    idempotency_digest text NOT NULL CHECK (length(idempotency_digest) = 64),
    intent_digest text NOT NULL CHECK (length(intent_digest) = 64),
    status text NOT NULL DEFAULT 'queued' CHECK (status IN (
        'queued', 'running', 'reconciling', 'succeeded', 'failed', 'canceled', 'expired'
    )),
    revision bigint NOT NULL DEFAULT 1,
    lease bigint NOT NULL DEFAULT 0,
    graph jsonb NOT NULL,
    content bytea,
    next_at timestamptz NOT NULL DEFAULT now(),
    deadline timestamptz NOT NULL DEFAULT now() + interval '30 minutes',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_id, project_id, workflow_id, actor_id, idempotency_digest),
    FOREIGN KEY (account_id, project_id, workflow_id) REFERENCES workflows(account_id, project_id, id),
    FOREIGN KEY (account_id, project_id, workflow_id, version)
        REFERENCES workflow_versions(account_id, project_id, workflow_id, version)
);
CREATE INDEX workflow_runs_queue_idx ON workflow_runs (next_at, id)
    WHERE status IN ('queued', 'running', 'reconciling');
CREATE INDEX workflow_runs_project_idx ON workflow_runs (account_id, project_id, workflow_id, created_at DESC, id);
CREATE INDEX workflow_runs_release_idx ON workflow_runs (account_id, project_id, workflow_id, draft_revision)
    WHERE status = 'succeeded' AND version IS NULL;
