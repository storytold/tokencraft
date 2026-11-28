ALTER TABLE public.gateway_webhook_subscriptions
    ADD COLUMN creation_receipt_digest bytea
        CHECK (octet_length(creation_receipt_digest) = 32);

CREATE UNIQUE INDEX gateway_webhook_creation_receipt
    ON public.gateway_webhook_subscriptions (account_id, project_id, creation_receipt_digest)
    WHERE creation_receipt_digest IS NOT NULL;

INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (91, 90);
