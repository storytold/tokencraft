CREATE TABLE public.gateway_admission_control (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    paused boolean NOT NULL DEFAULT false
);

INSERT INTO public.gateway_admission_control DEFAULT VALUES;

-- Earlier gateways do not participate in the admission fence.
INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (88, 88);
