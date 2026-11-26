use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use db_queries::{gateway_requests::RequestStatus, workflows::Run};
use serde::Serialize;

use super::super::catalog::digest;
use crate::error::ApiResult;

use super::{
    activity::REQUEST_STATUS,
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
    request_status: Option<RequestStatus>,
    provider: Option<String>,
    admitted_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct RequestEvidence {
    idempotency_digest: String,
    id: String,
    status: RequestStatus,
    provider: String,
    created_at: DateTime<Utc>,
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
    let query = format!(
        "SELECT r.idempotency_digest, r.id, {REQUEST_STATUS} AS status, r.provider,
                r.created_at, COALESCE(r.succeeded_at, r.failed_at, r.canceled_at) AS finished_at
         FROM gateway_requests r
         LEFT JOIN gateway_provider_attempts a
           ON a.account_id = r.account_id AND a.request_id = r.id AND a.attempt_number = 1
         WHERE r.account_id = $1 AND r.project_id = $2 AND r.actor_id = $3 AND r.app_ref = $4
           AND r.client_ref IS NOT DISTINCT FROM $5::text AND r.idempotency_digest = ANY($6::text[])"
    );
    let requests = sqlx::query_as::<_, RequestEvidence>(&query)
    .bind(scope.account.as_str())
    .bind(scope.project.as_str())
    .bind(row.id.as_str())
    .bind(row.workflow_id.as_str())
    .bind(scope.client_ref)
    .bind(&digests)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| (row.idempotency_digest.clone(), row))
    .collect::<BTreeMap<_, _>>();

    let active = matches!(row.status.as_str(), "queued" | "running" | "reconciling");
    let nodes = graph
        .nodes
        .iter()
        .zip(&digests)
        .map(|(node, digest)| {
            let request = requests.get(digest);
            let step = steps.iter().find(|step| step.node == node.id);
            let tool = progress.and_then(|state| state.tools.get(&node.id));
            let reused = progress.and_then(|state| state.reuse.as_ref())
                .is_some_and(|reuse| reuse.completed.contains(&node.id));
            let failed = progress.and_then(|state| state.failed_node.as_deref())
                == Some(node.id.as_str());
            let status = match (failed, step.map(|step| step.status.as_str()).or_else(|| tool.map(|step| step.status.as_str()))) {
                (_, _) if reused => "reused",
                (true, _) | (_, Some("failed")) => "failed",
                (_, Some("succeeded")) => "succeeded",
                _ if row.status == "succeeded" => "succeeded",
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
                request: request.map(|evidence| evidence.id.clone()).or_else(|| step.map(|step| step.request.as_str().to_owned())),
                request_status: request.map(|evidence| evidence.status),
                provider: request.map(|evidence| evidence.provider.clone()),
                admitted_at: request.map(|evidence| evidence.created_at).or_else(|| tool.map(|progress| progress.admitted_at)),
                finished_at: request.and_then(|evidence| evidence.finished_at).or_else(|| tool.and_then(|progress| progress.finished_at)),
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
