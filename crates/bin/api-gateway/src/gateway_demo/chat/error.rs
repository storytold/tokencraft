use axum::http::{header::{RETRY_AFTER, WWW_AUTHENTICATE}, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::{ApiError, ACTIVE_REQUEST_LIMIT_MESSAGE, MONTHLY_BUDGET_MESSAGE, PROVIDER_CREDENTIAL_MESSAGE, SUBMISSION_RATE_MESSAGE};

use super::AcceptedRequest;

pub const X_REQUEST_ID: &str = "x-request-id";
pub const X_SHOULD_RETRY: &str = "x-should-retry";

pub fn request_headers(accepted: &AcceptedRequest) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let id = HeaderValue::from_str(accepted.id.as_str()).expect("request IDs are header-safe");
    headers.insert(X_REQUEST_ID, id.clone());
    headers.insert(super::super::REQUEST_ID_HEADER, id);
    accepted.append_trace_header(&mut headers);
    headers.insert(
        axum::http::header::LOCATION,
        HeaderValue::from_str(&accepted.status_url).expect("status URLs are header-safe"),
    );
    headers
}

#[derive(Serialize, ToSchema)]
#[schema(as = ChatErrorDetail)]
struct ErrorBody {
    message: String,
    #[serde(rename = "type")]
    kind: &'static str,
    code: &'static str,
    param: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub(super) struct ChatErrorResponse {
    error: ErrorBody,
}

pub struct ChatError {
    status: StatusCode,
    body: ErrorBody,
    headers: HeaderMap,
}

impl ChatError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        let kind = match status {
            StatusCode::UNAUTHORIZED => "authentication_error",
            StatusCode::FORBIDDEN => "permission_error",
            StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
            _ if status.is_server_error() => "server_error",
            _ => "invalid_request_error",
        };
        Self {
            status,
            body: ErrorBody {
                kind,
                code,
                param: None,
                message: message.into(),
            },
            headers: HeaderMap::new(),
        }
    }

    pub fn parameter(code: &'static str, message: impl Into<String>, param: &str) -> Self {
        let mut error = Self::new(StatusCode::BAD_REQUEST, code, message);
        error.body.param = Some(param.to_owned());
        error
    }

    pub fn traced(mut self, accepted: &AcceptedRequest) -> Self {
        self.headers.extend(request_headers(accepted));
        self
    }

    pub(super) fn responses_parameter(mut self) -> Self {
        let name = match self.body.param.as_deref() {
            Some("messages") => "input",
            Some("max_completion_tokens" | "max_tokens") => "max_output_tokens",
            _ => return self,
        };
        self.body.param = Some(name.into());
        self
    }

    pub(super) fn responses_data(self) -> serde_json::Value {
        serde_json::json!({
            "message": self.body.message, "code": self.body.code, "param": self.body.param
        })
    }

    pub(super) fn stream_data(self) -> String {
        serde_json::to_string(&ChatErrorResponse { error: self.body })
            .expect("chat error response contains only serializable fields")
    }
}

impl From<ApiError> for ChatError {
    fn from(error: ApiError) -> Self {
        let retry_after = error.retry_after();
        let (status, code, message) = match error {
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, "invalid_request", message),
            ApiError::Unauthorized(message) => {
                (StatusCode::UNAUTHORIZED, "invalid_api_key", message)
            }
            ApiError::Forbidden(message) => {
                (StatusCode::FORBIDDEN, "insufficient_permissions", message)
            }
            ApiError::NotFound(message) => (StatusCode::NOT_FOUND, "not_found", message),
            ApiError::Conflict(message) => (StatusCode::CONFLICT, "conflict", message),
            ApiError::ArchiveRetrievalRequired(message) => {
                (StatusCode::CONFLICT, "archive_retrieval_required", message)
            }
            ApiError::QuoteStale => (
                StatusCode::CONFLICT,
                "quote_stale",
                crate::error::QUOTE_STALE_MESSAGE.into(),
            ),
            ApiError::BudgetExceeded => (
                StatusCode::CONFLICT,
                "monthly_budget_exceeded",
                MONTHLY_BUDGET_MESSAGE.into(),
            ),
            ApiError::ProviderCredentialUnavailable => (
                StatusCode::CONFLICT,
                "provider_credential_unavailable",
                PROVIDER_CREDENTIAL_MESSAGE.into(),
            ),
            ApiError::ActiveRequestLimit => (
                StatusCode::TOO_MANY_REQUESTS,
                "active_request_limit",
                ACTIVE_REQUEST_LIMIT_MESSAGE.into(),
            ),
            ApiError::SubmissionRateLimit(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                "submission_rate_limit",
                SUBMISSION_RATE_MESSAGE.into(),
            ),
            ApiError::AdmissionsPaused => (
                StatusCode::SERVICE_UNAVAILABLE,
                "admissions_paused",
                crate::error::ADMISSIONS_PAUSED_MESSAGE.into(),
            ),
            ApiError::NotImplemented(message) => {
                (StatusCode::NOT_IMPLEMENTED, "not_implemented", message)
            }
            ApiError::ServiceUnavailable(message) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                message,
            ),
            ApiError::ExportCapacity => (
                StatusCode::SERVICE_UNAVAILABLE,
                "export_capacity",
                crate::error::EXPORT_CAPACITY_MESSAGE.into(),
            ),
            ApiError::ArtifactDownloadCapacity => (
                StatusCode::SERVICE_UNAVAILABLE,
                "artifact_download_capacity",
                crate::error::ARTIFACT_DOWNLOAD_CAPACITY_MESSAGE.into(),
            ),
            ApiError::Internal(message) => {
                tracing::error!(detail = %message, "chat compatibility handler failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".into(),
                )
            }
        };
        let mut converted = Self::new(status, code, message);
        if let Some(retry_after) = retry_after {
            converted.headers.insert(RETRY_AFTER, retry_after);
        }
        converted
    }
}

impl IntoResponse for ChatError {
    fn into_response(mut self) -> Response {
        if self.status == StatusCode::UNAUTHORIZED {
            self.headers
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        self.headers
            .insert(X_SHOULD_RETRY, HeaderValue::from_static("false"));
        (
            self.status,
            self.headers,
            Json(ChatErrorResponse { error: self.body }),
        )
            .into_response()
    }
}
