use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use db_queries::workflows::Run;
use super::super::catalog::digest;
use serde::Serialize;

use crate::error::ApiResult;

use super::{
    graph::{Graph, Kind},
    runner,
};

#[derive(Serialize)]
pub(super) struct Execution {
    title: String,
    observed_at: DateTime<Utc>,
    evidence_expired: bool,
    nodes: Vec<Node>,
}

#[derive(Serialize)]
struct Node {
    id: String,
    title: String,
    kind: Kind,
    dependencies: Vec<String>,
    status: &'static str,
    request: Option<String>,
    admitted_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

pub(super) async fn read(
    pool: &db::PgPool,
    row: &Run,
    graph: &Graph,
    progress: Option<&runner::State>,
) -> ApiResult<Execution> {
    let steps = progress.map_or(&[][..], |state| state.steps.as_slice());
    let digests: Vec<_> = graph.nodes.iter().map(|node| digest(runner::step_key(&row.id, &node.id))).collect();
    let scope = runner::scope(row);
    let requests = sqlx::query_as::<_, (String, String, DateTime<Utc>, Option<DateTime<Utc>>)>(
        "SELECT idempotency_digest, id, created_at, COALESCE(succeeded_at, failed_at, canceled_at)
         FROM gateway_requests
         WHERE account_id = $1 AND project_id = $2 AND actor_id = $3 AND app_ref = $4
           AND client_ref IS NOT DISTINCT FROM $5::text AND idempotency_digest = ANY($6::text[])",
    )
    .bind(scope.account.as_str())
    .bind(scope.project.as_str())
    .bind(row.id.as_str())
    .bind(row.workflow_id.as_str())
    .bind(scope.client_ref)
    .bind(&digests)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(digest, id, start, end)| (digest, (id, start, end)))
    .collect::<BTreeMap<_, _>>();

    let active = matches!(row.status.as_str(), "queued" | "running" | "reconciling");
    let nodes = graph
        .nodes
        .iter()
        .zip(&digests)
        .map(|(node, digest)| {
            let request = requests.get(digest);
            let step = steps.iter().find(|step| step.node == node.id);
            let failed = progress.and_then(|state| state.failed_node.as_deref())
                == Some(node.id.as_str());
            let status = match (failed, step.map(|step| step.status.as_str())) {
                (true, _) | (_, Some("failed")) => "failed",
                (_, Some("succeeded")) => "succeeded",
                (_, Some("reconciling")) if active => "reconciling",
                (_, Some(_)) if active => "running",
                (_, Some(_)) => "interrupted",
                (_, None) if progress.is_none() => "unknown",
                (_, None) if active && request.is_some() => "running",
                (_, None) if request.is_some() => "interrupted",
                (_, None) if active => "pending",
                (_, None) => "not_run",
            };
            Node {
                id: node.id.clone(),
                title: node.title.clone(),
                kind: node.kind,
                dependencies: node.dependencies(),
                status,
                request: request.map(|(id, _, _)| id.clone()).or_else(|| step.map(|step| step.request.as_str().to_owned())),
                admitted_at: request.map(|(_, start, _)| *start),
                finished_at: request.and_then(|(_, _, end)| *end),
            }
        })
        .collect();
    Ok(Execution {
        title: graph.title.clone(),
        observed_at: Utc::now(),
        evidence_expired: progress.is_none(),
        nodes,
    })
}
