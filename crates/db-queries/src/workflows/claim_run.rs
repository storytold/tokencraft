use super::Run;
use ids::{AccountId, GatewayProjectId, WorkflowId, WorkflowRunId};

pub async fn claim_run<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    tool_pins: &serde_json::Value,
) -> sqlx::Result<Option<Run>> {
    sqlx::query_as!(Run,
        r#"WITH candidate AS (
            SELECT id
            FROM workflow_runs
            WHERE status IN ('queued', 'running', 'reconciling') AND next_at <= statement_timestamp()
              AND deadline > statement_timestamp()
              AND NOT EXISTS (
                SELECT 1 FROM jsonb_array_elements(
                    CASE WHEN jsonb_typeof(graph->'nodes') = 'array' THEN graph->'nodes' ELSE '[]'::jsonb END
                ) AS step
                WHERE step->>'kind' = 'tool'
                  AND ($1::jsonb -> account_id -> project_id ? (step#>>'{tool,binding_revision}')) IS NOT TRUE
              )
            ORDER BY next_at, id LIMIT 1 FOR UPDATE SKIP LOCKED
        )
        UPDATE workflow_runs w
        SET lease = w.lease + 1,
            next_at = least(w.deadline, statement_timestamp() + interval '45 seconds')
        FROM candidate
        WHERE w.id = candidate.id
        RETURNING w.id AS "id: WorkflowRunId",
            w.account_id AS "account_id: AccountId", w.project_id AS "project_id: GatewayProjectId",
            w.workflow_id AS "workflow_id: WorkflowId", w.actor_id, w.status AS "status!", w.revision AS "revision!",
            w.lease, w.version, w.draft_revision, w.graph, w.content, w.deadline, w.created_at"#,
        tool_pins,
    )
    .fetch_optional(exec)
    .await
}
