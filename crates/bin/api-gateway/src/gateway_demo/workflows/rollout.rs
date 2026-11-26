use std::collections::BTreeMap;

use db_queries::workflows::ToolDependency;
use serde::Serialize;

use super::tool_plugins::Bindings;

#[derive(Serialize)]
pub(in crate::gateway_demo) struct Report<'a> {
    pub checked_workflow_tool_references: i64,
    pub workflow_tool_blockers: Vec<Blocker<'a>>,
}

#[derive(Serialize)]
pub(in crate::gateway_demo) struct Blocker<'a> {
    binding_sha256: &'a str,
    operation: &'a str,
    operation_revision: i64,
    runs: i64,
    reason: &'static str,
}

pub(in crate::gateway_demo) fn check(dependencies: &[ToolDependency]) -> anyhow::Result<Report<'_>> {
    let bindings = Bindings::load()?;
    let mut blocked = BTreeMap::new();
    for dependency in dependencies {
        if let Some(reason) = bindings.rollout_blocker(dependency) {
            let key = (
                dependency.binding_digest.as_str(),
                dependency.operation.as_str(),
                dependency.operation_revision,
                reason,
            );
            *blocked.entry(key).or_insert(0) += dependency.runs;
        }
    }
    let workflow_tool_blockers = blocked
        .into_iter()
        .map(|((binding_sha256, operation, operation_revision, reason), runs)| Blocker {
            binding_sha256,
            operation,
            operation_revision,
            runs,
            reason,
        })
        .collect();
    Ok(Report {
        checked_workflow_tool_references: dependencies.iter().map(|dependency| dependency.runs).sum(),
        workflow_tool_blockers,
    })
}
