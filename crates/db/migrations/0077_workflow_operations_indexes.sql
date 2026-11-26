CREATE INDEX workflow_runs_operations_active_idx
    ON workflow_runs (account_id, project_id)
    WHERE status IN ('queued', 'running', 'reconciling');

CREATE INDEX workflow_runs_operations_terminal_idx
    ON workflow_runs (account_id, updated_at DESC, id DESC)
    INCLUDE (status, created_at)
    WHERE status IN ('succeeded', 'failed', 'canceled', 'expired');

CREATE INDEX workflow_runs_operations_project_terminal_idx
    ON workflow_runs (account_id, project_id, updated_at DESC, id DESC)
    INCLUDE (status, created_at)
    WHERE status IN ('succeeded', 'failed', 'canceled', 'expired');

CREATE TABLE public.gateway_schema_compatibility (
    migration_version bigint PRIMARY KEY,
    oldest_gateway_schema bigint NOT NULL
        CHECK (oldest_gateway_schema BETWEEN 1 AND migration_version)
);

-- This metadata is trusted by gateways during startup. Discard default grants before copying readers.
REVOKE ALL ON public.gateway_schema_compatibility FROM PUBLIC;

DO $$
DECLARE
    role_name text;
BEGIN
    FOR role_name IN
        SELECT DISTINCT r.rolname
        FROM pg_catalog.pg_class c
        CROSS JOIN LATERAL pg_catalog.aclexplode(c.relacl) acl
        JOIN pg_catalog.pg_roles r ON r.oid = acl.grantee
        WHERE c.oid = 'public.gateway_schema_compatibility'::regclass AND r.oid <> c.relowner
    LOOP
        EXECUTE format('REVOKE ALL ON public.gateway_schema_compatibility FROM %I', role_name);
    END LOOP;

    FOR role_name IN
        SELECT DISTINCT r.rolname
        FROM pg_catalog.pg_class c
        CROSS JOIN LATERAL pg_catalog.aclexplode(c.relacl) acl
        JOIN pg_catalog.pg_roles r ON r.oid = acl.grantee
        WHERE c.oid = 'public._sqlx_migrations'::regclass
          AND acl.privilege_type = 'SELECT' AND r.oid <> c.relowner
    LOOP
        EXECUTE format('GRANT SELECT ON public.gateway_schema_compatibility TO %I', role_name);
    END LOOP;
END;
$$;

-- Schema-76 binaries can continue using the existing workflow and delivery tables.
INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (77, 76);
