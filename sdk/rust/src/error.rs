use std::error::Error as _;
use std::time::Duration;

use reqwest::{Response, StatusCode};
use serde::Deserialize;

const MAX_GATEWAY_ERROR_BYTES: usize = 64 * 1024;
const GATEWAY_ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    InvalidInput(&'static str),
    #[error("this operation requires a TokenCraft API key")]
    MissingKey,
    #[error("TokenCraft operation timed out")]
    Timeout { request_id: Option<String> },
    #[error("could not connect to or read from the TokenCraft gateway")]
    Transport {
        #[source]
        source: reqwest::Error,
        request_id: Option<String>,
    },
    #[error("the gateway returned an invalid response")]
    Response {
        #[source]
        source: reqwest::Error,
        request_id: Option<String>,
    },
    #[error("TokenCraft returned HTTP {status} ({code}): {message}")]
    Gateway {
        status: StatusCode,
        code: String,
        message: String,
        request_id: Option<String>,
    },
}

impl Error {
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Gateway { request_id, .. }
            | Self::Timeout { request_id }
            | Self::Transport { request_id, .. }
            | Self::Response { request_id, .. } => request_id.as_deref(),
            Self::InvalidInput(_) | Self::MissingKey => None,
        }
    }

    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Gateway { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub(super) async fn from_gateway(response: Response, request_id: Option<String>) -> Self {
        let status = response.status();
        let problem = tokio::time::timeout(GATEWAY_ERROR_BODY_TIMEOUT, read_problem(response))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        Self::Gateway {
            status,
            code: problem.code.unwrap_or_else(|| "http_error".into()),
            message: problem
                .message
                .unwrap_or_else(|| "the gateway did not return an error message".into()),
            request_id,
        }
    }

    pub(super) fn from_http(source: reqwest::Error, request_id: Option<String>) -> Self {
        let source = source.without_url();
        if source.is_timeout() {
            return Self::Timeout { request_id };
        }
        // Reqwest wraps body transfer errors in its JSON decoder's error type.
        let interrupted_body = source
            .source()
            .and_then(|error| error.downcast_ref::<reqwest::Error>())
            .is_some_and(reqwest::Error::is_body);
        if source.is_decode() && !interrupted_body {
            return Self::Response { source, request_id };
        }
        Self::Transport { source, request_id }
    }
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    error: Problem,
}

#[derive(Default, Deserialize)]
struct Problem {
    code: Option<String>,
    message: Option<String>,
}

async fn read_problem(mut response: Response) -> Option<Problem> {
    let declared = response.content_length().unwrap_or_default();
    if declared > MAX_GATEWAY_ERROR_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > MAX_GATEWAY_ERROR_BYTES - bytes.len() {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<ErrorEnvelope>(&bytes).ok().map(|envelope| envelope.error)
}
