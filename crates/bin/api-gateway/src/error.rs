//! The gateway's error type. Every handler/service returns `Result<T, ApiError>`; the
//! `IntoResponse` impl renders a JSON envelope `{ "error": { "code", "message" } }`.

use axum::http::{header::RETRY_AFTER, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub(crate) struct GatewayError {
  error: GatewayErrorDetail,
}

#[derive(Serialize, ToSchema)]
struct GatewayErrorDetail {
  code: &'static str,
  message: String,
}

pub(crate) const MONTHLY_BUDGET_MESSAGE: &str = "request would exceed the monthly estimated budget";
pub(crate) const PROVIDER_CREDENTIAL_MESSAGE: &str = "save an active provider key for this project or organization";
pub(crate) const ACTIVE_REQUEST_LIMIT_MESSAGE: &str = "active request limit reached. Retry after outstanding work completes";
pub(crate) const SUBMISSION_RATE_MESSAGE: &str = "organization submission rate limit reached";
pub(crate) const ADMISSIONS_PAUSED_MESSAGE: &str = "new requests are temporarily paused for maintenance";

pub(crate) const QUOTE_STALE_MESSAGE: &str = "quote expired or changed; request a fresh comparison";

pub(crate) const EXPORT_CAPACITY_MESSAGE: &str = "event exports are busy; retry shortly";
pub(crate) const ARTIFACT_DOWNLOAD_CAPACITY_MESSAGE: &str = "artifact downloads are busy; retry shortly";

#[derive(Debug)]
pub enum ApiError {
  BadRequest(String),
  Unauthorized(String),
  Forbidden(String),
  NotFound(String),
  /// A uniqueness/state conflict (e.g. email already in use with a different method).
  Conflict(String),
  ArchiveRetrievalRequired(String),
  QuoteStale,
  BudgetExceeded,
  ActiveRequestLimit,
  SubmissionRateLimit(i64),
  AdmissionsPaused,
  ProviderCredentialUnavailable,
  ExportCapacity,
  ArtifactDownloadCapacity,
  NotImplemented(String),
  ServiceUnavailable(String),
  Internal(String),
}

impl ApiError {
  fn status(&self) -> StatusCode {
    match self {
      Self::BadRequest(_) => StatusCode::BAD_REQUEST,
      Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
      Self::Forbidden(_) => StatusCode::FORBIDDEN,
      Self::NotFound(_) => StatusCode::NOT_FOUND,
      Self::Conflict(_) => StatusCode::CONFLICT,
      Self::ArchiveRetrievalRequired(_) => StatusCode::CONFLICT,
      Self::QuoteStale | Self::BudgetExceeded => StatusCode::CONFLICT,
      Self::ActiveRequestLimit | Self::SubmissionRateLimit(_) => StatusCode::TOO_MANY_REQUESTS,
      Self::ProviderCredentialUnavailable => StatusCode::CONFLICT,
      Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
      Self::AdmissionsPaused | Self::ExportCapacity | Self::ArtifactDownloadCapacity
      | Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
      Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }

  fn code(&self) -> &'static str {
    match self {
      Self::BadRequest(_) => "bad_request",
      Self::Unauthorized(_) => "unauthorized",
      Self::Forbidden(_) => "forbidden",
      Self::NotFound(_) => "not_found",
      Self::Conflict(_) => "conflict",
      Self::ArchiveRetrievalRequired(_) => "archive_retrieval_required",
      Self::QuoteStale => "quote_stale",
      Self::BudgetExceeded => "monthly_budget_exceeded",
      Self::ActiveRequestLimit => "active_request_limit",
      Self::SubmissionRateLimit(_) => "submission_rate_limit",
      Self::AdmissionsPaused => "admissions_paused",
      Self::ProviderCredentialUnavailable => "provider_credential_unavailable",
      Self::ExportCapacity => "export_capacity",
      Self::ArtifactDownloadCapacity => "artifact_download_capacity",
      Self::NotImplemented(_) => "not_implemented",
      Self::ServiceUnavailable(_) => "service_unavailable",
      Self::Internal(_) => "internal_error",
    }
  }

  fn message(&self) -> &str {
    match self {
      Self::QuoteStale => QUOTE_STALE_MESSAGE,
      Self::BudgetExceeded => MONTHLY_BUDGET_MESSAGE,
      Self::ActiveRequestLimit => ACTIVE_REQUEST_LIMIT_MESSAGE,
      Self::SubmissionRateLimit(_) => SUBMISSION_RATE_MESSAGE,
      Self::AdmissionsPaused => ADMISSIONS_PAUSED_MESSAGE,
      Self::ProviderCredentialUnavailable => PROVIDER_CREDENTIAL_MESSAGE,
      Self::ExportCapacity => EXPORT_CAPACITY_MESSAGE,
      Self::ArtifactDownloadCapacity => ARTIFACT_DOWNLOAD_CAPACITY_MESSAGE,
      Self::BadRequest(m)
      | Self::Unauthorized(m)
      | Self::Forbidden(m)
      | Self::NotFound(m)
      | Self::Conflict(m)
      | Self::ArchiveRetrievalRequired(m)
      | Self::NotImplemented(m)
      | Self::ServiceUnavailable(m)
      | Self::Internal(m) => m,
    }
  }

  pub(crate) fn retry_after(&self) -> Option<HeaderValue> {
    match self {
      Self::SubmissionRateLimit(seconds) => HeaderValue::from_str(&seconds.to_string()).ok(),
      Self::AdmissionsPaused => Some(HeaderValue::from_static("5")),
      Self::ExportCapacity | Self::ArtifactDownloadCapacity => Some(HeaderValue::from_static("2")),
      _ => None,
    }
  }
}

impl IntoResponse for ApiError {
  fn into_response(self) -> Response {
    // Never leak internal details to the client; log them instead.
    let public_message = match &self {
      ApiError::Internal(m) => {
        tracing::error!("internal error: {m}");
        "internal server error".to_string()
      }
      other => other.message().to_string(),
    };
    let body = Json(GatewayError { error: GatewayErrorDetail { code: self.code(), message: public_message } });
    let mut response = (self.status(), body).into_response();
    if let Some(retry_after) = self.retry_after() {
      response.headers_mut().insert(RETRY_AFTER, retry_after);
    }
    response
  }
}

impl From<sqlx::Error> for ApiError {
  fn from(e: sqlx::Error) -> Self {
    ApiError::Internal(format!("db error: {e}"))
  }
}

pub type ApiResult<T> = Result<T, ApiError>;
