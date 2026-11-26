use ids::{AccountId, GatewayProjectId};
use sqlx::{PgConnection, Result};

#[derive(sqlx::FromRow)]
pub struct ToolDependency {
    pub account_id: AccountId,
    pub project_id: GatewayProjectId,
    pub binding_digest: String,
    pub operation: String,
    pub operation_revision: i64,
    pub runs: i64,
}

#[derive(sqlx::FromRow)]
pub struct GenerationDependency {
    pub account_id: AccountId,
    pub project_id: GatewayProjectId,
    pub model: String,
    pub kind: String,
    pub first_frame: bool,
    pub frame_from_upload: bool,
    pub aspect_ratio: Option<String>,
    pub duration_seconds: Option<i32>,
    pub resolution: Option<String>,
    pub max_output_tokens: Option<i32>,
    pub prompt_bytes: Option<i32>,
    pub system_bytes: Option<i32>,
    pub max_usd_nanos: i64,
    pub runs: i64,
}

async fn has_workflows(conn: &mut PgConnection) -> Result<bool> {
    sqlx::query_scalar("SELECT pg_catalog.to_regclass('public.workflow_runs') IS NOT NULL")
        .fetch_one(conn)
        .await
}

pub async fn tool_dependencies(conn: &mut PgConnection, limit: i64) -> Result<Vec<ToolDependency>> {
    if !has_workflows(conn).await? {
        return Ok(Vec::new());
    }

    sqlx::query_as(
        r#"WITH pinned AS (
             SELECT run.id, run.account_id, run.project_id,
                    node->'tool'->>'binding_revision' AS binding_digest,
                    node->>'model' AS operation, (node->'tool'->>'revision')::bigint AS operation_revision
               FROM public.workflow_runs run
               CROSS JOIN LATERAL jsonb_array_elements(run.graph->'nodes') AS node
              WHERE run.status IN ('queued', 'running', 'reconciling') AND run.deadline > now()
                AND node->>'kind' = 'tool'
           )
           SELECT account_id, project_id, binding_digest, operation, operation_revision,
                  count(DISTINCT id) AS runs
             FROM pinned
            GROUP BY account_id, project_id, binding_digest, operation, operation_revision
            ORDER BY account_id, project_id, binding_digest, operation, operation_revision
            LIMIT $1"#,
    )
    .bind(limit)
    .fetch_all(conn)
    .await
}

pub async fn generation_dependencies(conn: &mut PgConnection, limit: i64) -> Result<Vec<GenerationDependency>> {
    if !has_workflows(conn).await? {
        return Ok(Vec::new());
    }

    sqlx::query_as(
        r#"WITH requirements AS (
             SELECT run.id, run.account_id, run.project_id, node->>'model' AS model, node->>'kind' AS kind,
                    coalesce(settings ? 'first_frame', false) AS first_frame,
                    coalesce(settings#>>'{first_frame,source}' = 'request', false) AS frame_from_upload,
                    CASE WHEN settings#>>'{aspect_ratio,source}' = 'fixed'
                         THEN settings#>>'{aspect_ratio,value}' END AS aspect_ratio,
                    CASE WHEN settings#>>'{duration_seconds,source}' = 'fixed'
                         THEN (settings#>>'{duration_seconds,value}')::integer END AS duration_seconds,
                    CASE WHEN settings#>>'{resolution,source}' = 'fixed'
                         THEN settings#>>'{resolution,value}' END AS resolution,
                    CASE WHEN settings#>>'{max_output_tokens,source}' = 'fixed'
                         THEN (settings#>>'{max_output_tokens,value}')::integer END AS max_output_tokens,
                    CASE WHEN node->>'kind' = 'text' THEN
                        CASE WHEN settings#>>'{prompt,source}' = 'fixed'
                             THEN octet_length(settings#>>'{prompt,value}') ELSE 1 END
                    END AS prompt_bytes,
                    CASE WHEN node->>'kind' = 'text' AND settings ? 'system' THEN
                        CASE WHEN settings#>>'{system,source}' = 'fixed'
                             THEN octet_length(settings#>>'{system,value}') ELSE 1 END
                    END AS system_bytes,
                    (node->>'max_usd_nanos')::bigint AS max_usd_nanos
               FROM public.workflow_runs run
               CROSS JOIN LATERAL jsonb_array_elements(run.graph->'nodes') AS node
               CROSS JOIN LATERAL (SELECT node->'settings' AS settings) fields
              WHERE run.status IN ('queued', 'running', 'reconciling') AND run.deadline > now()
                AND node->>'kind' IN ('text', 'image', 'video')
           )
           SELECT account_id, project_id, model, kind, first_frame, frame_from_upload, aspect_ratio, duration_seconds,
                  resolution, max_output_tokens, prompt_bytes, system_bytes, max_usd_nanos, count(DISTINCT id) AS runs
             FROM requirements
            GROUP BY account_id, project_id, model, kind, first_frame, frame_from_upload, aspect_ratio, duration_seconds,
                     resolution, max_output_tokens, prompt_bytes, system_bytes, max_usd_nanos
            ORDER BY account_id, project_id, model, kind, first_frame, frame_from_upload, aspect_ratio, duration_seconds,
                     resolution, max_output_tokens, prompt_bytes, system_bytes, max_usd_nanos
            LIMIT $1"#,
    )
    .bind(limit)
    .fetch_all(conn)
    .await
}
