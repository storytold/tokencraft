BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL statement_timeout = '2000ms';

\set observed_admissions_paused null
SELECT coalesce(has_table_privilege(to_regclass('public.gateway_admission_control'), 'SELECT'), false)
    AS can_read_admission_pause \gset
\if :can_read_admission_pause
SELECT coalesce((SELECT paused::text FROM public.gateway_admission_control WHERE singleton), 'null')
    AS observed_admissions_paused \gset
\endif

WITH lanes AS (
    SELECT * FROM public.gateway_provider_work_sample(:'bindings'::jsonb)
), connections AS (
    SELECT greatest(0, current_setting('max_connections')::integer
        - current_setting('superuser_reserved_connections')::integer
        - coalesce(current_setting('reserved_connections', true)::integer, 0)
        - :'reserve_connections'::integer - count(*)) AS free
    FROM pg_stat_activity WHERE backend_type = 'client backend'
)
SELECT json_build_object(
    'oldest_ready_age_seconds', coalesce(floor(max(oldest_seconds))::integer, 0),
    'eligible_sample', sum(eligible),
    'additional_connections', (SELECT free FROM connections),
    'admissions_paused', :'observed_admissions_paused'::jsonb
) FROM lanes;

COMMIT;
