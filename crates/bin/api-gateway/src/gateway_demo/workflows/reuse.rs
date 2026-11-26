use std::collections::{BTreeMap, BTreeSet};

use db_queries::workflows as db;
use ids::WorkflowRunId;
use serde::{Deserialize, Serialize};

use super::super::artifacts::{self, ScopedDownload};
use super::graph::{InputValues, Output, Plan};
use super::runner::{self, Runtime, State};
use super::{tool_execution, Security};
use crate::error::{ApiError, ApiResult};

// Older workers reject the stored graph before dispatch rather than rerunning a reuse job.
pub(super) const STORED_GRAPH_SCHEMA: u8 = 2;

#[derive(Deserialize, Serialize)]
pub(super) struct Reuse {
    pub source_run_id: WorkflowRunId,
    pub pending: BTreeMap<String, Output>,
    pub completed: BTreeSet<String>,
}

pub(super) fn fingerprints(security: &Security, inputs: &InputValues) -> ApiResult<BTreeMap<String, String>> {
    inputs.iter().map(|(name, value)| {
        let encoded = serde_json::to_vec(&("workflow-input-v1", name, value))
            .map_err(|_| runner::unavailable())?;
        Ok((name.clone(), security.digest(&encoded)))
    }).collect()
}

pub(super) fn prepare(source: WorkflowRunId, previous: State, next: &State, plan: &Plan<'_>) -> ApiResult<Reuse> {
    let old = previous.input_fingerprints.as_ref().ok_or_else(unavailable)?;
    let new = next.input_fingerprints.as_ref().ok_or_else(runner::unavailable)?;
    let mut outputs = previous.outputs;
    let mut pending = BTreeMap::new();
    // The plan is in dependency order. A changed input invalidates all downstream outputs.
    for node in &plan.nodes {
        let unchanged = node.request_inputs().into_iter().all(|field| {
            old.get(field).zip(new.get(field)).is_some_and(|(before, after)| before == after)
        });
        if unchanged && node.dependencies().iter().all(|step| pending.contains_key(step)) {
            if let Some(output) = outputs.remove(&node.id) {
                pending.insert(node.id.clone(), output);
            }
        }
    }
    if pending.is_empty() {
        return Err(ApiError::Conflict("the source run has no reusable steps for these inputs".into()));
    }
    Ok(Reuse { source_run_id: source, pending, completed: BTreeSet::new() })
}

pub(super) fn unavailable() -> ApiError {
    ApiError::Conflict("the source run is unavailable for reuse on this workflow revision".into())
}

pub(super) async fn materialize(
    runtime: &Runtime, row: &db::Run, source: &WorkflowRunId, node: &str, output: &Output,
) -> ApiResult<Option<db::ToolArtifact>> {
    match output {
        Output::Tool { node: output_node, .. } if output_node == node => {
            tool_execution::copy_artifact(runtime, row, source, node).await.map(Some)
        }
        Output::Tool { .. } => Err(runner::unavailable()),
        Output::Image { request_id } | Output::Video { request_id } => {
            artifacts::read_verified(ScopedDownload {
                pool: &runtime.pool, scope: runner::scope(row), runtime: runtime.providers.as_deref(),
                request: request_id, slot: 0,
            }).await?;
            Ok(None)
        }
        Output::Text { .. } | Output::Fixture { .. } => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::gateway_demo::workflows::graph::Graph;

    fn finishing_graph() -> Graph {
        let binding = "a".repeat(64);
        serde_json::from_value(json!({
            "schema": 1, "title": "Finishing", "max_usd_nanos": 0,
            "inputs": {
                "video": {"title":"Video","kind":"video"},
                "narration": {"title":"Narration","kind":"audio"},
                "start": {"title":"Start","kind":"parameter","parameter":{"type":"integer","minimum":0,"maximum":600000}},
                "delay": {"title":"Delay","kind":"parameter","parameter":{"type":"integer","minimum":0,"maximum":600000}}
            },
            "nodes": [
                {"id":"trim","title":"Trim","kind":"tool","model":"video.trim","max_usd_nanos":0,
                 "settings":{"video":{"source":"request","field":"video"}},
                 "tool":{"binding_revision":binding,"revision":1,"inputs":{"video":"video"},"output":"video",
                         "parameters":{"duration_ms":1000},"parameter_inputs":{"start_ms":"start"}}},
                {"id":"audio","title":"Audio","kind":"tool","model":"video.attach_audio","max_usd_nanos":0,
                 "settings":{"video":{"source":"step","step":"trim"},"audio":{"source":"request","field":"narration"}},
                 "tool":{"binding_revision":binding,"revision":2,"inputs":{"video":"video","audio":"audio"},"output":"video",
                         "parameters":{},"parameter_inputs":{"offset_ms":"delay"}}}
            ],
            "outputs": {"master":"audio"}
        })).unwrap()
    }

    fn state(fingerprints: Value) -> State {
        serde_json::from_value(json!({
            "inputs": {}, "input_fingerprints": fingerprints, "steps": [],
            "outputs": {"trim":{"type":"tool","node":"trim","kind":"video"},
                        "audio":{"type":"tool","node":"audio","kind":"video"}}
        })).unwrap()
    }

    #[test]
    fn changing_only_delay_reuses_trim_and_invalidating_trim_invalidates_audio() {
        let graph = finishing_graph();
        let plan = graph.compile().unwrap();
        let prior = || state(json!({"video":"a","narration":"b","start":"c","delay":"d"}));
        let changed_delay = state(json!({"video":"a","narration":"b","start":"c","delay":"e"}));
        let saved = prepare(WorkflowRunId::generate(), prior(), &changed_delay, &plan).unwrap();
        assert_eq!(saved.pending.keys().map(String::as_str).collect::<Vec<_>>(), ["trim"]);
        assert!(saved.completed.is_empty());

        let changed_start = state(json!({"video":"a","narration":"b","start":"different","delay":"d"}));
        assert!(matches!(
            prepare(WorkflowRunId::generate(), prior(), &changed_start, &plan),
            Err(ApiError::Conflict(_))
        ));
        assert!(matches!(
            prepare(WorkflowRunId::generate(), state(Value::Null), &changed_delay, &plan),
            Err(ApiError::Conflict(_))
        ));
    }

    #[test]
    fn a_saved_video_generation_is_reused_until_its_prompt_changes() {
        let mut graph = finishing_graph();
        let input = serde_json::from_value(json!({"title":"Prompt","kind":"text"})).unwrap();
        graph.inputs.insert("brief".into(), input);
        graph.inputs.remove("video");
        graph.nodes[0].settings.insert("video".into(), super::super::graph::Binding::Step { step: "generate".into() });
        graph.nodes.push(serde_json::from_value(json!({
            "id":"generate","title":"Generate","kind":"video","model":"example_video","max_usd_nanos":100,
            "settings": {
                "prompt":{"source":"template","text":"A clip of {{input.brief}}"},
                "duration_seconds":{"source":"fixed","value":5},
                "aspect_ratio":{"source":"fixed","value":"16:9"},
                "resolution":{"source":"fixed","value":"720p"}
            }
        })).unwrap());
        graph.max_usd_nanos = 100;
        let plan = graph.compile().unwrap();
        let prior = || {
            let mut previous = state(json!({"brief":"a","narration":"b","start":"c","delay":"d"}));
            previous.outputs.insert("generate".into(), Output::Video { request_id: ids::GatewayRequestId::generate() });
            previous
        };
        let changed_delay = state(json!({"brief":"a","narration":"b","start":"c","delay":"e"}));
        let reuse = prepare(WorkflowRunId::generate(), prior(), &changed_delay, &plan).unwrap();
        assert_eq!(reuse.pending.keys().map(String::as_str).collect::<Vec<_>>(), ["generate","trim"]);

        let changed_brief = state(json!({"brief":"different","narration":"b","start":"c","delay":"d"}));
        assert!(matches!(prepare(WorkflowRunId::generate(), prior(), &changed_brief, &plan), Err(ApiError::Conflict(_))));
    }
}
