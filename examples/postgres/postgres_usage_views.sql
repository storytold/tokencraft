-- Run in the customer database after the v1 export importer has initialized it.
BEGIN;

CREATE INDEX IF NOT EXISTS export_events_usage_scope_idx
    ON tokencraft_export.events (source_url, project_id, (payload->>'type'), (payload->>'request_id'))
    WHERE payload->>'type' IN ('request.accepted', 'usage.observed', 'usage.supplier_settled');

CREATE OR REPLACE VIEW tokencraft_export.supplier_settlement_postings AS
SELECT source_url,
       project_id,
       payload->>'request_id' AS request_id,
       event_id,
       payload->>'client_ref' AS client_ref,
       payload#>>'{data,app_ref}' AS app_ref,
       payload#>>'{data,model}' AS model,
       payload#>>'{data,provider}' AS provider,
       (payload->>'occurred_at')::timestamptz AS recorded_at,
       (payload#>>'{data,revision}')::integer AS revision,
       (payload#>>'{data,amount_usd_nanos}')::bigint AS amount_usd_nanos,
       (payload#>>'{data,delta_usd_nanos}')::bigint AS delta_usd_nanos
FROM tokencraft_export.events
WHERE payload->>'type' = 'usage.supplier_settled';

CREATE OR REPLACE VIEW tokencraft_export.provider_charge_observations AS
SELECT source_url, project_id, event_id, payload->>'request_id' AS request_id,
       payload->>'client_ref' AS client_ref, payload#>>'{data,app_ref}' AS app_ref,
       payload#>>'{data,provider}' AS provider, payload#>>'{data,provider_charge,unit}' AS unit,
       (payload#>>'{data,provider_charge,quantity}')::numeric AS quantity,
       (payload->>'occurred_at')::timestamptz AS observed_at
FROM tokencraft_export.events
WHERE payload->>'type' = 'usage.observed' AND payload#>>'{data,source}' = 'connector_charge';

CREATE OR REPLACE VIEW tokencraft_export.request_usage AS
WITH observations AS (
    SELECT source_url, project_id, payload->>'request_id' AS request_id,
           payload#>>'{data,source}' AS source,
           (payload#>>'{data,amount_usd_nanos}')::bigint AS amount_usd_nanos,
           (payload#>>'{data,output_count}')::bigint AS output_count,
           (payload#>>'{data,input_tokens}')::bigint AS input_tokens,
           (payload#>>'{data,output_tokens}')::bigint AS output_tokens
    FROM tokencraft_export.events
    WHERE payload->>'type' = 'usage.observed' AND payload#>>'{data,source}' <> 'connector_charge'
), usage AS (
    SELECT source_url, project_id, request_id,
           sum(amount_usd_nanos) FILTER (WHERE source = 'mock_provider') AS mock_reported_usd_nanos,
           sum(amount_usd_nanos) FILTER (WHERE source <> 'mock_provider') AS provider_reported_usd_nanos,
           count(*) FILTER (
               WHERE source <> 'mock_provider' AND amount_usd_nanos IS NULL
           ) AS provider_reports_missing_cost,
           sum(output_count) AS completed_outputs,
           sum(input_tokens) AS input_tokens,
           sum(output_tokens) AS output_tokens
    FROM observations
    GROUP BY source_url, project_id, request_id
), latest_settlement AS (
    SELECT DISTINCT ON (source_url, project_id, request_id)
           source_url, project_id, request_id, event_id, revision, amount_usd_nanos
    FROM tokencraft_export.supplier_settlement_postings
    ORDER BY source_url, project_id, request_id, revision DESC, event_id DESC
)
SELECT admission.source_url,
       admission.project_id,
       admission.payload->>'request_id' AS request_id,
       admission.event_id AS admission_event_id,
       admission.payload->>'client_ref' AS client_ref,
       admission.payload#>>'{data,app_ref}' AS app_ref,
       admission.payload#>>'{data,model}' AS model,
       admission.payload#>>'{data,provider}' AS provider,
       admission.payload#>>'{data,offer_id}' AS offer_id,
       (admission.payload->>'occurred_at')::timestamptz AS accepted_at,
       (admission.payload#>>'{data,estimated_usd_nanos}')::bigint AS estimated_usd_nanos,
       coalesce(usage.mock_reported_usd_nanos, 0) AS mock_reported_usd_nanos,
       usage.provider_reported_usd_nanos,
       coalesce(usage.provider_reports_missing_cost, 0) AS provider_reports_missing_cost,
       coalesce(usage.completed_outputs, 0) AS completed_outputs,
       usage.input_tokens,
       usage.output_tokens,
       latest_settlement.amount_usd_nanos AS supplier_settled_usd_nanos,
       latest_settlement.revision AS supplier_settlement_revision,
       latest_settlement.event_id AS supplier_settlement_event_id
FROM tokencraft_export.events AS admission
LEFT JOIN usage
    ON usage.source_url = admission.source_url
   AND usage.project_id = admission.project_id
   AND usage.request_id = admission.payload->>'request_id'
LEFT JOIN latest_settlement
    ON latest_settlement.source_url = admission.source_url
   AND latest_settlement.project_id = admission.project_id
   AND latest_settlement.request_id = admission.payload->>'request_id'
WHERE admission.payload->>'type' = 'request.accepted';

COMMIT;
