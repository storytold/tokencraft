use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Utc};
use db_queries::gateway_requests::{self, RequestScope};
use db_queries::gateway_webhooks::{self, Delivery, DeliveryAttempt};
use ids::GatewayRequestId;
use serde::Serialize;

use super::access::{self, GatewayCaller, Permission, ScopeQuery};
use super::catalog::{digest, Adapter};
use super::handlers::parse_request_id;
use super::quote::{Quote, Reason, Workload};
use super::requests::{self, EventKind, RequestStatus, RequestView, TimelineEvent};
use super::routing_policy::{Setting, Source};
use super::upstream_labels::{model_label, provider_label};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(super) mod baseline;
pub(super) mod otlp;
mod compute;
mod provider_path;
mod summary;
use provider_path::ProviderPath;

#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = RequestTraceSnapshot)]
pub(super) struct Snapshot {
    pub revision: String,
    pub observed_at: DateTime<Utc>,
    pub analysis_available: bool,
    pub evidence: Evidence,
    /// Gateway attempts and the downstream provider reported by the upstream, when available.
    provider_path: ProviderPath,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation: Option<requests::TraceCorrelation>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = RequestTraceEvidence)]
pub(super) struct Evidence {
    schema_version: u8,
    #[schema(value_type = String)]
    request_id: GatewayRequestId,
    #[schema(schema_with = requests::request_status_schema)]
    status: &'static str,
    subscription_configured: bool,
    delivery_status: Option<String>,
    delivery_attempts_omitted: usize,
    pub steps: Vec<Step>,
    coverage: Vec<&'static str>,
    summary: summary::Summary,
    #[serde(skip_serializing_if = "Option::is_none")]
    routing: Option<provider_path::DiagnosticPath>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = RequestTraceStep)]
pub(super) struct Step {
    pub id: String,
    pub parent_id: Option<String>,
    name: String,
    component: &'static str,
    kind: Timing,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    status: String,
    tone: Tone,
    logs: Vec<Log>,
    attributes: Vec<Attribute>,
}

#[derive(Serialize, Clone, Copy, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = RequestTraceTiming)]
enum Timing {
    Point,
    Interval,
}

#[derive(Serialize, Clone, Copy, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = RequestTraceTone)]
enum Tone {
    Ok,
    Active,
    Warning,
    Error,
    Neutral,
}

#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = RequestTraceLog)]
struct Log {
    at: DateTime<Utc>,
    code: &'static str,
    message: &'static str,
    severity: Tone,
}

#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = RequestTraceAttribute)]
struct Attribute {
    label: String,
    value: String,
}

impl Step {
    fn point(id: impl Into<String>, name: impl Into<String>, component: &'static str) -> Self {
        Self {
            id: id.into(),
            parent_id: None,
            name: name.into(),
            component,
            kind: Timing::Point,
            started_at: DateTime::<Utc>::UNIX_EPOCH,
            ended_at: None,
            status: String::new(),
            tone: Tone::Neutral,
            logs: vec![],
            attributes: vec![],
        }
    }

    fn at(mut self, time: DateTime<Utc>) -> Self {
        self.started_at = time;
        self
    }

    fn until(mut self, end: Option<DateTime<Utc>>) -> Self {
        self.kind = Timing::Interval;
        self.ended_at = end;
        self
    }

    fn after(mut self, parent: impl Into<String>) -> Self {
        self.parent_id = Some(parent.into());
        self
    }

    fn status(mut self, status: impl Into<String>, tone: Tone) -> Self {
        self.status = status.into();
        self.tone = tone;
        self
    }

    fn log(&mut self, log: Log) {
        self.logs.push(log);
    }

    fn field(&mut self, label: impl Into<String>, value: impl ToString) {
        self.attributes.push(Attribute { label: label.into(), value: value.to_string() });
    }

    fn optional(&mut self, label: &'static str, value: Option<impl ToString>) {
        if let Some(value) = value {
            self.field(label, value);
        }
    }
}

impl Log {
    fn from_event(event: &TimelineEvent) -> Self {
        let (code, message, severity) = match event.kind {
            EventKind::Accepted => ("request.accepted", "Gateway persisted the request.", Tone::Ok),
            EventKind::MockProviderSubmitted => ("mock_provider.accepted", "The mock worker recorded provider acceptance.", Tone::Ok),
            EventKind::MockProviderCompleted => ("mock_provider.completed", "The mock worker recorded completion and usage.", Tone::Ok),
            EventKind::Canceled => ("request.canceled", "Gateway recorded the request cancellation.", Tone::Neutral),
            EventKind::ProviderSubmitStarted => ("provider.submit_started", "Worker recorded the start of provider dispatch.", Tone::Active),
            EventKind::ProviderCompleted => ("provider.completed", "Worker recorded provider completion.", Tone::Ok),
            EventKind::ProviderFailed => ("provider.failed", "Worker recorded a terminal failure.", Tone::Error),
            EventKind::ProviderOutcomeUnconfirmed => ("provider.outcome_unconfirmed", "The worker could not establish the provider outcome.", Tone::Warning),
            EventKind::ProviderReconciled => ("provider.reconciled", "The gateway recovered the provider job ID.", Tone::Ok),
        };
        Self { at: event.at, code, message, severity }
    }
}

#[utoipa::path(
    get, path = "/v1/requests/{id}/trace", operation_id = "getGatewayRequestTrace", tag = "native",
    security(("project_api_key" = []), ("session_cookie" = []), ("api_key" = [])),
    params(("id" = String, Path, description = "Durable request ID."), ScopeQuery),
    responses(
        (status = 200, description = "Scoped timing, routing, and cost evidence.", body = Snapshot),
        (status = 404, description = "Request is outside scope or unavailable.", body = crate::error::GatewayError),
        (status = "default", response = super::RequestErrorResponse),
    ),
)]
pub(super) async fn get(
    State(state): State<AppState>,
    caller: GatewayCaller,
    Query(scope): Query<ScopeQuery>,
    Path(raw): Path<String>,
) -> ApiResult<Json<Snapshot>> {
    let access = access::resolve(&state, caller, scope, Permission::ReadRequests).await?;
    let id = parse_request_id(&raw)?;
    Ok(Json(load(&state.pool, access.scope(), id, false).await?))
}

pub(super) async fn load(
    pool: &db::PgPool,
    scope: RequestScope<'_>,
    id: GatewayRequestId,
    analysis_available: bool,
) -> ApiResult<Snapshot> {
    let mut tx = pool.begin().await?;
    gateway_requests::read_snapshot(&mut *tx).await?;
    let request = requests::get(&mut *tx, scope, &id).await?;
    let compute_attempts = if request.upstream.as_ref().is_some_and(|upstream| {
        upstream.adapter == Adapter::OpenAiCompatibleChat.as_str()
    }) {
        gateway_requests::compute_attempts::read_compute_attempts(&mut *tx, scope, &id)
            .await?
            .map(serde_json::from_value::<Vec<super::compute_evidence::ComputeAttempt>>)
            .transpose()
            .map_err(|_| ApiError::Internal("invalid stored compute evidence".into()))?
            .unwrap_or_default()
    } else {
        vec![]
    };
    let retries = gateway_requests::read_route_retries(&mut *tx, &id).await?
        .map(serde_json::from_value::<Vec<RouteRetry>>).transpose()
        .map_err(|_| ApiError::Internal("invalid stored route retry".into()))?
        .unwrap_or_default();
    let delivery = if request.webhook_subscription_id.is_some() {
        gateway_webhooks::find_request_delivery(&mut *tx, scope, &id).await?
    } else {
        None
    };
    let attempts = if let Some(delivery) = &delivery {
        let delivery_id = delivery.id.parse()
            .map_err(|_| ApiError::Internal("invalid stored webhook delivery ID".into()))?;
        gateway_webhooks::attempts(&mut *tx, scope, &delivery_id).await?
    } else {
        vec![]
    };
    let observed_at = Utc::now();
    tx.commit().await?;

    let mut evidence = assemble(id, &request, delivery.as_ref(), attempts);
    let compute = compute::add_steps(&mut evidence.steps, compute_attempts, request.status);
    if !compute.is_empty() {
        evidence.coverage.push(
            "Compute locations come from operator configuration at dispatch. A response starting does not establish inference completion.",
        );
    }
    let provider_path = ProviderPath::from_request(&request, &retries, compute);
    evidence.routing = provider_path.diagnostics();
    for (index, retry) in retries.into_iter().enumerate() {
        let mut step = Step::point(&format!("route-retry-{}", index + 1), "Availability retry", "dispatch worker")
            .at(retry.at).after("routing").status("Rate limited before acceptance", Tone::Warning);
        step.field("Offer", retry.decision.offer_id);
        step.field("Next offer", retry.decision.next_offer_id);
        step.field("Next estimate (nano-USD)", retry.decision.estimated_usd_nanos);
        step.field("Minimum retry wait (ms)", retry.decision.delay_ms);
        let position = evidence.steps.iter().position(|step| step.id == "provider").unwrap_or(evidence.steps.len());
        evidence.steps.insert(position, step);
    }
    let encoded = serde_json::to_vec(&(&evidence, &provider_path))
        .map_err(|_| ApiError::Internal("could not encode request trace".into()))?;
    Ok(Snapshot {
        revision: digest(encoded), observed_at, analysis_available, evidence, provider_path,
        correlation: request.correlation,
    })
}

#[derive(serde::Deserialize)]
struct RouteRetry {
    at: DateTime<Utc>,
    decision: RouteRetryDecision,
}

#[derive(serde::Deserialize)]
struct RouteRetryDecision {
    offer_id: String,
    next_offer_id: String,
    estimated_usd_nanos: i64,
    delay_ms: i32,
}

fn assemble(
    id: GatewayRequestId,
    request: &RequestView,
    delivery: Option<&Delivery>,
    mut attempts: Vec<DeliveryAttempt>,
) -> Evidence {
    let accepted = &request.events[0];
    let dispatch = request.events.iter().find(|event| matches!(
        event.kind, EventKind::MockProviderSubmitted | EventKind::ProviderSubmitStarted
    ));
    let terminal = request.events.iter().rfind(|event| matches!(
        event.kind, EventKind::MockProviderCompleted | EventKind::Canceled |
            EventKind::ProviderCompleted | EventKind::ProviderFailed
    ) || (matches!(event.kind, EventKind::ProviderOutcomeUnconfirmed)
        && request.status == RequestStatus::AwaitingReconciliation));
    let (status, outcome_label, tone) = match request.status {
        RequestStatus::Queued => ("queued", "Queued", Tone::Active),
        RequestStatus::Running => ("running", "Running", Tone::Active),
        RequestStatus::Succeeded => ("succeeded", "Succeeded", Tone::Ok),
        RequestStatus::Canceled => ("canceled", "Canceled", Tone::Neutral),
        RequestStatus::Failed => ("failed", "Failed", Tone::Error),
        RequestStatus::AwaitingReconciliation => ("awaiting_reconciliation", "Outcome unconfirmed", Tone::Warning),
    };
    let mut admission = Step::point("admission", "Request accepted", "gateway").at(accepted.at)
        .status("Recorded", Tone::Ok);
    admission.log(Log::from_event(accepted));
    let route = route_step(&request.quote, accepted.at);

    let queue_boundary = dispatch.or(terminal);
    let mut queue = Step::point("queue", "Queue wait", "dispatch worker").at(accepted.at)
        .until(queue_boundary.map(|event| event.at)).after("admission")
        .status(if dispatch.is_some() { "Dispatched" } else { outcome_label }, if dispatch.is_some() { Tone::Ok } else { tone });
    if let Some(event) = queue_boundary {
        queue.log(Log::from_event(event));
    }
    let mut steps = vec![admission, route, queue];

    let result_observed_at = request.upstream.as_ref().and_then(|upstream| upstream.result_observed_at);
    let publication_end = terminal.filter(|end| result_observed_at.is_some_and(|at| end.at >= at));

    if let Some(event) = dispatch {
        let completed = result_observed_at.filter(|at| *at >= event.at);
        let (label, provider_tone) = if completed.is_some() { ("Completion observed", Tone::Ok) }
            else { (outcome_label, tone) };
        let mut provider = Step::point("provider", "Provider window", "provider adapter").at(event.at)
            .until(completed.or_else(|| terminal.map(|end| end.at))).after("queue")
            .status(label, provider_tone);
        provider.log(Log::from_event(event));
        add_provider_fields(&mut provider, request);
        steps.push(provider);
    }
    if let Some(upstream) = &request.upstream {
        if let Some(at) = upstream.callback_received_at {
            let recovered = upstream.callback_recovered_from;
            let mut callback = Step::point("provider-callback", "Provider callback", "gateway")
                .at(at).after("provider")
                .status(if recovered.is_some() && upstream.callback_reconciled_at.is_none() {
                    "Job ID recovered"
                } else {
                    "Verified"
                }, Tone::Ok);
            callback.optional("First unconfirmed (UTC)", recovered.map(|time| time.to_rfc3339()));
            steps.push(callback);
        }
        if let Some(at) = upstream.callback_reconciled_at {
            let after = if upstream.callback_received_at.is_some_and(|callback| callback <= at) {
                "provider-callback"
            } else {
                "provider"
            };
            let mut recovery = Step::point("provider-reconciled", "Provider job recovered", "gateway")
                .at(at).after(after).status("Job ID verified", Tone::Ok);
            recovery.optional("First unconfirmed (UTC)", upstream.callback_recovered_from.map(|time| time.to_rfc3339()));
            steps.push(recovery);
        }
    }
    use super::requests::ProviderRecheck;
    let observed_recheck = match request.recheck.as_ref() {
        Some(ProviderRecheck::Pending { requested_at, .. }) => Some((*requested_at, true)),
        Some(ProviderRecheck::Finished { requested_at }) => Some((*requested_at, false)),
        _ => None,
    };
    if let Some((at, pending)) = observed_recheck {
        let mut recheck = Step::point("provider-recheck", "Provider recheck", "provider adapter")
            .at(at).after("provider")
            .status(if pending { "Scheduled or checking" } else { "Recheck ended" },
                    if pending { Tone::Active } else { tone });
        recheck.log(Log {
            at,
            code: "provider.recheck_requested",
            message: "An administrator scheduled a status and result check for the original job.",
            severity: Tone::Neutral,
        });
        steps.push(recheck);
    }
    if let Some(at) = result_observed_at {
        let label = match request.status {
            RequestStatus::Succeeded => "Published",
            RequestStatus::AwaitingReconciliation => "Publication unconfirmed",
            RequestStatus::Failed | RequestStatus::Canceled => "Publication ended",
            _ => "Fetching and verifying",
        };
        let mut publication = Step::point("artifact-publication", "Artifact publication", "artifact worker")
            .at(at).until(publication_end.map(|event| event.at)).after("provider").status(label, tone);
        publication.log(Log {
            at,
            code: "provider.completion_observed",
            message: "A provider status read confirmed completion. Artifact publication includes retrieval and verification.",
            severity: Tone::Ok,
        });
        if let Some(end) = publication_end {
            publication.log(Log::from_event(end));
        }
        steps.push(publication);
    }
    if let Some(event) = terminal {
        let parent = if publication_end.is_some() { "artifact-publication" }
            else if dispatch.is_some() { "provider" } else { "queue" };
        let mut outcome = Step::point("outcome", "Request outcome", "gateway").at(event.at)
            .after(parent).status(outcome_label, tone);
        outcome.log(Log::from_event(event));
        if dispatch.is_none() {
            add_provider_fields(&mut outcome, request);
        }
        steps.push(outcome);
    }

    let mut coverage = vec![
        "Step logs summarize durable lifecycle and delivery records.",
        "Routing duration and provider-internal timings are unavailable.",
        "Provider response content, client references, credentials, payloads and destination URLs are excluded.",
        "Provider-reported cost and administrator-recorded supplier settlements are recorded separately.",
    ];
    if request.status == RequestStatus::AwaitingReconciliation {
        coverage.push(if request.upstream.as_ref().is_some_and(|upstream| upstream.accepted_at.is_some()) {
            "The provider acknowledged the job. Its final outcome is unconfirmed."
        } else {
            "Provider acceptance is unconfirmed. An additional dispatch may duplicate work."
        });
    }
    let latest_attempt = attempts.first().map(|attempt| (attempt.number, attempt.http_status));
    let attempt_count = delivery.map_or(0, |event| event.attempt_count.max(0) as usize);
    let omitted = attempt_count.saturating_sub(attempts.len());
    if let Some(delivery) = delivery {
        attempts.reverse(); // The query returns the most recent attempts first.
        add_delivery_steps(&mut steps, delivery, attempts);
        if omitted > 0 {
            coverage.push("Older webhook attempts were omitted from this snapshot.");
        }
    } else if request.webhook_subscription_id.is_some() {
        coverage.push("A webhook subscription was configured. There is no delivery record for this snapshot.");
    }
    if let Some(at) = request.content_removed_at {
        steps.push(
            Step::point("content-removal", "Stored content removed", "gateway")
                .at(at).after("outcome").status("Removed", Tone::Neutral),
        );
    }
    let summary = summary::summarize(request, delivery, latest_attempt, &steps);
    Evidence {
        schema_version: 1,
        request_id: id,
        status,
        subscription_configured: request.webhook_subscription_id.is_some(),
        delivery_status: delivery.map(|event| event.state.clone()),
        delivery_attempts_omitted: omitted,
        steps,
        coverage,
        summary,
        routing: None,
    }
}

fn route_step(quote: &Quote, at: DateTime<Utc>) -> Step {
    let mut step = Step::point("routing", "Route recorded", "routing policy").at(at)
        .after("admission").status("Quote snapshot", Tone::Neutral);
    step.log(Log {
        at, code: "route.snapshot", message: "The selected quote was stored with the request.", severity: Tone::Neutral,
    });
    step.field("Quote ID", &quote.id);
    step.field("Catalog revision", &quote.catalog_revision);
    step.field("Requested model", &quote.input.model);
    if quote.input.is_auto() {
        step.optional(
            "Selected model",
            quote.selected_candidate().and_then(|candidate| candidate.model.as_deref()),
        );
    }
    step.optional("Pinned offer", quote.pinned_offer_id.as_deref());
    step.field("Organization policy revision", quote.routing.as_ref().map_or(0, |routing| routing.organization_revision));
    step.field("Project policy revision", quote.routing.as_ref().map_or(0, |routing| routing.project_revision));
    if let Some(routing) = &quote.routing {
        if let Some(preference) = &routing.provider_preference {
            let order = preference.value.iter().map(|provider| provider.as_str()).collect::<Vec<_>>().join(", ");
            step.field("Route preference", if order.is_empty() { "Lowest estimated cost".into() } else { order });
            step.field("Preference source", preference.source.label());
        }
        if let Some(minimum) = routing.min_quality {
            step.field("Minimum community rating", minimum.value);
            step.field("Community rating source", minimum.source.label());
        }
        if let Some(minimum) = routing.min_usage_accounts {
            step.field("Minimum organizations using the model", minimum.value);
            step.field("Model usage threshold source", minimum.source.label());
        }
        if let Some(limits) = &routing.provider_limits {
            let layers = match (limits.organization, limits.project) {
                (true, true) => "Organization and project",
                (true, false) => "Organization",
                _ => "Project",
            };
            step.field("Provider limits", layers);
        }
    }
    if let Workload::Image { count, aspect_ratio, max_relative_error_bps } = &quote.input.workload {
        step.field("Requested images", count);
        step.field("Requested aspect ratio", aspect_ratio);
        let tolerance = quote.routing.as_ref().and_then(|routing| routing.aspect_ratio_tolerance)
            .or_else(|| max_relative_error_bps.map(|value| Setting { value, source: Source::Request }));
        if let Some(setting) = tolerance {
            step.field("Image ratio tolerance (bps)", setting.value);
            step.field("Image ratio tolerance source", setting.source.label());
        }
    }
    if let Workload::Video { duration_seconds, aspect_ratio, resolution, max_relative_error_bps } =
        &quote.input.workload
    {
        step.field("Requested duration (seconds)", duration_seconds);
        step.field("Requested aspect ratio", aspect_ratio);
        step.field("Requested resolution", resolution);
        let tolerance = quote.routing.as_ref()
            .and_then(|routing| routing.aspect_ratio_tolerance)
            .or_else(|| max_relative_error_bps.map(|value| Setting { value, source: Source::Request }));
        if let Some(setting) = tolerance {
            step.field("Video ratio tolerance (bps)", setting.value);
            step.field("Video ratio tolerance source", setting.source.label());
        }
    }
    if let Workload::Audio { duration_seconds, format } = &quote.input.workload {
        step.field("Requested duration (seconds)", duration_seconds);
        step.field("Requested audio format", format);
    }
    if let Workload::Mesh { format } = &quote.input.workload {
        step.field("Requested mesh format", format);
    }
    step.optional("Cost cap (nano-USD)", quote.input.max_total_usd_nanos);
    step.optional("Temperature", quote.chat_options.temperature);
    step.optional("Top p", quote.chat_options.top_p);
    if let Some(window) = &quote.performance_window {
        let ranking = match quote.routing.as_ref().and_then(|routing| routing.route_ranking.as_ref()) {
            Some(setting) if window.applied && setting.value == super::routing_policy::RouteRanking::CostPerCompletion =>
                "Settled cost per completion",
            _ if window.applied => "Recent queue reliability",
            _ => "Estimated cost",
        };
        step.field("Ranking", ranking);
    }
    for candidate in &quote.candidates {
        let selected = quote.selected_offer_id.as_ref() == Some(&candidate.offer_id);
        let reason = match candidate.reason {
            Reason::Eligible if selected => "Selected",
            Reason::Eligible => "Eligible",
            Reason::ProviderExcluded => "Provider excluded",
            Reason::ProviderPolicyExcluded => "Blocked by routing policy",
            Reason::ProviderRegionUnmet => "Processing region requirement unmet",
            Reason::ProviderRetentionUnmet => "Upstream retention requirement unmet",
            Reason::WorkloadUnsupported => "Workload unsupported",
            Reason::AspectRatioUnsupported => "Aspect ratio unsupported",
            Reason::AboveCostCap => "Above cost cap",
            Reason::PriceStale => "Price expired",
            Reason::QueueCoolingDown => "Provider queue cooling down",
            Reason::ProviderCredentialUnavailable => "Provider credential unavailable",
            Reason::InsufficientModelRatings => "Fewer than ten community ratings",
            Reason::BelowModelQuality => "Below the community quality threshold",
            Reason::BelowModelUsage => "Below the community usage threshold",
        };
        let label = format!("Offer {} ({})", candidate.offer_id, candidate.provider.as_str());
        let mut decision = reason.to_owned();
        if let Some(rank) = candidate.preference_rank {
            decision.push_str(&format!(" · preference {rank}"));
        }
        if let Some(amount) = candidate.estimated_usd_nanos {
            decision.push_str(&format!(" · {amount} nano-USD estimate"));
        }
        if let Some(ratio) = &candidate.applied_aspect_ratio {
            decision.push_str(&format!(" · {ratio} aspect ratio"));
        }
        if let Some(limit) = &candidate.offer_tolerance {
            decision.push_str(&format!(" · {} bps limit ({})", limit.value, limit.source.label()));
        }
        if let Some(rate) = &candidate.rate_evidence {
            decision.push_str(&format!(" · {} · observed {} · valid through {}",
                rate.source_url, rate.observed_at.to_rfc3339(), rate.valid_until.to_rfc3339()));
        }
        if let Some(reference) = &candidate.artcraft_reference {
            decision.push_str(&format!(" · ArtCraft preset {} ({})", reference.offer_id, reference.revision));
        }
        if let Some(samples) = &candidate.performance {
            decision.push_str(&format!(" · {} of {} acknowledged runs completed", samples.succeeded, samples.samples));
            if let Some(median) = samples.median_ms {
                decision.push_str(&format!(" · {median} ms median completion"));
            }
            if let Some(cost) = &samples.cost {
                decision.push_str(&format!(" · {} of {} direct runs settled", cost.settled, samples.samples));
                if let Some(total) = &cost.settled_usd_nanos {
                    decision.push_str(&format!(" · {total} nano-USD settled across {} confirmed completions", samples.succeeded));
                }
            }
        }
        step.field(label, decision);
    }
    step
}

fn add_provider_fields(step: &mut Step, request: &RequestView) {
    if let Some(settlement) = &request.supplier_settlement {
        step.field("Supplier settlement (nano-USD)", &settlement.amount_usd_nanos);
        step.field("Settlement revision", settlement.revision);
    }
    if let Some(upstream) = &request.upstream {
        step.field("Adapter", &upstream.adapter);
        step.optional("Upstream model sent", model_label(upstream.requested_model.as_deref()));
        step.optional("Upstream model reported", model_label(upstream.reported_model.as_deref()));
        step.optional("Provider name in response", provider_label(upstream.reported_provider.as_deref()));
        step.optional("Credential scope", upstream.credential_scope);
        step.optional("Credential binding", upstream.credential_binding.as_deref());
        let version_label = match upstream.credential_scope {
            Some("organization") => "Organization credential version",
            _ => "Project credential version",
        };
        step.optional(version_label, upstream.credential_version);
        step.optional("First streamed text (UTC)", upstream.stream_started_at.map(|time| time.to_rfc3339()));
        step.optional("First stream disconnection (UTC)", upstream.stream_detached_at.map(|time| time.to_rfc3339()));
        step.optional("Normalized error", upstream.error_code.as_deref());
        step.optional("Input tokens", upstream.input_tokens);
        step.optional("Output tokens", upstream.output_tokens);
        step.optional("Provider-reported cost (nano-USD)", upstream.reported_usd_nanos.as_deref());
        if let Some(charge) = &upstream.provider_charge {
            step.field("Provider-reported charge", format!("{} {}", charge.quantity, charge.unit));
            step.field("Charge observed (UTC)", charge.observed_at.to_rfc3339());
        }
        step.optional("Result expiry (UTC)", upstream.result_expires_at.map(|time| time.to_rfc3339()));
    } else {
        step.field("Adapter", "mock");
        step.optional("Mock-reported cost (nano-USD)", request.mock_reported_usd_nanos);
    }
}

fn add_delivery_steps(steps: &mut Vec<Step>, delivery: &Delivery, attempts: Vec<DeliveryAttempt>) {
    let mut event = Step::point("webhook", "Webhook event enqueued", "webhook worker")
        .at(delivery.occurred_at).after("outcome").status("Event recorded", Tone::Neutral);
    event.log(Log {
        at: delivery.occurred_at, code: "webhook.enqueued",
        message: "The terminal request transaction recorded the outbound event.", severity: Tone::Neutral,
    });
    event.field("Event ID", &delivery.id);
    event.field("Event type", &delivery.event_type);
    event.field("Delivery state", &delivery.state);
    event.field("Recorded attempt count", delivery.attempt_count);
    if delivery.state == "pending" {
        if let Some(at) = delivery.next_attempt_at {
            event.field("Eligible to send after (UTC)", at.to_rfc3339());
        } else {
            event.field("Retry schedule", "No further attempt scheduled");
        }
    }
    event.optional("Last delivery error", delivery.last_error.as_deref());
    steps.push(event);

    let mut parent = String::from("webhook");
    let mut prior_end = None;
    for attempt in attempts {
        if let Some(end) = prior_end.filter(|time| *time < attempt.started_at) {
            let wait_id = format!("webhook-gap-{}", attempt.number);
            steps.push(Step::point(&wait_id, "Time between attempts", "webhook worker")
                .at(end).until(Some(attempt.started_at)).after(&parent).status("Elapsed wait", Tone::Neutral));
            parent = wait_id;
        }
        let id = format!("webhook-attempt-{}", attempt.number);
        let (status, finished_code, finished_message, tone) = match attempt.outcome.as_deref() {
            Some("delivered") => ("Delivered", "webhook.delivered", "The receiver acknowledged the event.", Tone::Ok),
            Some("http_error") => ("HTTP error", "webhook.http_error", "The receiver returned an unsuccessful HTTP response.", Tone::Error),
            Some("network_error") => ("Network error", "webhook.network_error", "The delivery transport returned an error.", Tone::Error),
            Some("lease_expired") => ("Lease expired", "webhook.lease_expired", "The attempt lease expired before acknowledgment was recorded.", Tone::Warning),
            Some("destination_unavailable") => ("Destination unavailable", "webhook.destination_unavailable", "The registered destination was unavailable.", Tone::Error),
            _ => ("Sending", "webhook.finished", "The attempt finished.", Tone::Active),
        };
        let label = attempt.http_status.map_or_else(|| status.to_owned(), |code| format!("{status} · HTTP {code}"));
        let mut step = Step::point(&id, format!("Webhook attempt {}", attempt.number), "webhook worker")
            .at(attempt.started_at).until(attempt.finished_at).after(&parent).status(label, tone);
        step.log(Log { at: attempt.started_at, code: "webhook.attempt_started",
            message: "The delivery worker recorded the start of this attempt.", severity: Tone::Active });
        if let Some(at) = attempt.finished_at {
            step.log(Log { at, code: finished_code, message: finished_message, severity: tone });
        }
        step.field("Event ID", &delivery.id);
        step.field("Attempt", attempt.number);
        step.optional("HTTP status", attempt.http_status);
        steps.push(step);
        parent = id;
        prior_end = attempt.finished_at;
    }
    if let Some(at) = delivery.finished_at {
        let (status, tone, code, message) = if delivery.state == "delivered" {
            ("Delivered", Tone::Ok, "webhook.delivery_completed", "The delivery was marked complete.")
        } else {
            ("Delivery stopped", Tone::Error, "webhook.delivery_stopped", "The delivery was marked terminal without acknowledgment.")
        };
        let mut step = Step::point("webhook-outcome", "Webhook outcome", "webhook worker")
            .at(at).after(parent).status(status, tone);
        step.log(Log { at, code, message, severity: tone });
        steps.push(step);
    }
}
