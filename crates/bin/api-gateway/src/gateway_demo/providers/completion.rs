use std::{sync::Arc, time::Duration};

use axum::{
    extract::{Extension, Path, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    Json,
};
use db_queries::gateway_requests::connector_completions;
use ring::hmac;
use serde::{Deserialize, Serialize};
use tracing::Instrument;

use super::ProviderRuntime;
use crate::{
    error::{ApiError, ApiResult},
    gateway_demo::{handlers::parse_request_id, readiness::Readiness, RequestErrorResponse},
    middleware::trace_context::work_span,
    state::AppState,
};

pub(super) struct CompletionTokens(hmac::Key);

#[derive(Serialize)]
pub(super) struct CompletionGrant {
    path: String,
    token: String,
}

impl CompletionTokens {
    pub(super) fn new(root: &[u8; 32]) -> Self {
        let key = hmac::Key::new(hmac::HMAC_SHA256, root);
        let domain = hmac::sign(&key, b"tokencraft/connector-completion/hmac/v1");
        Self(hmac::Key::new(hmac::HMAC_SHA256, domain.as_ref()))
    }

    pub(super) fn issue(&self, id: &str) -> CompletionGrant {
        CompletionGrant {
            path: format!("/v1/connector-jobs/{id}/completion"),
            token: hex::encode(hmac::sign(&self.0, id.as_bytes()).as_ref()),
        }
    }

    fn verify(&self, id: &str, token: &str) -> bool {
        let mut digest = [0u8; 32];
        hex::decode_to_slice(token, &mut digest).is_ok()
            && hmac::verify(&self.0, id.as_bytes(), &digest).is_ok()
    }
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompletionHint {
    version: u8,
}

#[utoipa::path(
    post, path = "/v1/connector-jobs/{id}/completion", operation_id = "notifyConnectorCompletion", tag = "native",
    summary = "Notify the gateway that a connector job has a terminal status",
    params(("id" = String, Path, description = "The request ID supplied to the connector.")),
    request_body = CompletionHint,
    security(("connector_completion_token" = [])),
    responses((status = 204, description = "The notification was accepted. Replays are safe."),
              (status = "default", response = RequestErrorResponse)),
)]
pub(crate) async fn receive(
    State(state): State<AppState>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    Json(hint): Json<CompletionHint>,
) -> ApiResult<StatusCode> {
    let id = parse_request_id(&raw)?;
    let denied = || ApiError::Unauthorized("invalid connector completion token".into());
    let mut authorization = headers.get_all(AUTHORIZATION).iter();
    let token = authorization.next().and_then(|header| header.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer ")).ok_or_else(denied)?;
    if authorization.next().is_some() || !runtime.as_ref().is_some_and(|service| {
        service.completion_tokens.verify(id.as_str(), token)
    }) {
        return Err(denied());
    }
    if hint.version != 1 {
        return Err(ApiError::BadRequest("unsupported completion notification version".into()));
    }
    connector_completions::receive(&state.pool, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) fn spawn(pool: db::PgPool, runtime: Arc<ProviderRuntime>, readiness: Arc<Readiness>) {
    let bindings: Vec<_> = runtime.connectors.completion_digests().map(str::to_owned).collect();
    if bindings.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_millis(500));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            readiness.work_tick(&mut timer).await;
            if let Err(error) = recover(&pool, &runtime, &bindings).await {
                tracing::warn!(sqlstate = ?error.as_database_error().and_then(|e| e.code()),
                    "connector completion lookup deferred");
            }
        }
    });
}

async fn recover(pool: &db::PgPool, runtime: &ProviderRuntime, bindings: &[String]) -> sqlx::Result<()> {
    let Some(lookup) = connector_completions::claim_lookup(pool, bindings).await? else {
        return Ok(());
    };
    let span = work_span("connector_completion_lookup", &lookup.request_id, lookup.trace_id.as_deref(),
                         lookup.admission_span_id.as_deref());
    async {
        if let Some(connector) = runtime.connectors.by_digest(&lookup.binding_digest) {
            if let Some(handle) = connector
                .lookup(&lookup.request_id, lookup.trace_id.as_deref())
                .await
            {
                connector_completions::reconcile(pool, &lookup, &handle).await?;
            }
        }
        Ok(())
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests {
    use super::CompletionTokens;

    #[test]
    fn a_completion_token_is_bound_to_its_request_and_gateway() {
        let signer = CompletionTokens::new(&[3; 32]);
        let grant = signer.issue("req_first");
        assert!(signer.verify("req_first", &grant.token));
        assert!(!signer.verify("req_second", &grant.token));
        assert!(!CompletionTokens::new(&[4; 32]).verify("req_first", &grant.token));
        assert!(!signer.verify("req_first", "bad"));
    }
}
