use std::{
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url, header};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::chat_input::{ChatInput, ChatOptions, ChatResult, Message};
use super::super::upstream_labels::provider_label;
use super::local_chat::compute::ComputeStack;

mod stream;
mod compute_evidence;
use compute_evidence::ComputeTrace;
use super::super::compute_evidence::ComputeOutcome;
pub(super) use stream::StreamSink;
pub(in crate::gateway_demo) use stream::purpose as stream_chunk_purpose;

const MAX_BODY: usize = 512 * 1024;
const MAX_RESULT: usize = 32 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);
const FAILOVER_WINDOW: Duration = Duration::from_secs(9);
const LEASE_MARGIN: Duration = Duration::from_secs(2);
const UNHEALTHY_COOLDOWN: Duration = Duration::from_secs(5);

pub struct Completion {
    pub result: ChatResult,
    pub handle: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    pub cost_usd_nanos: Option<i64>,
}

pub enum SubmitOutcome {
    Completed(Completion),
    Interrupted(Completion),
    Rejected(&'static str),
    Uncertain(&'static str),
}

pub struct ChatClient {
    destination: Destination,
    client: Client,
}

enum Destination {
    Direct {
        endpoint: Url,
        credentials: Option<header::HeaderValue>,
        tenant_keys: bool,
    },
    Compute(ComputeStack),
}

pub(super) struct ChatCall<'a> {
    pub model: &'a str,
    pub input: &'a ChatInput,
    pub stream: bool,
    pub dispatch: Option<ChatDispatch<'a>>,
    pub tenant_key: Option<&'a header::HeaderValue>,
}

pub(super) struct ChatDispatch<'a> {
    pub pool: Option<&'a db::PgPool>,
    pub request_id: &'a str,
    pub trace_id: Option<&'a str>,
    pub lease: DateTime<Utc>,
}

impl ChatClient {
    pub(super) fn local(endpoint: Url, token: Option<&str>) -> anyhow::Result<Self> {
        let auth = token
            .map(authorization)
            .transpose()
            .map_err(|_| anyhow::anyhow!("invalid local chat bearer key"))?;
        Self::build(Destination::Direct {
            endpoint,
            credentials: auth,
            tenant_keys: false,
        })
    }

    pub(super) fn tenant(endpoint: Url, token: &str) -> anyhow::Result<Self> {
        let credentials = authorization(token)
            .map_err(|_| anyhow::anyhow!("invalid tenant binding service key"))?;
        Self::build(Destination::Direct { endpoint, credentials: Some(credentials), tenant_keys: true })
    }

    pub(super) fn requires_tenant_key(&self) -> bool {
        matches!(&self.destination, Destination::Direct { tenant_keys: true, .. })
    }

    pub(super) fn worker_origin(&self) -> Option<String> {
        match &self.destination {
            Destination::Direct { endpoint, .. } => Some(endpoint.origin().ascii_serialization()),
            Destination::Compute(_) => None,
        }
    }

    pub(super) fn compute_stack(&self) -> Option<&ComputeStack> {
        match &self.destination {
            Destination::Compute(stack) => Some(stack),
            _ => None,
        }
    }

    pub(super) fn compute(stack: ComputeStack) -> anyhow::Result<Self> {
        Self::build(Destination::Compute(stack))
    }

    fn build(destination: Destination) -> anyhow::Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            destination,
            client,
        })
    }

    pub(super) fn covers_compute_regions(&self, declared: &[String]) -> bool {
        match &self.destination {
            Destination::Compute(stack) => stack.covers_regions(declared),
            Destination::Direct { .. } => true,
        }
    }

    pub(super) async fn submit_for_run(&self, call: ChatCall<'_>) -> SubmitOutcome {
        let mut response = match self.send(&call).await {
            Ok(response) => response,
            Err(outcome) => return outcome,
        };
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if body.len() + chunk.len() <= MAX_BODY => {
                    body.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                Ok(Some(_)) => return SubmitOutcome::Uncertain("provider_response_too_large"),
                Err(_) => return SubmitOutcome::Uncertain("provider_transport_interrupted"),
            }
        }
        match completion(&body) {
            Some(result) => SubmitOutcome::Completed(result),
            None => SubmitOutcome::Uncertain("provider_response_unrecognized"),
        }
    }

    async fn send(&self, call: &ChatCall<'_>) -> Result<Response, SubmitOutcome> {
        if self.requires_tenant_key() != call.tenant_key.is_some() {
            return Err(SubmitOutcome::Rejected("provider_credential_unavailable"));
        }
        if let Destination::Compute(stack) = &self.destination {
            return self.send_compute(stack, call).await;
        }
        let request = self.direct_request(call).map_err(SubmitOutcome::Rejected)?;
        checked_response(request.send().await)
    }

    fn direct_request(&self, call: &ChatCall<'_>) -> Result<RequestBuilder, &'static str> {
        let Destination::Direct {
            endpoint,
            credentials,
            ..
        } = &self.destination
        else {
            return Err("provider_binding_unavailable");
        };
        let mut request = self.post(endpoint, credentials.clone());
        if let Some(key) = call.tenant_key {
            request = request.header("x-tokencraft-upstream-key", key.clone());
        }
        self.request(request, call)
    }

    async fn send_compute(
        &self,
        stack: &ComputeStack,
        call: &ChatCall<'_>,
    ) -> Result<Response, SubmitOutcome> {
        let Some(dispatch) = &call.dispatch else {
            return Err(SubmitOutcome::Rejected("compute_dispatch_unavailable"));
        };
        let trace = ComputeTrace::new(dispatch)?;
        let retry_until = Instant::now() + FAILOVER_WINDOW;
        let mut reason = "compute_targets_cooling_down";
        let remaining = || {
            (dispatch.lease - Utc::now())
                .to_std()
                .ok()
                .and_then(|remaining| remaining.checked_sub(LEASE_MARGIN))
                .filter(|remaining| !remaining.is_zero())
        };
        use crate::gateway_demo::compute_providers;
        let policy = if let Some(pool) = dispatch.pool {
            Some(tokio::time::timeout(Duration::from_secs(1),
                compute_providers::dispatch_policy(pool, dispatch.request_id))
                .await.ok().and_then(Result::ok)
                .ok_or(SubmitOutcome::Rejected("compute_policy_unavailable"))?)
        } else {
            None
        };
        let account = policy.as_ref().map(|(account, _)| account);
        let order = policy.as_ref().and_then(|(_, policies)| compute_providers::order(policies, &stack.name));
        let targets = stack.ordered_targets(dispatch.request_id, account, order);
        if targets.is_empty() {
            return Err(SubmitOutcome::Rejected("compute_targets_excluded"));
        }
        for target in targets {
            let now = Instant::now();
            let Some(lease_budget) = remaining() else {
                return Err(SubmitOutcome::Rejected("compute_dispatch_deadline"));
            };
            if now >= retry_until {
                break;
            }
            if !target.available(now) {
                continue;
            }
            let request = self.request(self.post(&target.endpoint, target.auth.clone()), call)
                .map_err(SubmitOutcome::Rejected)?;
            let evidence_budget = lease_budget.min(retry_until.saturating_duration_since(Instant::now()));
            let attempt = trace.start(target, stack, evidence_budget).await?;
            let Some(timeout) = remaining().filter(|_| Instant::now() < retry_until) else {
                attempt.record(ComputeOutcome::DeadlineBeforeSend, None).await;
                return Err(SubmitOutcome::Rejected("compute_dispatch_deadline"));
            };
            let result = request.timeout(timeout.min(REQUEST_TIMEOUT)).send().await;
            let http_status = result.as_ref().ok().map(|response| response.status().as_u16());
            let (failure, cooldown) = match result {
                Err(error) if error.is_connect() => {
                    (ComputeOutcome::ConnectionFailed, UNHEALTHY_COOLDOWN)
                }
                Ok(ref response)
                    if response.status() == StatusCode::TOO_MANY_REQUESTS
                        && target.retries_rate_limit() =>
                {
                    let seconds = response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(2)
                        .clamp(1, 30);
                    (ComputeOutcome::CapacityRejected, Duration::from_secs(seconds))
                }
                result => {
                    let (outcome, unhealthy) = match &result {
                        Ok(response) if response.status().is_success() => {
                            (ComputeOutcome::ResponseStarted, false)
                        }
                        Ok(response) => (ComputeOutcome::UpstreamError, response.status().is_server_error()),
                        Err(_) => (ComputeOutcome::TransportUnconfirmed, true),
                    };
                    if unhealthy {
                        // Avoid this target on fresh requests. The current request may have run.
                        target.cool_down_until(Instant::now() + UNHEALTHY_COOLDOWN);
                    }
                    attempt.record(outcome, http_status).await;
                    return checked_response(result);
                }
            };
            reason = failure.code();
            target.cool_down_until(Instant::now() + cooldown);
            attempt.record(failure, http_status).await;
        }
        Err(SubmitOutcome::Rejected(reason))
    }

    fn post(&self, endpoint: &Url, auth: Option<header::HeaderValue>) -> RequestBuilder {
        let request = self.client.post(endpoint.clone());
        match auth {
            Some(auth) => request.header(header::AUTHORIZATION, auth),
            None => request,
        }
    }

    fn request(
        &self,
        request: RequestBuilder,
        call: &ChatCall<'_>,
    ) -> Result<RequestBuilder, &'static str> {
        let input = call.input;
        #[derive(Serialize)]
        struct StreamOptions {
            include_usage: bool,
        }
        #[derive(Serialize)]
        struct WireRequest<'a> {
            model: &'a str,
            messages: &'a [Message],
            max_tokens: u32,
            stream: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            stream_options: Option<StreamOptions>,
            #[serde(flatten)]
            options: ChatOptions,
        }
        let request = if call.stream {
            request.header(header::ACCEPT, "text/event-stream")
        } else {
            request
        };
        let trace_id = call.dispatch.as_ref().and_then(|dispatch| dispatch.trace_id);
        let request = crate::middleware::trace_context::attach_provider_trace(request, trace_id);
        Ok(request.json(&WireRequest {
            model: call.model,
            messages: &input.messages,
            max_tokens: input.max_tokens,
            stream: call.stream,
            stream_options: call.stream.then_some(StreamOptions {
                include_usage: true,
            }),
            options: input.options,
        }))
    }
}

fn checked_response(result: reqwest::Result<Response>) -> Result<Response, SubmitOutcome> {
    match result {
        Ok(response) if response.status().is_success() => Ok(response),
        Ok(response) => Err(rejection(response.status().as_u16())),
        Err(_) => Err(SubmitOutcome::Uncertain("provider_transport_interrupted")),
    }
}

fn rejection(status: u16) -> SubmitOutcome {
    match status {
        400 | 401 | 402 | 403 | 404 | 413 | 422 | 429 => {
            SubmitOutcome::Rejected("provider_rejected_request")
        }
        _ => SubmitOutcome::Uncertain("provider_outcome_unconfirmed"),
    }
}

pub(super) fn authorization(key: &str) -> Result<header::HeaderValue, header::InvalidHeaderValue> {
    let mut auth = header::HeaderValue::from_str(&format!("Bearer {key}"))?;
    auth.set_sensitive(true);
    Ok(auth)
}

#[derive(Deserialize)]
struct WireResponse {
    id: String,
    model: Option<String>,
    provider: Option<Value>,
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    finish_reason: Option<String>,
    message: Reply,
}

#[derive(Deserialize)]
struct Reply {
    content: Option<String>,
}

#[derive(Default, Deserialize)]
struct Usage {
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
}

fn completion(body: &[u8]) -> Option<Completion> {
    let mut reply: WireResponse = serde_json::from_slice(body).ok()?;
    let handle = safe_label(reply.id, 160)?;
    if reply.choices.len() != 1 {
        return None;
    }
    let choice = reply.choices.pop()?;
    if choice.finish_reason.as_deref() == Some("error") {
        return None;
    }
    let text = choice
        .message
        .content
        .filter(|value| value.len() <= MAX_RESULT)?;
    let usage = reply.usage.unwrap_or_default();
    Some(Completion {
        result: ChatResult {
            text,
            finish_reason: choice.finish_reason.and_then(|value| safe_label(value, 48)),
        },
        handle,
        model: reply.model.and_then(|value| safe_label(value, 160)),
        provider: provider_label(reply.provider.as_ref().and_then(Value::as_str)).map(str::to_owned),
        input_tokens: usage.prompt_tokens.filter(|count| *count >= 0),
        output_tokens: usage.completion_tokens.filter(|count| *count >= 0),
        cost_usd_nanos: None,
    })
}

fn safe_label(value: String, max: usize) -> Option<String> {
    (!value.is_empty() && value.len() <= max && !value.chars().any(char::is_control))
        .then_some(value)
}
