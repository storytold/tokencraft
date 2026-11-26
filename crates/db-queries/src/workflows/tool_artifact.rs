use ids::WorkflowRunId;

pub struct ToolArtifact {
    pub content_type: String,
    pub sha256: String,
    pub ciphertext: Vec<u8>,
}

pub async fn read_tool_artifact<'e, E: sqlx::PgExecutor<'e>>(
    exec: E, run: &WorkflowRunId, node: &str,
) -> sqlx::Result<Option<ToolArtifact>> {
    sqlx::query_as!(ToolArtifact,
        "SELECT artifact.content_type, artifact.sha256, artifact.ciphertext
         FROM workflow_tool_artifacts artifact JOIN workflow_runs run ON run.id = artifact.run_id
         WHERE artifact.run_id = $1 AND artifact.node = $2 AND run.content IS NOT NULL
           AND (run.status NOT IN ('succeeded','failed','canceled','expired')
                OR run.updated_at >= now() - interval '7 days')",
        run.as_str(), node
    ).fetch_optional(exec).await
}

pub async fn insert_tool_artifact<'e, E: sqlx::PgExecutor<'e>>(
    exec: E, run: &WorkflowRunId, node: &str, artifact: &ToolArtifact,
) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO workflow_tool_artifacts(run_id,node,content_type,sha256,ciphertext)
         VALUES ($1,$2,$3,$4,$5)",
        run.as_str(), node, artifact.content_type, artifact.sha256, artifact.ciphertext
    ).execute(exec).await.map(|_| ())
}
