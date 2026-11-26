CREATE TABLE workflow_uploads (
    id text PRIMARY KEY,
    account_id text NOT NULL,
    project_id text NOT NULL,
    workflow_id text NOT NULL,
    actor_id text NOT NULL,
    content_type text NOT NULL CHECK (length(content_type) BETWEEN 1 AND 128),
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes integer NOT NULL CHECK (size_bytes BETWEEN 1 AND 33554432),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) = size_bytes + 28),
    expires_at timestamptz NOT NULL DEFAULT now() + interval '1 hour',
    FOREIGN KEY (account_id, project_id, workflow_id) REFERENCES workflows(account_id, project_id, id)
        ON DELETE CASCADE
);

CREATE INDEX workflow_uploads_scope_idx ON workflow_uploads (account_id, project_id, workflow_id);
CREATE INDEX workflow_uploads_expiry_idx ON workflow_uploads (expires_at, id);
