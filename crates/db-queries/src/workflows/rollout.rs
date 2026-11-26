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

pub async fn tool_dependencies(conn: &mut PgConnection, limit: i64) -> Result<Vec<ToolDependency>> {
    let migrated: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass('public.workflow_runs') IS NOT NULL")
        .fetch_one(&mut *conn)
        .await?;
    if !migrated {
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
