CREATE TABLE gateway_provider_workflow_frames (
    request_id text PRIMARY KEY REFERENCES gateway_provider_runs (request_id) ON DELETE CASCADE,
    -- Up to 2 MiB of PNG bytes plus the encryption nonce and tag.
    encrypted_png bytea NOT NULL CHECK (octet_length(encrypted_png) BETWEEN 29 AND 2097180)
);

CREATE FUNCTION forget_provider_workflow_frame() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM gateway_provider_workflow_frames WHERE request_id = NEW.request_id;
    RETURN NULL;
END;
$$;

CREATE TRIGGER provider_workflow_frame_cleanup
    AFTER UPDATE OF request_payload ON gateway_provider_runs
    FOR EACH ROW WHEN (OLD.request_payload IS NOT NULL AND NEW.request_payload IS NULL)
    EXECUTE FUNCTION forget_provider_workflow_frame();
