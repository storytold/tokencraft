use std::collections::HashSet;

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

use super::{
    access::{self, GatewayCaller, Permission, ScopeQuery},
    load, parse_request_id, Snapshot, Timing, Tone,
};
use crate::{
    error::{ApiError, ApiResult},
    middleware::trace_context::{evidence_root_span_id, stable_id, valid_trace_id},
    state::AppState,
};

#[utoipa::path(
    get, path = "/v1/requests/{id}/trace/otlp", operation_id = "downloadGatewayRequestTrace", tag = "native",
    summary = "Download redacted request timing as OpenTelemetry JSON",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Durable request ID."), ScopeQuery),
    responses(
        (status = 200, description = "OTLP/HTTP JSON ExportTraceServiceRequest attachment.", body = Object),
        (status = 404, description = "Request is outside scope or unavailable.", body = crate::error::GatewayError),
        (status = "default", response = super::super::RequestErrorResponse),
    ),
)]
pub(in crate::gateway_demo) async fn download(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let id = parse_request_id(&raw)?;
    let filename = format!("attachment; filename=\"tokencraft-{id}-otel.json\"");
    let snapshot = load(&state.pool, access.scope(), id, false).await?;
    let payload = encode(&snapshot)?;
    Ok((
        [
            (header::CONTENT_DISPOSITION, filename),
            (header::CACHE_CONTROL, "no-store".into()),
        ],
        Json(payload),
    ))
}

fn valid_span_id(id: &str) -> bool {
    id.len() == 16
        && id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && id.bytes().any(|byte| byte != b'0')
}

fn encode(snapshot: &Snapshot) -> ApiResult<Value> {
    let evidence = &snapshot.evidence;
    let trace_id = snapshot
        .correlation
        .as_ref()
        .map(|context| context.trace_id.as_str())
        .filter(|id| valid_trace_id(id))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            stable_id(
                &format!("tokencraft.otlp.v1.legacy:{}", evidence.request_id),
                32,
            )
        });
    let root_id = evidence_root_span_id(&trace_id);
    let step_id = |id: &str| stable_id(&format!("tokencraft.otlp.v1.step:{trace_id}:{id}"), 16);
    let observed: Vec<_> = evidence
        .steps
        .iter()
        .filter_map(|step| {
            let start = step.started_at.timestamp_nanos_opt()?;
            let end = match step.kind {
                Timing::Point => start,
                Timing::Interval => step.ended_at?.timestamp_nanos_opt()?,
            };
            (start > 0 && end >= start).then_some((step, start, end))
        })
        .collect();
    let start = observed
        .iter()
        .map(|(_, start, _)| *start)
        .min()
        .ok_or_else(|| ApiError::Conflict("request has no recorded timestamps to export".into()))?;
    let end = observed
        .iter()
        .map(|(_, _, end)| *end)
        .max()
        .unwrap_or(start);
    let included: HashSet<_> = observed
        .iter()
        .map(|&(step, _, _)| step.id.as_str())
        .collect();
    let caller = snapshot.correlation.as_ref().and_then(|context| {
        let trace = context
            .caller_trace_id
            .as_deref()
            .filter(|id| valid_trace_id(id))?;
        let span = context
            .caller_span_id
            .as_deref()
            .filter(|id| valid_span_id(id))?;
        Some(json!({"traceId": trace, "spanId": span}))
    });
    let mut spans = vec![json!({
        "traceId": trace_id, "spanId": root_id, "name": "TokenCraft request evidence", "kind": 1,
        "startTimeUnixNano": start.to_string(), "endTimeUnixNano": end.to_string(),
        "status": {"code": if evidence.status == "failed" { 2 } else { 0 }},
        "links": caller.into_iter().collect::<Vec<_>>(),
        "attributes": [
            {"key": "tokencraft.request.id", "value": {"stringValue": evidence.request_id.as_str()}},
            {"key": "tokencraft.request.status", "value": {"stringValue": evidence.status}},
            {"key": "tokencraft.evidence.revision", "value": {"stringValue": snapshot.revision}},
            {"key": "tokencraft.evidence.omitted_steps",
             "value": {"intValue": (evidence.steps.len() - observed.len()).to_string()}},
            {"key": "tokencraft.evidence.omitted_webhook_attempts",
             "value": {"intValue": evidence.delivery_attempts_omitted.to_string()}},
        ],
    })];

    for (step, start, end) in observed {
        // The waterfall's parents are causal predecessors. Link to those spans
        // without nesting a later interval inside an earlier, closed span.
        let predecessor = step
            .parent_id
            .as_deref()
            .filter(|id| included.contains(id))
            .map(|id| json!({"traceId": trace_id, "spanId": step_id(id)}));
        let events: Vec<_> = step
            .logs
            .iter()
            .filter_map(|log| {
                let at = log.at.timestamp_nanos_opt()?;
                (start..=end)
                    .contains(&at)
                    .then(|| json!({"name": log.code, "timeUnixNano": at.to_string()}))
            })
            .collect();
        // Export server-generated timing labels and event codes only. Detailed
        // waterfall attributes can identify provider accounts or upstream jobs.
        spans.push(json!({
            "traceId": trace_id, "spanId": step_id(&step.id), "parentSpanId": root_id,
            "name": step.name, "kind": 1, "startTimeUnixNano": start.to_string(),
            "endTimeUnixNano": end.to_string(), "status": {"code": if matches!(step.tone, Tone::Error) { 2 } else { 0 }},
            "links": predecessor.into_iter().collect::<Vec<_>>(), "events": events,
            "attributes": [{"key": "tokencraft.component", "value": {"stringValue": step.component}}],
        }));
    }

    Ok(json!({"resourceSpans": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "tokencraft"}}]},
        "scopeSpans": [{"scope": {"name": "tokencraft.request.evidence", "version": "1"}, "spans": spans}],
    }]}))
}
