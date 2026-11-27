//! Shared handler plumbing: request metadata extraction and session-cookie response helpers.

use std::net::IpAddr;

use axum::http::header::{SET_COOKIE, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::auth::cookie;
use crate::auth::service::{AuthOutcome, ReqMeta};
use crate::auth::token::SESSION_TTL;
use crate::handlers::api_types::UserBody;
use crate::state::AppState;

use ids::SessionId;
use serde::Serialize;
use utoipa::ToSchema;

/// Read the first address in a forwarding chain supplied by trusted ingress.
pub(crate) fn forwarded_client_ip(headers: &HeaderMap) -> Option<IpAddr> {
  let mut fields = headers.get_all("x-forwarded-for").iter();
  let value = fields.next()?.to_str().ok()?;
  if fields.next().is_some() {
    return None;
  }
  value.split(',').next()?.trim().parse().ok()
}

/// Extract optional client metadata for sessions and audits.
pub fn req_meta(headers: &HeaderMap) -> ReqMeta {
  let ip = forwarded_client_ip(headers).map(|ip| ip.to_string());
  let user_agent = headers.get(USER_AGENT).and_then(|v| v.to_str().ok()).map(str::to_string);
  ReqMeta { ip, user_agent }
}

pub fn set_cookie_header(resp: &mut Response, cookie: &str) {
  if let Ok(value) = HeaderValue::from_str(cookie) {
    resp.headers_mut().append(SET_COOKIE, value);
  }
}

/// The logged-in response body: the user + their new session id (the token itself travels only
/// in the HttpOnly cookie).
#[derive(Debug, Serialize, ToSchema)]
pub struct AuthSuccessResponse {
  pub user: UserBody,
  #[schema(value_type = String, example = "sess_01j9…")]
  pub session_id: SessionId,
}

/// Build the logged-in response: set the session cookie + return the typed body.
pub fn auth_success(state: &AppState, outcome: AuthOutcome, status: StatusCode) -> Response {
  let cookie =
    cookie::set_cookie(&outcome.token, &state.cookie_domain, state.cookie_secure, SESSION_TTL.num_seconds());
  let body = AuthSuccessResponse { user: UserBody::from_user(&outcome.user), session_id: outcome.session.id.clone() };
  let mut resp = (status, Json(body)).into_response();
  set_cookie_header(&mut resp, &cookie);
  resp
}

/// Re-set the session cookie on a typed response (used after `session_version` bumps).
pub fn with_reminted_cookie<T: Serialize>(state: &AppState, token: &str, body: T) -> Response {
  let cookie = cookie::set_cookie(token, &state.cookie_domain, state.cookie_secure, SESSION_TTL.num_seconds());
  let mut resp = (StatusCode::OK, Json(body)).into_response();
  set_cookie_header(&mut resp, &cookie);
  resp
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn optional_session_metadata_contains_only_a_parsed_forwarded_address() {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("fixture"));
    assert_eq!(req_meta(&headers).ip, None);

    for (value, expected) in [
      ("203.0.113.9, 10.0.0.1", Some("203.0.113.9")),
      ("  2001:0db8:0000:0000::8  ", Some("2001:db8::8")),
      ("unknown", None),
      ("192.0.2.0/24", None),
      ("unknown, 192.0.2.1", None),
    ] {
      headers.insert("x-forwarded-for", HeaderValue::from_static(value));
      let metadata = req_meta(&headers);
      assert_eq!(metadata.ip.as_deref(), expected);
      assert_eq!(metadata.user_agent.as_deref(), Some("fixture"));
    }

    headers.insert("x-forwarded-for", HeaderValue::from_static("192.0.2.1"));
    headers.append("x-forwarded-for", HeaderValue::from_static("192.0.2.2"));
    assert_eq!(req_meta(&headers).ip, None);
  }
}
