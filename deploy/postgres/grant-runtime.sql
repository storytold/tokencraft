-- Apply as the migration owner in the dedicated TokenCraft database.
-- psql -X -v ON_ERROR_STOP=1 -v runtime_role=... -f grant-runtime.sql
\set ON_ERROR_STOP on
BEGIN;
SET LOCAL search_path = pg_catalog;
SELECT set_config('tokencraft.runtime_role', :'runtime_role', true) AS runtime_role_name \gset

DO $grants$
DECLARE
    runtime_name text := current_setting('tokencraft.runtime_role');
    runtime_oid oid;
    migration_owner oid;
    metadata_name text;
    metadata oid;
    relation record;
    routine record;
BEGIN
    IF current_setting('server_version_num')::integer < 170000 THEN
        RAISE EXCEPTION 'TokenCraft runtime grants require PostgreSQL 17 or newer';
    END IF;

    SELECT oid INTO migration_owner FROM pg_roles WHERE rolname = current_user;
    SELECT oid INTO runtime_oid FROM pg_roles WHERE rolname = runtime_name;
    IF runtime_oid IS NULL THEN
        RAISE EXCEPTION 'Create the runtime role before applying these grants';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_roles
        WHERE pg_has_role(runtime_oid, oid, 'SET')
          AND (oid = migration_owner OR rolsuper OR rolcreaterole OR rolcreatedb OR rolreplication
               OR rolbypassrls OR has_schema_privilege(oid, 'public', 'CREATE'))
    ) THEN
        RAISE EXCEPTION 'Use a separate runtime role without elevated privileges or public schema CREATE';
    END IF;
    IF to_regclass('public._sqlx_migrations') IS NULL THEN
        RAISE EXCEPTION 'Apply TokenCraft migrations before granting runtime access';
    END IF;

    EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I', current_database(), runtime_name);
    EXECUTE format('GRANT USAGE ON SCHEMA public TO %I', runtime_name);
    FOR relation IN
        SELECT c.relname, c.relkind
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relowner = migration_owner
          AND c.relkind IN ('r', 'p', 'v', 'm', 'S')
          AND c.relname NOT IN ('_sqlx_migrations', 'gateway_schema_compatibility', 'gateway_admission_control')
    LOOP
        CASE relation.relkind
            WHEN 'S' THEN
                EXECUTE format('GRANT USAGE ON SEQUENCE public.%I TO %I', relation.relname, runtime_name);
            WHEN 'v', 'm' THEN
                EXECUTE format('GRANT SELECT ON TABLE public.%I TO %I', relation.relname, runtime_name);
            ELSE
                EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE public.%I TO %I',
                               relation.relname, runtime_name);
        END CASE;
    END LOOP;

    FOR routine IN
        SELECT p.oid::regprocedure AS signature
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'public' AND p.proowner = migration_owner
          AND p.prokind = 'f' AND NOT p.prosecdef
    LOOP
        EXECUTE format('GRANT EXECUTE ON FUNCTION %s TO %I', routine.signature, runtime_name);
    END LOOP;

    FOREACH metadata_name IN ARRAY ARRAY['_sqlx_migrations', 'gateway_schema_compatibility', 'gateway_admission_control']
    LOOP
        metadata := to_regclass(format('public.%I', metadata_name));
        CONTINUE WHEN metadata IS NULL;
        IF NOT EXISTS (SELECT 1 FROM pg_class WHERE oid = metadata AND relowner = migration_owner) THEN
            RAISE EXCEPTION 'The migration role must own public.%', metadata_name;
        END IF;
        EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC, %I', metadata_name, runtime_name);
        EXECUTE format('GRANT SELECT ON TABLE public.%I TO %I', metadata_name, runtime_name);
        IF EXISTS (
            SELECT 1 FROM pg_roles
            WHERE pg_has_role(runtime_oid, oid, 'SET')
              AND (has_table_privilege(oid, metadata, 'INSERT, UPDATE, DELETE, TRUNCATE, TRIGGER, MAINTAIN')
                   OR has_any_column_privilege(oid, metadata, 'INSERT, UPDATE'))
        ) THEN
            RAISE EXCEPTION 'Runtime role has additional write privileges on public.%', metadata_name;
        END IF;
    END LOOP;
END
$grants$;

COMMIT;
