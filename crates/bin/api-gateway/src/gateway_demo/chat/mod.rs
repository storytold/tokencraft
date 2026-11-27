mod error;
pub(crate) mod responses;
mod stream;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::extract::{rejection::JsonRejection, FromRequestParts, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use db_queries::gateway_requests::RequestScope;
use ids::GatewayRequestId;
use serde::{de::IgnoredAny, Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    error::{ApiError, ApiResult},
    middleware::Caller,
    state::AppState,
};

use super::{
    access::{self, GatewayCaller, Permission, ScopeQuery},
    catalog::valid_identifier,
    catalog_refresh::CatalogStore,
    chat_input::{message_input_bound, ChatOptions, Message},
    providers::ProviderRuntime,
    quote::{QuoteInput, Workload},
    routing_policy::{parse_provider_order, PROVIDER_PREFERENCE_HEADER},
    requests::{self, AcceptedRequest, AdmissionInput, QuoteSelection, RequestStatus, RequestView},
    webhooks::WebhookConfig,
};
use error::{request_headers, ChatError, ChatErrorResponse};

pub(crate) use error::{X_REQUEST_ID, X_SHOULD_RETRY};
pub(crate) const CLIENT_REF: &str = "x-tokencraft-client-ref";
pub(crate) const MAX_COST_NANOS: &str = "x-tokencraft-max-cost-nanos";
pub(crate) const PROVIDER_HEADER: &str = "x-tokencraft-provider";
pub(crate) const WEBHOOK_SUBSCRIPTION_ID: &str = "x-tokencraft-webhook-subscription-id";

const WAIT: Duration = Duration::from_secs(8);
const DEFAULT_OUTPUT_TOKENS: u32 = 512;

#[derive(Clone, Copy)]
enum Wire {
    Chat { include_usage: bool },
    Responses(responses::Settings),
}

enum WireRequest {
    Chat(Result<Json<ChatRequest>, JsonRejection>),
    Responses(Result<Json<responses::CreateResponse>, JsonRejection>),
}

impl WireRequest {
    fn normalize(self) -> Result<(ChatRequest, Wire), ChatError> {
        match self {
            Self::Chat(body) => {
                let Json(body) = body.map_err(json_error)?;
                let include_usage = body
                    .stream_options
                    .as_ref()
                    .is_some_and(|options| options.include_usage);
                Ok((body, Wire::Chat { include_usage }))
            }
            Self::Responses(body) => body.map_err(responses::json_error)?.0.normalize(),
        }
    }
}

fn stream_example() -> &'static str {
    r#"data: {"id":"req_01j00000000000000000000000","object":"chat.completion.chunk","created":1730000000,"model":"studio_chat_wire_demo","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"req_01j00000000000000000000000","object":"chat.completion.chunk","created":1730000000,"model":"studio_chat_wire_demo","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"req_01j00000000000000000000000","object":"chat.completion.chunk","created":1730000000,"model":"studio_chat_wire_demo","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

"#
}

#[derive(Deserialize, ToSchema)]
pub(super) struct ChatRequest {
    /// TokenCraft model alias configured in the chat catalog.
    model: String,
    /// Plain-text messages, including at least one user message.
    #[schema(min_items = 1, max_items = 16)]
    messages: Vec<Message>,
    /// Select server-sent events instead of a buffered JSON response.
    stream: Option<bool>,
    stream_options: Option<StreamOptions>,
    /// This endpoint accepts one completion per request.
    #[schema(minimum = 1, maximum = 1)]
    n: Option<u32>,
    /// Alias for max_completion_tokens. Supply only one limit.
    #[schema(minimum = 1, maximum = 64000)]
    max_tokens: Option<u32>,
    /// Defaults to 512 when no token limit is supplied.
    #[schema(minimum = 1, maximum = 64000)]
    max_completion_tokens: Option<u32>,
    #[schema(minimum = 0, maximum = 2)]
    temperature: Option<f64>,
    #[schema(minimum = 0, maximum = 1)]
    top_p: Option<f64>,
    #[serde(flatten)]
    #[schema(ignore)]
    unsupported: BTreeMap<String, IgnoredAny>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ChatStreamOptions)]
struct StreamOptions {
    /// Include a final usage event when both upstream token counts are available.
    #[serde(default)]
    include_usage: bool,
}

impl ChatRequest {
    fn into_admission(
        self,
        headers: &HeaderMap,
        client_ref: Option<String>,
    ) -> Result<AdmissionInput, ChatError> {
        if let Some(field) = self.unsupported.keys().next() {
            return Err(unsupported(field));
        }
        if self.stream_options.is_some() && self.stream != Some(true) {
            return Err(ChatError::parameter(
                "unsupported_value",
                "Set stream to true when supplying stream_options.",
                "stream_options",
            ));
        }
        if self.n.is_some_and(|n| n != 1) {
            return Err(ChatError::parameter(
                "unsupported_value",
                "Use one completion per request.",
                "n",
            ));
        }
        if !valid_identifier(&self.model) {
            return Err(ChatError::parameter(
                "invalid_value",
                "Use a TokenCraft model alias.",
                "model",
            ));
        }
        let input_tokens = message_input_bound(&self.messages).ok_or_else(|| {
            ChatError::parameter("invalid_value", "Supply up to 16 non-empty text messages including a user role, within the 6000-byte fixture input limit.", "messages")
        })?;
        let (max_output_tokens, token_parameter) =
            match (self.max_tokens, self.max_completion_tokens) {
                (Some(_), Some(_)) => {
                    return Err(ChatError::parameter(
                        "conflicting_parameters",
                        "Supply one output token limit.",
                        "max_completion_tokens",
                    ))
                }
                (Some(value), None) => (value, "max_tokens"),
                (None, value) => (
                    value.unwrap_or(DEFAULT_OUTPUT_TOKENS),
                    "max_completion_tokens",
                ),
            };
        if !(1..=64_000).contains(&max_output_tokens) {
            return Err(ChatError::parameter(
                "invalid_value",
                "Use a token limit between 1 and 64000.",
                token_parameter,
            ));
        }
        let chat_options = ChatOptions {
            temperature: self.temperature,
            top_p: self.top_p,
        };
        if let Some(field) = chat_options.invalid_parameter() {
            return Err(ChatError::parameter(
                "invalid_value",
                "Sampling value is out of range.",
                field,
            ));
        }
        let max_total_usd_nanos = header(headers, MAX_COST_NANOS)?
            .map(|value| {
                value
                    .parse::<i64>()
                    .ok()
                    .filter(|cost| (0..=1_000_000_000_000).contains(cost))
                    .ok_or_else(|| {
                        ChatError::parameter(
                            "invalid_value",
                            "Use a nonnegative estimated cost cap in nano-USD.",
                            MAX_COST_NANOS,
                        )
                    })
            })
            .transpose()?;
        let provider = header(headers, PROVIDER_HEADER)?
            .map(|value| {
                value.parse().map_err(|message| {
                    ChatError::parameter("invalid_value", message, PROVIDER_HEADER)
                })
            })
            .transpose()?;
        let preferred_providers = header(headers, PROVIDER_PREFERENCE_HEADER)?
            .map(|value| parse_provider_order(value).map_err(|_| {
                ChatError::parameter("invalid_value", "Supply a unique comma-separated provider order or cheapest.", PROVIDER_PREFERENCE_HEADER)
            }))
            .transpose()?;
        Ok(AdmissionInput {
            pinned_offer_id: None,
            selection: QuoteSelection::Chat,
            input: QuoteInput {
                data_requirements: None,
                min_quality: None,
                min_usage_accounts: None,
                model: self.model,
                provider,
                preferred_providers,
                max_total_usd_nanos,
                workload: Workload::Text {
                    max_input_tokens: input_tokens,
                    max_output_tokens,
                },
            },
            client_ref,
            app_ref: header(headers, requests::APP_REF)?.map(str::to_owned),
            webhook_subscription_id: header(headers, WEBHOOK_SUBSCRIPTION_ID)?.map(str::to_owned),
            messages: Some(self.messages),
            image_prompt: None,
            video_prompt: None,
            audio_prompt: None,
            mesh_prompt: None,
            chat_options,
            stream: self.stream == Some(true),
        })
    }
}

fn unsupported(field: &str) -> ChatError {
    if valid_identifier(field) {
        ChatError::parameter(
            "unsupported_parameter",
            "This parameter is unavailable on this text endpoint.",
            field,
        )
    } else {
        ChatError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
            "Unsupported text request parameter.",
        )
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<Option<&'a str>, ChatError> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .filter(|_| values.next().is_none())
        .map(Some)
        .ok_or_else(|| {
            ChatError::parameter(
                "invalid_header",
                "Supply one valid value for this header.",
                name,
            )
        })
}

async fn bearer(headers: &HeaderMap, state: &AppState) -> Result<GatewayCaller, ChatError> {
    let secret = header(headers, "authorization")?
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, secret)| {
            scheme.eq_ignore_ascii_case("Bearer")
                && !secret.is_empty()
                && secret.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .map(|(_, secret)| secret)
        .ok_or_else(|| {
            ChatError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_api_key",
                "Supply a TokenCraft bearer key.",
            )
        })?;
    // Reuse the shared API-key extractor with just the bearer header for this protocol.
    let (mut parts, _) = Request::new(()).into_parts();
    let mut value = HeaderValue::from_str(&format!("Bearer {secret}"))
        .expect("the bearer syntax was validated");
    value.set_sensitive(true);
    parts.headers.insert(AUTHORIZATION, value);
    let caller = GatewayCaller::from_request_parts(&mut parts, state).await?;
    if matches!(caller, GatewayCaller::Account(Caller::Session(_))) {
        return Err(ChatError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Supply a TokenCraft bearer key.",
        ));
    }
    Ok(caller)
}

/// Create a buffered or streamed text chat completion.
///
/// Available when the gateway runs the local chat provider profile. Supply a TokenCraft bearer
/// key. A project key needs `requests:submit` and `requests:read`. With `stream: true`, replay the
/// same request body and idempotency key to recover the encrypted stream from the beginning.
/// A completed stream ends with `data: [DONE]`. Errors after the headers arrive are sent as a
/// JSON error frame followed by EOF. Follow `Location` to inspect the original request.
#[utoipa::path(
    post,
    path = "/v1/chat/completions",
    operation_id = "createChatCompletion",
    tag = "chat",
    security(("project_api_key" = []), ("api_key" = [])),
    request_body = ChatRequest,
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "Stable key for this request intent."),
        ("X-TokenCraft-Client-Ref" = Option<String>, Header, description = "Authorized client reference."),
        ("X-TokenCraft-App-Ref" = Option<String>, Header, description = "Opaque application reference for usage and history."),
        ("X-TokenCraft-Max-Cost-Nanos" = Option<i64>, Header, description = "Estimated price cap in nano-USD."),
        ("X-TokenCraft-Provider" = Option<String>, Header, description = "Require a specific TokenCraft provider that the routing policy allows."),
        ("X-TokenCraft-Preferred-Providers" = Option<String>, Header, description = "Comma-separated provider priority or cheapest."),
        ("X-TokenCraft-Webhook-Subscription-Id" = Option<String>, Header, description = "Authorized terminal webhook subscription."),
    ),
    responses(
        (status = 200, description = "Buffered completion or stream of completion chunks.",
            content((Completion = "application/json"), (String = "text/event-stream", example = stream_example)),
            headers(
                ("Location" = String, description = "Native status URL for the accepted request."),
                ("TokenCraft-Request-Id" = String, description = "Canonical ID of the accepted request."),
                ("x-request-id" = String, description = "Canonical ID of the accepted request."),
            )
        ),
        (status = 403, description = "The request lacks permission or its provider is blocked by policy.", body = ChatErrorResponse),
        (status = 410, description = "The buffered completion expired.", body = ChatErrorResponse),
        (status = 429, description = "The active-request, organization submission, or concurrent stream limit was reached.", body = ChatErrorResponse,
            headers(("Retry-After" = u32, description = "Suggested retry delay in seconds for admission throttles."))),
        (status = 502, description = "The buffered provider request failed or its outcome is unconfirmed.", body = ChatErrorResponse),
        (status = 504, description = "The buffered request is still pending. Poll Location.", body = ChatErrorResponse),
        (status = "default", description = "Chat request error.", body = ChatErrorResponse),
    )
)]
pub(super) async fn complete(
    State(state): State<AppState>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(webhooks): Extension<Option<Arc<WebhookConfig>>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    headers: HeaderMap,
    body: Result<Json<ChatRequest>, JsonRejection>,
) -> Result<Response, ChatError> {
    execute(
        state,
        catalog,
        webhooks,
        providers,
        headers,
        WireRequest::Chat(body),
    )
    .await
}

async fn execute(
    state: AppState,
    catalog: Arc<CatalogStore>,
    webhooks: Option<Arc<WebhookConfig>>,
    providers: Option<Arc<ProviderRuntime>>,
    headers: HeaderMap,
    request: WireRequest,
) -> Result<Response, ChatError> {
    let caller = bearer(&headers, &state).await?;
    if let GatewayCaller::Project(key) = &caller {
        if !key
            .scopes
            .iter()
            .any(|scope| scope == Permission::ReadRequests.as_str())
        {
            return Err(
                ApiError::Forbidden("chat completions require requests:read".into()).into(),
            );
        }
    }
    let access = access::resolve(
        &state,
        caller,
        ScopeQuery::default(),
        Permission::SubmitRequests,
    )
    .await?;
    let (body, wire) = request.normalize()?;
    let runtime = providers.as_deref().ok_or_else(|| {
        ChatError::new(
            StatusCode::NOT_FOUND,
            "chat_unavailable",
            "Enable a compatible chat offer in the local catalog.",
        )
    })?;
    let client = header(&headers, CLIENT_REF)?;
    let scope = access.for_client(client)?;
    let model = body.model.clone();
    let admission = body
        .into_admission(&headers, client.map(str::to_owned))
        .map_err(|error| match wire {
            Wire::Chat { .. } => error,
            Wire::Responses(_) => error.responses_parameter(),
        })?;
    let stream_slot = if admission.stream {
        Some(runtime.streams.clone().try_acquire_owned().map_err(|_| {
            ChatError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "stream_capacity",
                "The gateway has reached its concurrent stream limit.",
            )
        })?)
    } else {
        None
    };
    let generated;
    let key = match header(&headers, "idempotency-key")? {
        Some(key) => key,
        None => {
            generated = GatewayRequestId::generate();
            generated.as_str()
        }
    };
    let (accepted, _) = requests::submit(
        &state.pool,
        &catalog,
        webhooks.as_deref(),
        Some(runtime),
        None,
        &access,
        key,
        admission,
    )
    .await?;
    if let Some(slot) = stream_slot {
        return Ok(stream::response(
            state,
            providers.expect("admission required a configured provider"),
            accepted,
            model,
            headers,
            wire,
            slot,
        ));
    }
    let completion = tokio::time::timeout(WAIT, async {
        let trace = wait_for_terminal(&state.pool, scope, &accepted.id).await?;
        match trace.status {
            RequestStatus::Succeeded => {
                let caller = bearer(&headers, &state).await?;
                let read_access = access::resolve(
                    &state,
                    caller,
                    ScopeQuery::default(),
                    Permission::ReadRequests,
                )
                .await?;
                let result = requests::provider_result(
                    &state.pool,
                    Some(runtime),
                    read_access.for_client(client)?,
                    &accepted.id,
                )
                .await
                .map_err(|error| match error {
                    ApiError::NotFound(_) => ChatError::new(
                        StatusCode::GONE,
                        "result_expired",
                        "The completion text has expired. The request trace is still available.",
                    ),
                    other => other.into(),
                })?
                .result;
                let finish_reason = supported_finish_reason(result.finish_reason.as_deref())?;
                let created = trace.accepted_at().timestamp();
                let tokens = trace.token_counts();
                let response = match wire {
                    Wire::Responses(settings) => {
                        let lifecycle = responses::TextLifecycle {
                            request: accepted.id.as_str(),
                            model: &model,
                            created,
                            settings,
                        };
                        let complete_at = trace
                            .events
                            .last()
                            .map_or(created, |event| event.at.timestamp());
                        Json(lifecycle.terminal(result.text, finish_reason, complete_at, tokens))
                            .into_response()
                    }
                    Wire::Chat { .. } => Json(Completion {
                        id: accepted.id.clone(),
                        object: "chat.completion",
                        created,
                        model,
                        choices: [Choice {
                            index: 0,
                            message: AssistantMessage {
                                role: "assistant",
                                content: result.text,
                            },
                            finish_reason,
                        }],
                        usage: tokens.map(|(prompt_tokens, completion_tokens)| Usage {
                            prompt_tokens,
                            completion_tokens,
                            total_tokens: i64::from(prompt_tokens) + i64::from(completion_tokens),
                        }),
                    })
                    .into_response(),
                };
                Ok(response)
            }
            RequestStatus::AwaitingReconciliation => Err(ChatError::new(
                StatusCode::BAD_GATEWAY,
                "provider_outcome_unconfirmed",
                "The provider outcome is unconfirmed. Follow Location to inspect this request.",
            )),
            RequestStatus::Failed => Err(ChatError::new(
                StatusCode::BAD_GATEWAY,
                "provider_failed",
                "The provider request failed. Follow Location to inspect this request.",
            )),
            RequestStatus::Canceled => Err(ChatError::new(
                StatusCode::CONFLICT,
                "request_canceled",
                "The request was canceled.",
            )),
            RequestStatus::Queued | RequestStatus::Running => {
                unreachable!("wait returns only settled states")
            }
        }
    })
    .await
    .map_err(|_| {
        ChatError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "request_pending",
            "The request is still being processed. Follow Location to poll its status.",
        )
        .traced(&accepted)
    })?
    .map_err(|error| error.traced(&accepted))?;
    Ok((request_headers(&accepted), completion).into_response())
}

fn supported_finish_reason(value: Option<&str>) -> Result<&'static str, ChatError> {
    Ok(match value {
        Some("stop") => "stop",
        Some("length") => "length",
        Some("content_filter") => "content_filter",
        _ => {
            return Err(ChatError::new(
                StatusCode::BAD_GATEWAY,
                "unsupported_finish_reason",
                "The trace and result are available through the native request API.",
            ))
        }
    })
}

fn json_error(error: JsonRejection) -> ChatError {
    let status = error.status();
    let (code, message) = match status {
        StatusCode::PAYLOAD_TOO_LARGE => (
            "request_too_large",
            "The chat request exceeds the local size limit.",
        ),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => ("unsupported_media_type", "Send a JSON request."),
        _ => (
            "invalid_request",
            "Use a model alias and plain-text system, user, or assistant messages.",
        ),
    };
    let status = if status == StatusCode::UNPROCESSABLE_ENTITY {
        StatusCode::BAD_REQUEST
    } else {
        status
    };
    ChatError::new(status, code, message)
}

async fn wait_for_terminal(
    pool: &db::PgPool,
    scope: RequestScope<'_>,
    id: &GatewayRequestId,
) -> ApiResult<RequestView> {
    let mut delay = Duration::from_millis(100);
    loop {
        let view = requests::get(pool, scope, id).await?;
        if !matches!(view.status, RequestStatus::Queued | RequestStatus::Running) {
            return Ok(view);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_millis(600));
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = ChatCompletion)]
pub(super) struct Completion {
    #[schema(value_type = String)]
    id: GatewayRequestId,
    object: &'static str,
    created: i64,
    model: String,
    choices: [Choice; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ChatCompletionChoice)]
struct Choice {
    index: u8,
    message: AssistantMessage,
    finish_reason: &'static str,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ChatAssistantMessage)]
struct AssistantMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ChatTokenUsage)]
struct Usage {
    prompt_tokens: i32,
    completion_tokens: i32,
    total_tokens: i64,
}
