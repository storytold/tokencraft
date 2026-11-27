use std::collections::BTreeMap;

use axum::extract::{rejection::JsonRejection, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{sse::Event, Response};
use axum::{Extension, Json};
use serde::{de::IgnoredAny, Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;

use super::super::chat_input::{Message, Role};
use super::{ChatError, ChatErrorResponse, ChatRequest, Wire, WireRequest, DEFAULT_OUTPUT_TOKENS};

#[derive(Deserialize, ToSchema)]
pub(in crate::gateway_demo) struct CreateResponse {
    /// Configured TokenCraft chat model alias.
    model: String,
    input: Input,
    instructions: Option<String>,
    stream: Option<bool>,
    /// Use stateless Responses semantics. The native request keeps its normal expiry.
    store: Option<bool>,
    max_output_tokens: Option<u32>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    #[serde(flatten)]
    #[schema(ignore)]
    unsupported: BTreeMap<String, IgnoredAny>,
}

#[derive(Deserialize, ToSchema)]
#[serde(untagged)]
#[schema(as = ResponsesInput)]
enum Input {
    Text(String),
    Messages(Vec<InputMessage>),
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ResponsesInputMessage)]
struct InputMessage {
    #[serde(rename = "type")]
    _kind: Option<MessageType>,
    role: InputRole,
    content: Content,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ResponsesMessageType)]
enum MessageType {
    Message,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ResponsesInputRole)]
enum InputRole {
    Developer,
    System,
    User,
    Assistant,
}

#[derive(Deserialize, ToSchema)]
#[serde(untagged)]
#[schema(as = ResponsesMessageContent)]
enum Content {
    Text(String),
    Parts(Vec<TextPart>),
}

#[derive(Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = ResponsesInputText)]
enum TextPart {
    InputText { text: String },
}

#[derive(Clone, Copy)]
pub(super) struct Settings {
    max_output_tokens: u32,
    temperature: Option<f64>,
    top_p: Option<f64>,
}

impl CreateResponse {
    pub(super) fn normalize(self) -> Result<(ChatRequest, Wire), ChatError> {
        if let Some(field) = self.unsupported.keys().next() {
            return Err(super::unsupported(field));
        }
        if self.store == Some(true) {
            return Err(ChatError::parameter(
                "unsupported_value",
                "Set store to false for the stateless Responses endpoint.",
                "store",
            ));
        }
        let mut messages = Vec::new();
        if let Some(instructions) = self.instructions {
            messages.push(Message::plain(Role::System, instructions));
        }
        match self.input {
            Input::Text(text) => messages.push(Message::plain(Role::User, text)),
            Input::Messages(items) => messages.extend(items.into_iter().map(InputMessage::message)),
        };
        let wire = Wire::Responses(Settings {
            max_output_tokens: self.max_output_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS),
            temperature: self.temperature,
            top_p: self.top_p,
        });
        Ok((
            ChatRequest {
                model: self.model,
                messages,
                stream: self.stream,
                stream_options: None,
                n: None,
                max_tokens: None,
                max_completion_tokens: self.max_output_tokens,
                temperature: self.temperature,
                top_p: self.top_p,
                unsupported: BTreeMap::new(),
            },
            wire,
        ))
    }
}

impl InputMessage {
    fn message(self) -> Message {
        let role = match self.role {
            InputRole::Developer | InputRole::System => Role::System,
            InputRole::User => Role::User,
            InputRole::Assistant => Role::Assistant,
        };
        let text = match self.content {
            Content::Text(text) => text,
            Content::Parts(parts) => parts
                .into_iter()
                .map(|TextPart::InputText { text }| text)
                .collect(),
        };
        Message::plain(role, text)
    }
}

/// Create a stateless text response through the scoped gateway.
/// Supply a TokenCraft bearer key with request submission and read permission.
/// Streaming uses named Responses events. Follow Location for the native request trace.
#[utoipa::path(
    post, path = "/v1/responses", operation_id = "createResponse", tag = "chat",
    security(("api_key" = []), ("project_api_key" = [])),
    request_body = CreateResponse,
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "Stable key for the request intent."),
        ("X-TokenCraft-Client-Ref" = Option<String>, Header, description = "Authorized client reference."),
        ("X-TokenCraft-App-Ref" = Option<String>, Header, description = "Opaque application reference for usage and history."),
        ("X-TokenCraft-Max-Cost-Nanos" = Option<i64>, Header, description = "Estimated cost cap in nano-USD."),
        ("X-TokenCraft-Provider" = Option<String>, Header, description = "Authorized provider pin."),
        ("X-TokenCraft-Preferred-Providers" = Option<String>, Header, description = "Comma-separated provider priority or cheapest."),
        ("X-TokenCraft-Webhook-Subscription-Id" = Option<String>, Header, description = "Authorized completion webhook."),
    ),
    responses(
        (status = 200, description = "Text response or named server-sent events.",
            content((Envelope = "application/json"), (String = "text/event-stream")),
            headers(
                ("Location" = String, description = "Native request status URL."),
                ("TokenCraft-Request-Id" = String, description = "Canonical native request identifier."),
            )
        ),
        (status = 429, description = "The active-request, organization submission, or concurrent stream limit was reached.", body = ChatErrorResponse,
            headers(("Retry-After" = u32, description = "Suggested retry delay in seconds for admission throttles."))),
        (status = 504, description = "The buffered request is still pending. Poll Location.", body = ChatErrorResponse),
        (status = "default", description = "Request error.", body = ChatErrorResponse),
    )
)]
pub(in crate::gateway_demo) async fn create(
    State(state): State<super::AppState>,
    Extension(catalog): Extension<std::sync::Arc<super::CatalogStore>>,
    Extension(webhooks): Extension<Option<std::sync::Arc<super::WebhookConfig>>>,
    Extension(providers): Extension<Option<std::sync::Arc<super::ProviderRuntime>>>,
    headers: HeaderMap,
    body: Result<Json<CreateResponse>, JsonRejection>,
) -> Result<Response, ChatError> {
    super::execute(
        state,
        catalog,
        webhooks,
        providers,
        headers,
        WireRequest::Responses(body),
    )
    .await
}

#[derive(Serialize, ToSchema)]
#[schema(as = TextResponse)]
pub(super) struct Envelope {
    id: String,
    object: &'static str,
    created_at: i64,
    completed_at: Option<i64>,
    model: String,
    status: &'static str,
    error: Option<Value>,
    incomplete_details: Option<IncompleteDetails>,
    output: Vec<OutputMessage>,
    usage: Option<TokenUsage>,
    instructions: Option<String>,
    max_output_tokens: u32,
    temperature: Option<f64>,
    top_p: Option<f64>,
    background: bool,
    store: bool,
    metadata: std::collections::BTreeMap<String, String>,
    parallel_tool_calls: bool,
    previous_response_id: Option<String>,
    text: Value,
    tool_choice: &'static str,
    tools: [Value; 0],
    truncation: &'static str,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ResponsesIncompleteDetails)]
struct IncompleteDetails {
    reason: &'static str,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ResponsesOutputMessage)]
struct OutputMessage {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    role: &'static str,
    status: &'static str,
    content: Vec<OutputText>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ResponsesOutputText)]
struct OutputText {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
    annotations: [Value; 0],
    logprobs: [Value; 0],
}

#[derive(Serialize, ToSchema)]
#[schema(as = ResponsesUsage)]
struct TokenUsage {
    input_tokens: i32,
    output_tokens: i32,
    total_tokens: i64,
    input_tokens_details: InputDetails,
    output_tokens_details: OutputDetails,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ResponsesInputTokenDetails)]
struct InputDetails {
    cached_tokens: u8,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ResponsesOutputTokenDetails)]
struct OutputDetails {
    reasoning_tokens: u8,
}

pub(super) struct TextLifecycle<'a> {
    pub request: &'a str,
    pub model: &'a str,
    pub created: i64,
    pub settings: Settings,
}

impl TextLifecycle<'_> {
    fn response(&self) -> Envelope {
        Envelope {
            id: self.id("resp"),
            object: "response",
            created_at: self.created,
            completed_at: None,
            model: self.model.to_owned(),
            status: "in_progress",
            error: None,
            incomplete_details: None,
            output: Vec::new(),
            usage: None,
            instructions: None,
            max_output_tokens: self.settings.max_output_tokens,
            temperature: self.settings.temperature,
            top_p: self.settings.top_p,
            background: false,
            store: false,
            metadata: BTreeMap::new(),
            parallel_tool_calls: false,
            previous_response_id: None,
            text: json!({"format":{"type":"text"}}),
            tool_choice: "none",
            tools: [],
            truncation: "disabled",
        }
    }

    pub(super) fn terminal(
        &self,
        text: String,
        finish_reason: &str,
        completed: i64,
        tokens: Option<(i32, i32)>,
    ) -> Envelope {
        let mut response = self.response();
        response.completed_at = Some(completed);
        response.incomplete_details = match finish_reason {
            "length" => Some(IncompleteDetails {
                reason: "max_output_tokens",
            }),
            "content_filter" => Some(IncompleteDetails {
                reason: "content_filter",
            }),
            _ => None,
        };
        response.status = if response.incomplete_details.is_some() {
            "incomplete"
        } else {
            "completed"
        };
        response
            .output
            .push(self.item(response.status, vec![output_text(text)]));
        response.usage = tokens.map(|(input_tokens, output_tokens)| TokenUsage {
            input_tokens,
            output_tokens,
            total_tokens: i64::from(input_tokens) + i64::from(output_tokens),
            input_tokens_details: InputDetails { cached_tokens: 0 },
            output_tokens_details: OutputDetails {
                reasoning_tokens: 0,
            },
        });
        response
    }

    fn item(&self, status: &'static str, content: Vec<OutputText>) -> OutputMessage {
        OutputMessage {
            id: self.id("msg"),
            kind: "message",
            role: "assistant",
            status,
            content,
        }
    }

    fn id(&self, prefix: &str) -> String {
        format!(
            "{prefix}_{}",
            self.request.strip_prefix("req_").unwrap_or(self.request)
        )
    }
}

fn output_text(text: String) -> OutputText {
    OutputText {
        kind: "output_text",
        text,
        annotations: [],
        logprobs: [],
    }
}

#[derive(Default)]
pub(super) struct Stream {
    sequence: u64,
}

impl Stream {
    pub(super) fn created(&mut self, lifecycle: &TextLifecycle<'_>) -> [Event; 2] {
        let envelope = lifecycle.response();
        [
            self.event("response.created", json!({"response":envelope})),
            self.event("response.in_progress", json!({"response":envelope})),
        ]
    }

    pub(super) fn start_item(&mut self, lifecycle: &TextLifecycle<'_>) -> [Event; 2] {
        let item = lifecycle.item("in_progress", Vec::new());
        [
            self.event("response.output_item.added", json!({"output_index":0,"item":item})),
            self.event("response.content_part.added", json!({
                "output_index":0,"item_id":item.id,"content_index":0,"part":output_text(String::new())
            })),
        ]
    }

    pub(super) fn delta(&mut self, lifecycle: &TextLifecycle<'_>, text: String) -> Event {
        self.event("response.output_text.delta", json!({
            "output_index":0,"item_id":lifecycle.id("msg"),"content_index":0,"delta":text,"logprobs":[]
        }))
    }

    pub(super) fn finish(&mut self, response: Envelope) -> [Event; 4] {
        let item = &response.output[0];
        let part = &item.content[0];
        let terminal_type = if response.status == "incomplete" {
            "response.incomplete"
        } else {
            "response.completed"
        };
        [
            self.event("response.output_text.done", json!({
                "output_index":0,"item_id":item.id,"content_index":0,"text":part.text,"logprobs":[]
            })),
            self.event("response.content_part.done", json!({
                "output_index":0,"item_id":item.id,"content_index":0,"part":part
            })),
            self.event("response.output_item.done", json!({"output_index":0,"item":item})),
            self.event(terminal_type, json!({"response":response})),
        ]
    }

    pub(super) fn error(&mut self, error: ChatError) -> Event {
        self.event("error", error.responses_data())
    }

    fn event(&mut self, kind: &'static str, mut body: Value) -> Event {
        body["type"] = json!(kind);
        body["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        Event::default().event(kind).data(body.to_string())
    }
}

pub(super) fn json_error(error: JsonRejection) -> ChatError {
    match error.status() {
        StatusCode::PAYLOAD_TOO_LARGE | StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            super::json_error(error)
        }
        _ => ChatError::parameter(
            "invalid_request",
            "Use a model alias and a string or a list of plain-text input messages.",
            "input",
        ),
    }
}
