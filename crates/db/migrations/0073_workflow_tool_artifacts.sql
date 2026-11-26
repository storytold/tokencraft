CREATE TABLE workflow_tool_artifacts (
    run_id text NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    node text NOT NULL CHECK (length(node) BETWEEN 1 AND 48),
    content_type text NOT NULL CHECK (length(content_type) BETWEEN 1 AND 128),
    sha256 text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 29 AND 33554460),
    PRIMARY KEY (run_id, node)
);
