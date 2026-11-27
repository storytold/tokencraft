use std::error::Error as _;

use reqwest::StatusCode;

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
