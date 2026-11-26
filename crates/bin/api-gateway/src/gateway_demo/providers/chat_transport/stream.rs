use chrono::{DateTime, Utc};
use db_queries::gateway_requests::chat_streams;
use reqwest::header;
use serde::{de::IgnoredAny, Deserialize};

use super::super::payloads::Payloads;
use super::{
    safe_label, ChatCall, ChatClient, ChatDispatch, ChatInput, ChatResult, Completion,
    SubmitOutcome, Usage, MAX_BODY, MAX_RESULT,
};

const MAX_FRAME: usize = 64 * 1024;
const MAX_CHUNKS: i16 = 512;

pub(in crate::gateway_demo::providers) struct StreamSink<'a> {
    pub pool: &'a db::PgPool,
    pub request: &'a str,
    pub trace_id: Option<&'a str>,
    pub lease: DateTime<Utc>,
    pub payloads: &'a Payloads,
}

impl ChatClient {
    pub(in crate::gateway_demo::providers) async fn stream(
        &self,
        model: &str,
        input: ChatInput,
        sink: StreamSink<'_>,
        tenant_key: Option<&header::HeaderValue>,
    ) -> sqlx::Result<SubmitOutcome> {
        let mut response = match self
            .send(&ChatCall {
                model,
                input: &input,
                stream: true,
                tenant_key,
                dispatch: Some(ChatDispatch {
                    pool: Some(sink.pool),
                    request_id: sink.request,
                    trace_id: sink.trace_id,
                    lease: sink.lease,
                }),
            })
            .await
        {
            Ok(response) => response,
            Err(outcome) => return Ok(outcome),
        };
        let is_sse = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        if !is_sse {
            return Ok(SubmitOutcome::Uncertain("provider_response_unrecognized"));
        }

        let mut state = Accumulator::default();
        let mut buffer = Vec::new();
        let mut data = Vec::new();
        let mut received = 0;
        let mut first_line = true;
        let mut skip_lf = false;
        while let Ok(Some(chunk)) = response.chunk().await {
            received += chunk.len();
            if received > MAX_BODY {
                break;
            }
            buffer.extend_from_slice(&chunk);
            let mut remaining = buffer.as_slice();
            while let Some(mut line) = next_line(&mut remaining, &mut skip_lf) {
                if first_line {
                    line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
                    first_line = false;
                }
                if line.is_empty() && !data.is_empty() {
                    if data == b"[DONE]" {
                        return Ok(state.outcome(true));
                    }
                    if !state.accept(&data, &sink).await? {
                        return Ok(state.outcome(false));
                    }
                    data.clear();
                } else if let Some(field) = line.strip_prefix(b"data:") {
                    if !data.is_empty() {
                        data.push(b'\n');
                    }
                    data.extend_from_slice(field.strip_prefix(b" ").unwrap_or(field));
                    if data.len() > MAX_FRAME {
                        return Ok(state.outcome(false));
                    }
                }
            }
            let consumed = buffer.len() - remaining.len();
            buffer.drain(..consumed);
            if buffer.len() > MAX_FRAME {
                break;
            }
        }
        Ok(state.outcome(false))
    }
}

fn next_line<'a>(input: &mut &'a [u8], skip_lf: &mut bool) -> Option<&'a [u8]> {
    // A CR ends the line immediately. Its optional LF can arrive in the next chunk.
    if !input.is_empty() && std::mem::take(skip_lf) {
        *input = input.strip_prefix(b"\n").unwrap_or(*input);
    }
    let bytes = *input;
    let end = bytes.iter().position(|byte| matches!(*byte, b'\r' | b'\n'))?;
    *skip_lf = bytes[end] == b'\r';
    *input = &bytes[end + 1..];
    Some(&bytes[..end])
}

#[derive(Deserialize)]
struct Frame {
    id: String,
    model: Option<String>,
    provider: Option<String>,
    choices: Vec<Choice>,
    usage: Option<Usage>,
    error: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct Choice {
    index: u8,
    #[serde(default)]
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<IgnoredAny>,
    function_call: Option<IgnoredAny>,
}

#[derive(Default)]
struct Accumulator {
    handle: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    finish_reason: Option<String>,
    text: String,
    usage: Usage,
    ordinal: i16,
}

impl Accumulator {
    async fn accept(&mut self, data: &[u8], sink: &StreamSink<'_>) -> sqlx::Result<bool> {
        let Ok(frame) = serde_json::from_slice::<Frame>(data) else {
            return Ok(false);
        };
        let Some(handle) = safe_label(frame.id, 160) else {
            return Ok(false);
        };
        if self
            .handle
            .as_ref()
            .is_some_and(|existing| existing != &handle)
        {
            return Ok(false);
        }
        self.handle.get_or_insert(handle);
        self.model = frame
            .model
            .and_then(|value| safe_label(value, 160))
            .or(self.model.take());
        self.provider = frame
            .provider
            .and_then(|value| safe_label(value, 120))
            .or(self.provider.take());
        if let Some(usage) = frame.usage {
            self.usage = usage;
        }
        if frame.error.is_some() {
            return Ok(false);
        }
        let [choice] = frame.choices.as_slice() else {
            return Ok(frame.choices.is_empty());
        };
        if self.finish_reason.is_some()
            || choice.index != 0
            || choice.delta.tool_calls.is_some()
            || choice.delta.function_call.is_some()
        {
            return Ok(false);
        }
        if let Some(content) = choice
            .delta
            .content
            .as_ref()
            .filter(|text| !text.is_empty())
        {
            if self.text.len() + content.len() > MAX_RESULT || self.ordinal == MAX_CHUNKS {
                return Ok(false);
            }
            let ordinal = self.ordinal + 1;
            let Some(payload) =
                sink.payloads
                    .seal(sink.request, &purpose(ordinal), content.as_bytes().to_vec())
            else {
                return Ok(false);
            };
            if !chat_streams::append(
                sink.pool,
                sink.request,
                sink.lease,
                ordinal,
                &payload,
                self.handle.as_deref().expect("validated upstream handle"),
            )
            .await?
            {
                return Ok(false);
            }
            self.ordinal = ordinal;
            self.text.push_str(content);
        }
        if let Some(reason) = choice.finish_reason.as_deref() {
            if !matches!(reason, "stop" | "length" | "content_filter") {
                return Ok(false);
            }
            self.finish_reason = Some(reason.into());
        }
        Ok(true)
    }

    fn outcome(self, done: bool) -> SubmitOutcome {
        let Some(handle) = self.handle else {
            return SubmitOutcome::Uncertain("provider_stream_interrupted");
        };
        let finished = done && self.finish_reason.is_some();
        let completion = Completion {
            result: ChatResult {
                text: self.text,
                finish_reason: self.finish_reason,
            },
            handle,
            model: self.model,
            provider: self.provider,
            input_tokens: self.usage.prompt_tokens.filter(|value| *value >= 0),
            output_tokens: self.usage.completion_tokens.filter(|value| *value >= 0),
            cost_usd_nanos: None,
        };
        if finished {
            SubmitOutcome::Completed(completion)
        } else {
            SubmitOutcome::Interrupted(completion)
        }
    }
}

pub(in crate::gateway_demo) fn purpose(ordinal: i16) -> String {
    format!("stream/{ordinal}")
}

#[cfg(test)]
mod tests {
    use super::next_line;

    #[test]
    fn accepts_cr_lf_and_crlf_split_across_chunks() {
        let mut skip_lf = false;
        let mut first = b"data: one\r".as_slice();
        assert_eq!(next_line(&mut first, &mut skip_lf), Some(b"data: one".as_slice()));
        assert_eq!(next_line(&mut first, &mut skip_lf), None);

        let mut second = b"\n\rdata: two\n\ndata: partial".as_slice();
        assert_eq!(next_line(&mut second, &mut skip_lf), Some(b"".as_slice()));
        assert_eq!(next_line(&mut second, &mut skip_lf), Some(b"data: two".as_slice()));
        assert_eq!(next_line(&mut second, &mut skip_lf), Some(b"".as_slice()));
        assert_eq!(next_line(&mut second, &mut skip_lf), None);
        assert_eq!(second, b"data: partial");
    }
}
