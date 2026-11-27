use chrono::{DateTime, Utc};
use ids::{AccountId, GatewayProjectId, GatewayRequestId};
use sqlx::{PgConnection, PgExecutor, PgPool};

use super::{artifacts, connector_completions, queue_turns::QueueLane, RequestScope};

pub struct NewProviderRun<'a> {
    pub request: &'a GatewayRequestId,
    pub binding_digest: &'a str,
    pub upstream_model: &'a str,
    pub payload: &'a [u8],
}

pub async fn create_provider_run<'e, E: PgExecutor<'e>>(
    exec: E,
    run: NewProviderRun<'_>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO gateway_provider_runs (request_id, binding_digest, upstream_model, request_payload)
         VALUES ($1, $2, $3, $4)",
        run.request.as_str(), run.binding_digest, run.upstream_model, run.payload,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub struct ClaimedProviderRun {
    pub request_id: String,
    pub trace_id: Option<String>,
    pub admission_span_id: Option<String>,
    pub account_id: AccountId,
    pub project_id: GatewayProjectId,
    pub provider: String,
    pub offer_id: String,
    pub data_requirements: Option<serde_json::Value>,
    pub adapter: String,
    pub binding_digest: String,
    pub upstream_model: String,
    pub request_payload: Vec<u8>,
    pub lease: DateTime<Utc>,
    pub credential_version: Option<i32>,
}

#[derive(Clone, Copy)]
pub struct SubmitBindings<'a> {
    pub local_chat: &'a [String],
    pub connectors: &'a [String],
    pub connector_recovery: &'a [String],
}

pub async fn claim_provider_run(
    pool: &PgPool,
    bindings: SubmitBindings<'_>,
) -> sqlx::Result<Option<ClaimedProviderRun>> {
    let lane = QueueLane::ProviderSubmit;
    let Some(mut tx) = lane.begin(pool).await? else {
        return Ok(None);
    };
    let run = sqlx::query_as!(
        ClaimedProviderRun,
        r#"WITH ready AS (
             SELECT r.id FROM gateway_requests r JOIN gateway_provider_runs p ON p.request_id = r.id
             LEFT JOIN gateway_queue_turns q ON q.project_id = r.project_id AND q.lane = $3
             WHERE ((r.adapter = 'openai_compatible_chat' AND p.binding_digest = ANY($1::text[]))
                 OR (r.adapter = 'connector_queue' AND p.binding_digest = ANY($2::text[])))
               AND r.dispatched_at IS NULL AND r.canceled_at IS NULL AND r.failed_at IS NULL
               AND r.dispatch_after <= now() AND r.dispatch_after > now() - interval '15 minutes'
               AND r.created_at > now() - interval '15 minutes'
               AND p.request_payload IS NOT NULL
               AND NOT EXISTS (
                 SELECT 1 FROM gateway_dispatch_controls control WHERE control.account_id = r.account_id
                   AND (control.pause_all OR r.provider = ANY(control.paused_providers))
               )
               AND (r.adapter NOT IN ('fal_queue', 'kinovi_queue', 'connector_queue') OR NOT EXISTS (
                 SELECT 1 FROM gateway_queue_cooldowns c
                 WHERE c.account_id = r.account_id AND c.project_id = r.project_id
                   AND c.binding_digest = p.binding_digest AND c.upstream_model = p.upstream_model
                   AND c.available_after > now()
               ))
             ORDER BY coalesce(q.turn, 0), r.dispatch_after, r.sid
             LIMIT 1 FOR UPDATE OF r SKIP LOCKED
           ), started AS (
             UPDATE gateway_requests r SET dispatched_at = statement_timestamp(),
                                            guarded_dispatch_at = statement_timestamp()
             FROM ready WHERE r.id = ready.id
             RETURNING r.id, r.account_id, r.project_id, r.provider, r.adapter, r.provider_credential_version,
                       r.trace_id, r.admission_span_id, r.offer_id,
                       r.quote #> '{routing,data_requirements}' AS data_requirements
           ), served AS (
             INSERT INTO gateway_queue_turns (project_id, lane)
             SELECT project_id, $3 FROM started
             ON CONFLICT (project_id, lane) DO UPDATE SET turn = excluded.turn
           )
           UPDATE gateway_provider_runs p SET lease_until = clock_timestamp() + interval '40 seconds',
               connector_recovery_enabled = started.adapter = 'connector_queue'
                   AND p.binding_digest = ANY($4::text[])
           FROM started WHERE p.request_id = started.id
           RETURNING p.request_id, started.adapter AS "adapter!", p.binding_digest, p.upstream_model,
                     started.account_id AS "account_id!: AccountId", started.project_id AS "project_id!: GatewayProjectId",
                     started.provider AS "provider!",
                     coalesce(p.route_retries->-1->'decision'->>'next_offer_id', started.offer_id) AS "offer_id!",
                     started.data_requirements AS "data_requirements?",
                     started.trace_id AS "trace_id?", started.admission_span_id AS "admission_span_id?",
                     p.request_payload AS "request_payload!", p.lease_until AS "lease!",
                     started.provider_credential_version AS "credential_version?""#,
        bindings.local_chat,
        bindings.connectors,
        lane.key(),
        bindings.connector_recovery,
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(run)
}

pub enum RunOutcome<'a> {
    Completed {
        payload: &'a [u8],
        outputs: i16,
        artifacts: &'a [artifacts::NewArtifact],
    },
    Rejected(&'a str),
    Uncertain(&'a str),
}

#[derive(Default)]
pub struct UpstreamObservation<'a> {
    pub handle: Option<&'a str>,
    pub model: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub finish_reason: Option<&'a str>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub reported_usd_nanos: Option<i64>,
    pub partial_usage: bool,
}

pub async fn finish_provider_run(
    pool: &PgPool,
    request: &str,
    lease: DateTime<Utc>,
    outcome: RunOutcome<'_>,
    observation: UpstreamObservation<'_>,
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN ISOLATION LEVEL READ COMMITTED").await?;
    let outcome = match outcome {
        RunOutcome::Rejected("connector_rejected")
            if connector_completions::lock_request(&mut tx, request).await?
                && connector_completions::received(&mut tx, request).await? =>
        {
            RunOutcome::Uncertain("connector_submission_unconfirmed")
        }
        outcome => outcome,
    };
    finish_provider_run_in(&mut tx, request, lease, outcome, observation).await?;
    tx.commit().await
}

pub(super) async fn finish_provider_run_in(
    conn: &mut PgConnection,
    request: &str,
    lease: DateTime<Utc>,
    outcome: RunOutcome<'_>,
    observation: UpstreamObservation<'_>,
) -> sqlx::Result<bool> {
    let (state, failure_code, output, outputs, media) = match outcome {
        RunOutcome::Completed {
            payload,
            outputs,
            artifacts,
        } => ("succeeded", None, Some(payload), outputs, artifacts),
        RunOutcome::Rejected(code) => ("failed", Some(code), None, 0, &[][..]),
        RunOutcome::Uncertain(code) => ("uncertain", Some(code), None, 0, &[][..]),
    };
    let updated = sqlx::query!(
        "UPDATE gateway_requests r SET
            succeeded_at = CASE WHEN $2::text = 'succeeded' THEN clock_timestamp() END,
            failed_at = CASE WHEN $2 = 'failed' THEN clock_timestamp() END,
            uncertain_at = CASE WHEN $2 = 'uncertain' THEN coalesce(r.uncertain_at, clock_timestamp()) END
         FROM gateway_provider_runs p
         WHERE r.id = $1 AND p.request_id = r.id AND p.lease_until = $3
           AND p.lease_until > clock_timestamp()
           AND (p.poll_deadline IS NULL OR p.poll_deadline > clock_timestamp())
           AND num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at) = 0
           AND (r.uncertain_at IS NULL OR (p.recheck_requested_at IS NOT NULL AND p.poll_deadline IS NOT NULL))
         RETURNING r.account_id, r.adapter",
        request,
        state,
        lease,
    )
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = updated {
        let expiry = sqlx::query_scalar!(
            "UPDATE gateway_provider_runs SET request_payload = NULL, lease_until = NULL, error_code = $2,
               response_payload = $3, result_expires_at = CASE
                 WHEN $3::bytea IS NOT NULL THEN coalesce(
                   (SELECT archive_at FROM gateway_content_history WHERE request_id = $1),
                   clock_timestamp() + CASE WHEN $9 THEN interval '24 hours' ELSE interval '1 hour' END) END,
               upstream_handle = coalesce($4::text, upstream_handle), reported_model = $5,
               reported_provider = $6, finish_reason = $7, poll_after = NULL, poll_deadline = NULL,
               queue_state = CASE WHEN accepted_at IS NOT NULL AND $8::text IN ('succeeded', 'failed')
                   THEN 'completed' ELSE queue_state END
             WHERE request_id = $1 RETURNING coalesce(
               (SELECT expires_at FROM gateway_content_history WHERE request_id = $1),
               result_expires_at) AS artifact_expiry",
            request, failure_code, output, observation.handle, observation.model, observation.provider, observation.finish_reason,
            state, !media.is_empty(),
        )
        .fetch_one(&mut *conn)
        .await?;
        if let Some(expires_at) = expiry {
            artifacts::insert(conn, request, expires_at, media).await?;
        }
        if output.is_some() || observation.partial_usage {
            let source = match (row.adapter.as_str(), observation.partial_usage) {
                ("openai_compatible_chat", true) => "local_chat_stream_partial",
                ("openai_compatible_chat", false) => "local_chat_report",
                ("fal_queue", _) => "fal_result",
                ("kinovi_queue", _) => "kinovi_result",
                ("connector_queue", _) => "connector_result",
                (_, true) => "openrouter_stream_partial",
                _ => "openrouter_report",
            };
            sqlx::query!(
                "INSERT INTO gateway_usage_observations
                   (account_id, request_id, attempt_number, source, amount_usd_nanos, output_count, reported_at, input_tokens, output_tokens)
                 VALUES ($1, $2, 1, $3, $4, $5, clock_timestamp(), $6, $7)",
                row.account_id, request,
                source,
                observation.reported_usd_nanos, outputs, observation.input_tokens, observation.output_tokens,
            )
            .execute(conn)
            .await?;
        }
        return Ok(true);
    }
    Ok(false)
}

pub async fn expire_provider_work(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query!(
        "WITH expired AS (
           SELECT r.id, r.dispatched_at IS NULL AS queued, p.accepted_at IS NOT NULL AS acknowledged
           FROM gateway_provider_runs p JOIN gateway_requests r ON r.id = p.request_id
           WHERE num_nonnulls(r.succeeded_at, r.failed_at, r.canceled_at) = 0
             AND (r.uncertain_at IS NULL OR (p.recheck_requested_at IS NOT NULL AND p.poll_deadline IS NOT NULL))
             AND ((p.lease_until <= clock_timestamp() AND p.accepted_at IS NULL)
                OR p.poll_deadline <= clock_timestamp()
                OR (r.dispatched_at IS NULL AND p.request_payload IS NOT NULL
                    AND LEAST(r.created_at, r.dispatch_after) <= clock_timestamp() - interval '15 minutes'))
           ORDER BY coalesce(p.poll_deadline, p.lease_until, r.dispatch_after)
           LIMIT 32 FOR UPDATE OF r SKIP LOCKED
         ), closed AS (
           UPDATE gateway_requests r SET
               failed_at = CASE WHEN expired.queued THEN clock_timestamp() END,
               uncertain_at = CASE WHEN NOT expired.queued THEN coalesce(r.uncertain_at, clock_timestamp()) END
           FROM expired WHERE r.id = expired.id
           RETURNING r.id, expired.queued, expired.acknowledged
         )
         UPDATE gateway_provider_runs p SET request_payload = NULL, lease_until = NULL,
             poll_after = NULL, poll_deadline = NULL,
             error_code = CASE WHEN closed.queued THEN 'queued_payload_expired'
               WHEN closed.acknowledged THEN 'provider_poll_deadline'
               WHEN p.stream_started_at IS NOT NULL THEN 'provider_stream_interrupted'
               ELSE 'submission_interrupted' END
         FROM closed WHERE p.request_id = closed.id",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn clear_provider_input<'e, E: PgExecutor<'e>>(
    exec: E,
    request: &GatewayRequestId,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE gateway_provider_runs SET request_payload = NULL WHERE request_id = $1 AND request_payload IS NOT NULL",
        request.as_str(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub struct ProviderResult {
    pub payload: Vec<u8>,
    pub expires_at: DateTime<Utc>,
    pub adapter: String,
}

pub async fn provider_result(
    pool: &PgPool,
    scope: RequestScope<'_>,
    request: &GatewayRequestId,
) -> sqlx::Result<Option<ProviderResult>> {
    sqlx::query_as!(
        ProviderResult,
        r#"SELECT p.response_payload AS "payload!", p.result_expires_at AS "expires_at!", r.adapter
           FROM gateway_requests r JOIN gateway_provider_runs p ON p.request_id = r.id
           WHERE r.account_id = $1 AND r.project_id = $2 AND ($3::text IS NULL OR r.client_ref = $3)
             AND r.id = $4 AND r.content_removed_at IS NULL
             AND p.result_expires_at > statement_timestamp() AND p.response_payload IS NOT NULL"#,
        scope.account.as_str(), scope.project.as_str(), scope.client_ref, request.as_str(),
    )
    .fetch_optional(pool)
    .await
}
