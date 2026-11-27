ALTER TABLE public.gateway_webhook_subscriptions
    ADD COLUMN rotation_receipt_digest text
        CHECK (rotation_receipt_digest ~ '^[a-f0-9]{64}$');

INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (87, 86);
