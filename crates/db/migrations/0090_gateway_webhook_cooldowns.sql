CREATE TABLE gateway_webhook_cooldowns (
    receiver_digest bytea PRIMARY KEY CHECK (octet_length(receiver_digest) = 32),
    available_after timestamptz NOT NULL
);

CREATE INDEX gateway_webhook_cooldowns_expiry ON gateway_webhook_cooldowns (available_after);

INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (90, 89);
