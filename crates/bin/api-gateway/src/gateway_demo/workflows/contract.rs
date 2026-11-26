use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::graph::{Graph, ValueType, MAX_INPUT_BYTES};
use super::tool_execution::MAX_ARTIFACT;
use super::tool_plugins::upload_content_types;
use super::{MAX_ACTIVE_RUNS_PER_APP, MAX_ACTIVE_RUNS_PER_WORKFLOW};

pub(super) fn document(graph: &Graph, endpoint: &str, version: i32) -> Value {
    let mut fields = BTreeMap::new();
    let mut required = Vec::new();
    let mut example = BTreeMap::new();
    let mut upload_inputs = BTreeMap::new();
    let mut upload_content = BTreeMap::new();
    for (name, field) in &graph.inputs {
        let (mut schema, sample) = match field.kind {
            ValueType::Text => (
                json!({"type":"string","minLength":1,"maxLength":MAX_INPUT_BYTES}),
                json!("Example value"),
            ),
            ValueType::Integer => (json!({"type":"integer","minimum":1,"maximum":32000}), json!(1)),
            kind => {
                upload_inputs.insert(name, kind);
                for content_type in upload_content_types(kind) {
                    upload_content.insert(content_type, json!({"schema":{"type":"string","format":"binary"}}));
                }
                (json!({
                    "type":"object", "required":["upload_id"], "additionalProperties":false,
                    "properties":{"upload_id":{"type":"string","description":"ID returned by uploadWorkflowInput"}}
                }), json!({"upload_id":"REPLACE_WITH_UPLOAD_ID"}))
            }
        };
        if let Some(default) = &field.default {
            schema["default"] = json!(default);
        } else {
            required.push(name);
            example.insert(name, sample);
        }
        fields.insert(name, schema);
    }
    let output_types: BTreeMap<_, _> = graph.outputs.iter()
        .filter_map(|(name, node)| graph.output_kind(node).map(|kind| (name, kind)))
        .collect();
    let run = format!("{endpoint}/runs/{{run_id}}");
    let run_id = json!({"in":"path","name":"run_id","required":true,"schema":{"type":"string"}});
    let gateway_error = json!({"$ref":"#/components/responses/GatewayError"});
    let mut accepted = json_response("Run accepted", "WorkflowReceipt");
    accepted["headers"] = json!({
        "Location": {"description":"Relative run status URL","schema":{"type":"string"}},
        "Retry-After": {"description":"Suggested polling delay in seconds","schema":{"type":"integer"}},
    });
    let mut replay = accepted.clone();
    replay["description"] = json!("Previously accepted run returned for the same key and input");
    let mut status = json_response("Run status and permitted outputs", "WorkflowRun");
    status["headers"] = json!({"ETag":{"schema":{"type":"string"}}});
    let throttled = retry_response(&format!(
        "An application can have {MAX_ACTIVE_RUNS_PER_APP} active runs. A workflow can have \
         {MAX_ACTIVE_RUNS_PER_WORKFLOW} active runs across its applications and draft tests. \
         New runs also share the organization's submission rate limit. \
         Wait for Retry-After and retry with the same idempotency key."
    ));
    let mut contract = json!({
        "openapi":"3.1.0", "info":{"title":graph.title,"version":version.to_string()},
        "components":{
            "securitySchemes":{"workflowKey":{"type":"http","scheme":"bearer"}},
            "schemas":schemas(),
            "responses":{"GatewayError":{
                "description":"The gateway could not complete the operation",
                "content":{
                    "application/json":{"schema":{"$ref":"#/components/schemas/GatewayError"}},
                    "text/plain":{"schema":{"type":"string"}}
                }
            }}
        },
        "security":[{"workflowKey":[]}],
        "paths":{
            (format!("{endpoint}/openapi.json")):{"get":{
                "operationId":"getWorkflowContract", "x-workflow-required-scope":"runs:submit",
                "responses":{"200":{"description":"Published OpenAPI document","content":{"application/json":{"schema":{"type":"object"}}}},"default":gateway_error}
            }},
            (format!("{endpoint}/runs")):{"post":{
                "operationId":"invokeWorkflow", "x-workflow-required-scope":"runs:submit",
                "parameters":[{"in":"header","name":"Idempotency-Key","required":true,"schema":{"type":"string","minLength":8,"maxLength":128}}],
                "requestBody":{"required":true,"content":{"application/json":{
                    "schema":{"type":"object","required":["inputs"],"additionalProperties":false,
                        "properties":{"inputs":{"type":"object","additionalProperties":false,"properties":fields,"required":required}}},
                    "example":{"inputs":example}
                }}},
                "responses":{"202":accepted,"200":replay,
                    "409":json_response("The workflow is paused, the invocation conflicts with an earlier submission, or the spending reservation exceeds a monthly limit.", "GatewayError"),
                    "429":throttled,"default":gateway_error}
            }},
            (&run):{
                "parameters":[run_id],
                "get":{
                    "operationId":"getWorkflowRun", "x-workflow-required-scope":"runs:read",
                    "description":"Read a run owned by this application. Outputs also require outputs:read.",
                    "parameters":[{"in":"header","name":"If-None-Match","schema":{"type":"string"}}],
                    "responses":{"200":status,"304":{"description":"Status unchanged","headers":{"ETag":{"schema":{"type":"string"}}}},"default":gateway_error}
                }
            },
            (format!("{run}/cancel")):{
                "parameters":[run_id],
                "post":{
                    "operationId":"cancelWorkflowRun", "x-workflow-required-scope":"runs:cancel",
                    "description":"Stop admitting work for this run. Already admitted jobs can continue.",
                    "responses":{"204":{"description":"Run canceled"},"409":gateway_error,"default":gateway_error}
                }
            }
        },
        "x-workflow-example-inputs":example,
        "x-workflow-output-types":output_types,
    });
    let artifact_names: Vec<_> = output_types.iter()
        .filter(|(_, kind)| **kind != ValueType::Text)
        .map(|(name, _)| name)
        .collect();
    if !artifact_names.is_empty() {
        contract["paths"][format!("{run}/outputs/{{name}}")] = json!({
            "parameters":[run_id,{"in":"path","name":"name","required":true,"schema":{"type":"string","enum":artifact_names}}],
            "get":{
                "operationId":"downloadWorkflowOutput", "x-workflow-required-scope":"outputs:read",
                "description":"Fetch the download_url of a permitted output before its content expires.",
                "responses":{
                    "200":{"description":"Artifact bytes in their original media type","content":{"*/*":{"schema":{"type":"string","format":"binary"}}}},
                    "404":gateway_error,"default":gateway_error
                }
            }
        });
    }
    if !upload_content.is_empty() {
        let busy = retry_response("This API process is busy. Retry the same upload.");
        contract["paths"][format!("{endpoint}/uploads")] = json!({"post":{
            "operationId":"uploadWorkflowInput", "x-workflow-required-scope":"runs:submit",
            "description":format!("Send the raw file with its exact Content-Length, up to {MAX_ARTIFACT} bytes. Use the returned upload_id in a run submitted with the same application credential."),
            "parameters":[{"in":"header","name":"X-TokenCraft-Content-Sha256","required":true,"schema":{"type":"string","pattern":"^[0-9a-f]{64}$"}}],
            "requestBody":{"required":true,"content":upload_content},
            "responses":{"201":json_response("Scoped upload ID and expiration", "WorkflowUpload"),"503":busy,"default":gateway_error}
        }});
        contract["x-workflow-upload-inputs"] = json!(upload_inputs);
    }
    contract
}

fn json_response(description: &str, schema: &str) -> Value {
    json!({"description":description,"content":{"application/json":{
        "schema":{"$ref":format!("#/components/schemas/{schema}")}
    }}})
}

fn retry_response(description: &str) -> Value {
    let mut response = json_response(description, "GatewayError");
    response["headers"] = json!({
        "Retry-After": {"description":"Suggested retry delay in seconds","schema":{"type":"integer"}}
    });
    response
}

fn schemas() -> Value {
    json!({
        "WorkflowReceipt":{
            "type":"object", "required":["id","workflow_version","status_url","replayed"],
            "properties":{
                "id":{"type":"string"},"workflow_version":{"type":["integer","null"]},
                "status_url":{"type":"string"},"replayed":{"type":"boolean"}
            }
        },
        "WorkflowUpload":{
            "type":"object", "required":["upload_id","expires_at"],
            "properties":{"upload_id":{"type":"string"},"expires_at":{"type":"string","format":"date-time"}}
        },
        "WorkflowRun":{
            "type":"object", "required":["id","status","workflow_version","draft_revision","created_at","steps","error"],
            "properties":{
                "id":{"type":"string"},"workflow_version":{"type":["integer","null"]},"draft_revision":{"type":"integer"},
                "created_at":{"type":"string","format":"date-time"},"error":{"type":["string","null"]},
                "status":{"enum":["queued","running","reconciling","succeeded","failed","canceled","expired"]},
                "steps":{"type":"array","items":{"type":"object","required":["node","request","status"],
                    "properties":{"node":{"type":"string"},"request":{"type":"string"},"status":{"type":"string"}}}},
                "tools":{"type":"object","additionalProperties":{"type":"object","required":["request_id","status","admitted_at","finished_at"],
                    "properties":{"request_id":{"type":"string"},"status":{"type":"string"},
                        "admitted_at":{"type":"string","format":"date-time"},"finished_at":{"type":["string","null"],"format":"date-time"}}}},
                "outputs":{"type":"object","additionalProperties":{"$ref":"#/components/schemas/WorkflowOutput"}}
            }
        },
        "WorkflowOutput":{"oneOf":[
            {"type":"object","required":["type","text"],"properties":{"type":{"const":"text"},"text":{"type":"string"}}},
            {"type":"object","required":["type","kind","download_url"],"properties":{"type":{"const":"media"},"kind":{"type":"string"},"download_url":{"type":"string"}}},
            {"type":"object","required":["type","kind"],"properties":{"type":{"const":"fixture"},"kind":{"type":"string"}}}
        ]},
        "GatewayError":{"type":"object","required":["error"],"properties":{"error":{
            "type":"object","required":["code","message"],"properties":{"code":{"type":"string"},"message":{"type":"string"}}
        }}}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENDPOINT: &str = "/v1/projects/project/workflow-endpoints/workflow";

    #[test]
    fn file_workflow_documents_uploads_receipts_polling_and_outputs() {
        let graph: Graph = serde_json::from_value(json!({
            "schema": 1, "title": "Image output", "max_usd_nanos": 0,
            "inputs": { "source": { "title": "Input image", "kind": "image" } },
            "nodes": [{ "id": "resize", "title": "Resize", "kind": "tool", "model": "image.resize",
                "max_usd_nanos": 0, "settings": { "image": { "source": "request", "field": "source" } },
                "tool": { "binding_revision": "fixture", "revision": 1, "inputs": { "image": "image" },
                    "output": "image", "parameters": { "width": 12, "height": 42 } } }],
            "outputs": { "image": "resize" }
        })).unwrap();
        let doc = document(&graph, ENDPOINT, 2);
        let paths = &doc["paths"];
        let uploads = &paths[format!("{ENDPOINT}/uploads")]["post"];
        let content = uploads["requestBody"]["content"].as_object().unwrap();
        assert_eq!(content.keys().map(String::as_str).collect::<Vec<_>>(),
            ["image/gif", "image/jpeg", "image/png", "image/webp"]);
        assert_eq!(doc["x-workflow-upload-inputs"], json!({ "source": "image" }));

        let invoke = &paths[format!("{ENDPOINT}/runs")]["post"];
        for code in ["200", "202"] {
            assert_eq!(invoke["responses"][code]["headers"]["Retry-After"]["schema"]["type"], "integer");
            assert_eq!(invoke["responses"][code]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/WorkflowReceipt");
        }
        let status = &paths[format!("{ENDPOINT}/runs/{{run_id}}")]["get"];
        assert_eq!(status["parameters"][0]["name"], "If-None-Match");
        assert!(status["responses"]["304"].get("content").is_none());
        let cancel = &paths[format!("{ENDPOINT}/runs/{{run_id}}/cancel")]["post"];
        assert_eq!(cancel["x-workflow-required-scope"], "runs:cancel");
        let output = &paths[format!("{ENDPOINT}/runs/{{run_id}}/outputs/{{name}}")];
        assert_eq!(output["parameters"][1]["schema"]["enum"], json!(["image"]));
        assert_eq!(output["get"]["x-workflow-required-scope"], "outputs:read");
    }

    #[test]
    fn text_workflow_omits_file_only_operations_and_preserves_defaults() {
        let graph: Graph = serde_json::from_value(json!({
            "schema": 1, "title": "Text output", "max_usd_nanos": 0,
            "inputs": { "prompt": { "title": "Prompt", "kind": "text", "default": "A default" } },
            "nodes": [{ "id": "chat", "title": "Chat", "kind": "text", "model": "example_text",
                "max_usd_nanos": 0, "settings": {} }],
            "outputs": { "reply": "chat" }
        })).unwrap();
        let doc = document(&graph, ENDPOINT, 1);
        let paths = doc["paths"].as_object().unwrap();
        assert!(!paths.contains_key(&format!("{ENDPOINT}/uploads")));
        assert!(!paths.contains_key(&format!("{ENDPOINT}/runs/{{run_id}}/outputs/{{name}}")));
        let invoke = &paths[&format!("{ENDPOINT}/runs")]["post"];
        let input = &invoke["requestBody"]["content"]["application/json"]["schema"]["properties"]["inputs"];
        assert_eq!(input["properties"]["prompt"]["default"], "A default");
        assert_eq!(input["required"], json!([]));
    }
}
