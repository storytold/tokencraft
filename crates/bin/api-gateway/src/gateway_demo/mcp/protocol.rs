use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use db_queries::gateway_mcp::Grant;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

use super::{oauth, tools::Gateway, SharedSettings};
use super::super::{access::Permission, catalog_refresh::CatalogStore, providers::ProviderRuntime, quote_seals::QuoteSeals};
use crate::{error::{ApiError, ApiResult}, state::AppState};

const VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const SERVER: &str = "TokenCraft";
const INSTRUCTIONS: &str = "List TokenCraft models and inspect their capabilities. Quote a media generation before submitting. Show the estimate and adjustments and obtain the user's approval. Use the exact quoted intent and reuse the same key on any retry. Keep the returned request ID for status, results and traces. Amounts in USD nanos are estimates unless marked reported or settled.";

#[derive(Serialize)]
struct RpcResult<T> {
    jsonrpc: &'static str,
    id: ValueId,
    result: T,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum ValueId {
    Number(i64),
    Text(String),
}

#[derive(Serialize)]
struct RpcError {
    jsonrpc: &'static str,
    id: Option<ValueId>,
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: i32,
    message: &'static str,
}

pub(super) async fn handle(
    State(state): State<AppState>,
    Extension(settings): Extension<SharedSettings>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(quote_seals): Extension<Arc<QuoteSeals>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(origin) = headers.get(header::ORIGIN) {
        if origin.to_str().ok() != Some(settings.origin.as_str())
            || headers.get_all(header::ORIGIN).iter().count() != 1
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    if headers.get("mcp-protocol-version").is_some_and(|value| {
        !value.to_str().is_ok_and(|version| VERSIONS.contains(&version))
    }) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok());
    if !content_type.is_some_and(|value| value.split(';').next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json")))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }

    let grant = match oauth::authenticate(&state, &settings, &headers).await {
        Ok(grant) => grant,
        Err(error) => return error,
    };
    let frame: Value = match serde_json::from_slice(&body) {
        Ok(frame) => frame,
        Err(_) => return error(None, -32700, "Parse error"),
    };
    let Some(method) = frame.get("method").and_then(Value::as_str) else {
        return error(None, -32600, "Invalid request");
    };
    if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return error(None, -32600, "Invalid request");
    }
    let Some(id) = frame.get("id") else {
        return StatusCode::ACCEPTED.into_response();
    };
    let Ok(id) = ValueId::deserialize(id) else {
        return error(None, -32600, "Invalid request ID");
    };
    let params = frame.get("params").unwrap_or(&Value::Null);
    match method {
        "initialize" => {
            let Some(version) = params.get("protocolVersion").and_then(Value::as_str) else {
                return error(Some(id), -32602, "Invalid initialize parameters");
            };
            let version = if VERSIONS.contains(&version) { version } else { VERSIONS[0] };
            response(id, json!({
                "protocolVersion": version,
                "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"tools": {}},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => response(id, json!({})),
        "tools/list" => {
            let tools: Vec<_> = Tool::ALL.iter().map(|tool| tool.descriptor()).collect();
            response(id, json!({"tools": tools}))
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return error(Some(id), -32602, "A tool name is required");
            };
            let Some(tool) = Tool::ALL.into_iter().find(|tool| tool.name() == name) else {
                return error(Some(id), -32602, "Unknown tool");
            };
            if !grant.scopes.iter().any(|scope| scope == tool.permission().as_str()) {
                return oauth::challenge(&settings, Some(tool.permission().as_str()));
            }
            let empty = json!({});
            let args = params.get("arguments").unwrap_or(&empty);
            let gateway = Gateway { state, catalog, providers, quote_seals };
            tool.call(id, &gateway, &settings, &grant, args).await
        }
        _ => error(Some(id), -32601, "Method not found"),
    }
}

#[derive(Clone, Copy)]
enum Tool {
    ListModels,
    GetModelSchema,
    Quote,
    Submit,
    ListRequests,
    GetRequest,
    GetResult,
    GetTrace,
    Cancel,
    Usage,
}

impl Tool {
    const ALL: [Self; 10] = [
        Self::ListModels, Self::GetModelSchema, Self::Quote, Self::Submit, Self::ListRequests,
        Self::GetRequest, Self::GetResult, Self::GetTrace, Self::Cancel, Self::Usage,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::ListModels => "list_models",
            Self::GetModelSchema => "get_model_schema",
            Self::Quote => "quote_generation",
            Self::Submit => "submit_generation",
            Self::ListRequests => "list_requests",
            Self::GetRequest => "get_request",
            Self::GetResult => "get_result",
            Self::GetTrace => "get_request_trace",
            Self::Cancel => "cancel_request",
            Self::Usage => "get_usage",
        }
    }

    fn permission(self) -> Permission {
        match self {
            Self::Submit => Permission::SubmitRequests,
            Self::Cancel => Permission::CancelRequests,
            Self::Usage => Permission::ReadUsage,
            _ => Permission::ReadRequests,
        }
    }

    fn descriptor(self) -> Value {
        let request_schema = || object(json!({"request_id": {"type": "string"}}), &["request_id"]);
        let (title, description, schema, fields) = match self {
            Self::ListModels => (
                "List generation models", "Discover connected image, video, audio, and 3D mesh models and their accepted settings.",
                object(json!({}), &[]), &["catalog_revision", "models"][..],
            ),
            Self::GetModelSchema => (
                "Get model capabilities", "Read the generation settings available for a connected TokenCraft model.",
                object(json!({"model": {"type": "string"}}), &["model"]), &["id", "media"][..],
            ),
            Self::Quote => (
                "Quote a generation", "Estimate an exact media intent within a USD nano ceiling. Makes no inference call. Show the estimate, adjustments and expiry before obtaining approval.",
                generation_schema(), &["quote_handle", "estimated_usd_nanos", "currency", "expires_at"][..],
            ),
            Self::Submit => (
                "Submit an approved generation", "Starts billable inference. Use only after the user approves. Send the same intent, quote handle and idempotency key on retries.",
                object(json!({
                    "intent": generation_schema(),
                    "quote_handle": {"type": "string"},
                    "app_ref": {"type": "string", "minLength": 1, "maxLength": 64},
                    "idempotency_key": {"type": "string", "minLength": 8, "maxLength": 128,
                        "description": "Create a unique key for the intent and preserve it for all retries."}
                }), &["intent", "quote_handle", "idempotency_key"]), &["request_id", "dashboard_url", "duplicate"][..],
            ),
            Self::ListRequests => (
                "List requests", "Read recent authorized requests. Use the returned next_after cursor for another page.",
                object(json!({"after": {"type": "string"}, "app_ref": {"type": "string", "minLength": 1, "maxLength": 64}}), &[]), &["requests", "next_after"][..],
            ),
            Self::GetRequest => (
                "Read request status", "Get the state, costs and when to poll again. Awaiting reconciliation means provider acceptance is uncertain.",
                request_schema(), &["request_id", "status", "dashboard_url"][..],
            ),
            Self::GetResult => (
                "Read a completed result", "Get a completed result and its expiry. The dashboard link opens media for an authorized TokenCraft user.",
                request_schema(), &["request_id", "result", "dashboard_url", "expires_at"][..],
            ),
            Self::GetTrace => (
                "Read request trace", "Inspect scoped routing, execution, costs, and webhook evidence for an existing request.",
                request_schema(), &["dashboard_url", "revision", "evidence"][..],
            ),
            Self::Cancel => (
                "Cancel a request", "Cancel a request when its current state permits. Check the request before and after cancellation.",
                request_schema(), &["request_id", "status"][..],
            ),
            Self::Usage => (
                "Read project usage", "Read authorized usage for a UTC date window with estimates, provider-reported usage and settlements.",
                object(json!({
                    "from": {"type":"string", "format":"date", "description":"Inclusive UTC date"},
                    "to": {"type":"string", "format":"date", "description":"Exclusive UTC date"},
                    "group_by": {"enum":["provider","model","client","app","day"]},
                    "app_ref": {"type":"string", "minLength":1, "maxLength":64},
                    "model": {"type":"string"}, "after": {"type":"string"}
                }), &[]), &["project_id", "totals", "groups", "currency"][..],
            ),
        };
        let scope = self.permission().as_str();
        let scopes = if self.permission() == Permission::ReadRequests { vec![scope] } else { vec!["requests:read", scope] };
        let security = json!([{"type": "oauth2", "scopes": scopes}]);
        let write = matches!(self, Self::Submit | Self::Cancel);
        json!({
            "name": self.name(), "title": title, "description": description,
            "inputSchema": schema, "outputSchema": {"type":"object", "required":fields},
            "annotations": {
                "readOnlyHint": !write, "destructiveHint": write,
                "idempotentHint": write, "openWorldHint": false
            },
            "securitySchemes": security,
            "_meta": {"securitySchemes": security}
        })
    }

    async fn call(
        self, id: ValueId, gateway: &Gateway, settings: &super::Settings, grant: &Grant, args: &Value,
    ) -> Response {
        macro_rules! call {
            ($operation:ident) => {{
                let Ok(args) = decode(args) else {
                    return error(Some(id), -32602, "Invalid tool arguments");
                };
                tool_result(id, gateway.$operation(grant, args).await)
            }};
        }
        match self {
            Self::ListModels => {
                if args.as_object().is_none_or(|args| !args.is_empty()) {
                    return error(Some(id), -32602, "Invalid tool arguments");
                }
                if let Err(error) = gateway.access(grant, Permission::ReadRequests).await {
                    return tool_result::<()>(id, Err(error));
                }
                tool_result(id, Ok(gateway.models(&gateway.catalog.current())))
            }
            Self::GetModelSchema => {
                let Ok(args) = decode(args) else {
                    return error(Some(id), -32602, "Invalid model arguments");
                };
                if let Err(error) = gateway.access(grant, Permission::ReadRequests).await {
                    return tool_result::<()>(id, Err(error));
                }
                tool_result(id, gateway.model(&gateway.catalog.current(), args))
            }
            Self::Quote => {
                let Ok(args) = decode(args) else { return error(Some(id), -32602, "Invalid generation arguments") };
                tool_result(id, gateway.quote(settings, grant, args).await)
            }
            Self::Submit => {
                let Ok(args) = decode(args) else { return error(Some(id), -32602, "Invalid submission arguments") };
                tool_result(id, gateway.submit(settings, grant, args).await)
            }
            Self::ListRequests => call!(list),
            Self::GetRequest => call!(status),
            Self::GetResult => call!(result),
            Self::GetTrace => call!(trace),
            Self::Cancel => call!(cancel),
            Self::Usage => call!(usage),
        }
    }
}

fn generation_schema() -> Value {
    let ratio = json!({"type":"string","pattern":"^[0-9]{1,3}:[0-9]{1,3}$"});
    let tolerance = json!({"type":"integer","minimum":0,"maximum":super::super::routing_policy::MAX_TOLERANCE_BPS});
    let image = object(json!({
        "kind": {"const":"image"}, "count": {"type":"integer","minimum":1,"maximum":8},
        "aspect_ratio": ratio, "max_relative_error_bps":tolerance
    }), &["kind", "count", "aspect_ratio"]);
    let video = object(json!({
        "kind": {"const":"video"}, "duration_seconds":{"type":"integer","minimum":1,"maximum":120},
        "aspect_ratio":ratio, "resolution":{"type":"string"}, "max_relative_error_bps":tolerance
    }), &["kind", "duration_seconds", "aspect_ratio", "resolution"]);
    let audio = object(json!({
        "kind": {"const":"audio"}, "duration_seconds":{"type":"integer","minimum":1,"maximum":120},
        "format":{"type":"string"}
    }), &["kind", "duration_seconds", "format"]);
    let mesh = object(json!({"kind":{"const":"mesh"}, "format":{"type":"string"}}), &["kind", "format"]);
    object(json!({
        "model": {"type":"string"}, "prompt":{"type":"string","minLength":1,"maxLength":4000},
        "max_total_usd_nanos":{"type":"integer","minimum":0,"description":"Maximum estimate in one-billionths of a US dollar. USD 0.05 is 50000000."},
        "preferred_providers":{"type":"array","uniqueItems":true,"items":{"type":"string","pattern":"^[a-z][a-z0-9_-]{0,63}$"},"description":"Provider priority. Use an empty array to rank by estimated cost."},
        "media":{"oneOf":[image,video,audio,mesh]}
    }), &["model", "prompt", "max_total_usd_nanos", "media"])
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
}

fn decode<T: DeserializeOwned>(args: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(args)
}

fn response(id: ValueId, result: impl Serialize) -> Response {
    Json(RpcResult { jsonrpc: "2.0", id, result }).into_response()
}

fn error(id: Option<ValueId>, code: i32, message: &'static str) -> Response {
    Json(RpcError { jsonrpc: "2.0", id, error: ErrorBody { code, message } }).into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResult<T> {
    content: [Text; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content: Option<T>,
    is_error: bool,
}

#[derive(Serialize)]
struct Text {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

fn tool_result<T: Serialize>(id: ValueId, outcome: ApiResult<T>) -> Response {
    let (value, text) = match outcome {
        Ok(value) => match serde_json::to_string(&value) {
            Ok(text) if text.len() <= 65_536 => (Some(value), text),
            _ => (None, "Result exceeds the connector response limit. Narrow the query or open TokenCraft.".into()),
        },
        Err(error) => {
            let text = match error {
                ApiError::BadRequest(text) | ApiError::Forbidden(text) | ApiError::NotFound(text) | ApiError::Conflict(text)
                | ApiError::ArchiveRetrievalRequired(text) | ApiError::NotImplemented(text)
                | ApiError::ServiceUnavailable(text) => text,
                ApiError::QuoteStale => crate::error::QUOTE_STALE_MESSAGE.into(),
                ApiError::BudgetExceeded => crate::error::MONTHLY_BUDGET_MESSAGE.into(),
                ApiError::ActiveRequestLimit => crate::error::ACTIVE_REQUEST_LIMIT_MESSAGE.into(),
                ApiError::SubmissionRateLimit(seconds) => format!(
                    "{}. Retry after {seconds}s with the same idempotency key.",
                    crate::error::SUBMISSION_RATE_MESSAGE,
                ),
                ApiError::ProviderCredentialUnavailable => crate::error::PROVIDER_CREDENTIAL_MESSAGE.into(),
                ApiError::ExportCapacity => crate::error::EXPORT_CAPACITY_MESSAGE.into(),
                ApiError::Unauthorized(_) => "Reconnect TokenCraft.".into(),
                ApiError::Internal(_) => "TokenCraft could not complete this operation.".into(),
            };
            (None, text)
        }
    };
    response(id, ToolResult {
        content: [Text { kind: "text", text }],
        is_error: value.is_none(),
        structured_content: value,
    })
}
