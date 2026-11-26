use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = ChatMessageRole)]
pub(super) enum Role {
    System,
    User,
    Assistant,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = ChatMessage)]
pub struct Message {
    role: Role,
    content: String,
}

impl Message {
    pub(super) fn plain(role: Role, content: String) -> Self {
        Self { role, content }
    }
}

pub fn validate_messages(messages: &[Message], input_budget: u32) -> bool {
    message_input_bound(messages).is_some_and(|bound| bound <= input_budget)
}

pub fn message_input_bound(messages: &[Message]) -> Option<u32> {
    if !(1..=16).contains(&messages.len())
        || messages
            .iter()
            .any(|message| message.content.trim().is_empty())
        || !messages
            .iter()
            .any(|message| matches!(message.role, Role::User))
    {
        return None;
    }
    input_bound_from_message_lengths(messages.iter().map(|message| message.content.len()))
}

pub(super) fn input_bound_from_message_lengths(lengths: impl IntoIterator<Item = usize>) -> Option<u32> {
    let bound = lengths.into_iter().map(|length| length + 8).sum::<usize>();
    (bound <= 6000).then_some(bound as u32)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
}

impl ChatOptions {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.top_p.is_none()
    }

    pub fn invalid_parameter(self) -> Option<&'static str> {
        if self
            .temperature
            .is_some_and(|value| !(0.0..=2.0).contains(&value))
        {
            Some("temperature")
        } else if self
            .top_p
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            Some("top_p")
        } else {
            None
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct ChatInput {
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "ChatOptions::is_empty")]
    pub options: ChatOptions,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct ChatResult {
    pub text: String,
    pub finish_reason: Option<String>,
}
