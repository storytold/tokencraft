use chrono::{DateTime, Utc};
use serde::Serialize;

use super::{compute::ComputeTargetAttempt, RouteRetry};
use crate::gateway_demo::{
    catalog::Provider,
    requests::{EventKind, RequestStatus, RequestView},
    upstream_labels::{model_label, provider_label, resembles_credential},
};

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct ProviderPath {
    admitted_offer_id: Option<String>,
    attempts: Vec<ProviderAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downstream: Option<DownstreamProvider>,
}

#[derive(Serialize, utoipa::ToSchema)]
struct ProviderAttempt {
    offer_id: String,
    provider: Option<Provider>,
    state: AttemptState,
    #[serde(skip_serializing_if = "Option::is_none")]
    adapter: Option<String>,
    /// Waterfall step containing the recorded evidence for this attempt.
    step_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reported_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acknowledged_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    compute_attempts: Vec<ComputeTargetAttempt>,
}

#[derive(Clone, Copy, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
enum AttemptState {
    RateLimited,
    Queued,
    Dispatching,
    Acknowledged,
    Succeeded,
    Failed,
    Canceled,
    OutcomeUnconfirmed,
}

#[derive(Clone, Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
enum DownstreamProvider {
    /// Identity named in the upstream response.
    Reported {
        provider: String,
    },
    Pending,
    NotDisclosed,
}

/// Routing evidence shared with support and the analysis model.
#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = RequestTraceDiagnosticPath)]
pub(super) struct DiagnosticPath {
    admitted_offer_id: Option<String>,
    attempts: Vec<DiagnosticAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downstream: Option<DownstreamProvider>,
}

#[derive(Serialize, utoipa::ToSchema)]
#[schema(as = RequestTraceDiagnosticAttempt)]
struct DiagnosticAttempt {
    offer_id: String,
    provider: Option<Provider>,
    state: AttemptState,
    step_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reported_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acknowledged_at: Option<DateTime<Utc>>,
}

impl ProviderPath {
    pub(super) fn diagnostics(&self) -> Option<DiagnosticPath> {
        (!self.attempts.is_empty()).then(|| DiagnosticPath {
            admitted_offer_id: self.admitted_offer_id.clone(),
            attempts: self.attempts.iter().map(|attempt| DiagnosticAttempt {
                offer_id: attempt.offer_id.clone(),
                provider: attempt.provider.clone(),
                state: attempt.state,
                step_id: attempt.step_id.clone(),
                requested_model: attempt.requested_model.clone(),
                reported_model: attempt.reported_model.clone(),
                acknowledged_at: attempt.acknowledged_at,
            }).collect(),
            downstream: self.downstream.clone(),
        })
    }

    pub(super) fn from_request(
        request: &RequestView,
        retries: &[RouteRetry],
        compute_attempts: Vec<ComputeTargetAttempt>,
    ) -> Self {
        let admitted = request.quote.selected_offer_id.as_deref();
        let current = retries
            .last()
            .map(|retry| retry.decision.next_offer_id.as_str())
            .or(admitted);
        let upstream = request.upstream.as_ref();
        let provider = |offer_id: &str| {
            request
                .quote
                .candidates
                .iter()
                .find(|candidate| candidate.offer_id == offer_id)
                .map(|candidate| candidate.provider.clone())
        };
        let mut attempts: Vec<_> = retries
            .iter()
            .enumerate()
            .map(|(index, retry)| ProviderAttempt {
                offer_id: retry.decision.offer_id.clone(),
                provider: provider(&retry.decision.offer_id),
                state: AttemptState::RateLimited,
                adapter: None,
                step_id: format!("route-retry-{}", index + 1),
                requested_model: None,
                reported_model: None,
                correlation_id: None,
                acknowledged_at: None,
                compute_attempts: vec![],
            })
            .collect();
        if let Some(offer_id) = current {
            let acknowledged_at = upstream.and_then(|row| row.accepted_at);
            let dispatched = request.events.iter().any(|event| {
                matches!(
                    event.kind,
                    EventKind::ProviderSubmitStarted | EventKind::MockProviderSubmitted
                )
            });
            let state = match request.status {
                RequestStatus::Queued => AttemptState::Queued,
                RequestStatus::Running if acknowledged_at.is_some() => AttemptState::Acknowledged,
                RequestStatus::Running => AttemptState::Dispatching,
                RequestStatus::Succeeded => AttemptState::Succeeded,
                RequestStatus::Failed => AttemptState::Failed,
                RequestStatus::Canceled => AttemptState::Canceled,
                RequestStatus::AwaitingReconciliation => AttemptState::OutcomeUnconfirmed,
            };
            let step_id = if dispatched {
                "provider"
            } else if matches!(
                request.status,
                RequestStatus::Queued | RequestStatus::Running
            ) {
                "queue"
            } else {
                "outcome"
            };
            attempts.push(ProviderAttempt {
                offer_id: offer_id.to_owned(),
                provider: provider(offer_id),
                state,
                adapter: Some(
                    upstream
                        .map_or("mock", |row| row.adapter.as_str())
                        .to_owned(),
                ),
                step_id: step_id.into(),
                requested_model: upstream
                    .and_then(|row| model_label(row.requested_model.as_deref()))
                    .map(str::to_owned),
                reported_model: upstream
                    .and_then(|row| model_label(row.reported_model.as_deref()))
                    .map(str::to_owned),
                correlation_id: request
                    .provider_handle
                    .as_deref()
                    .filter(|value| {
                        !value.is_empty()
                            && value.len() <= 160
                            && value.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric()
                                    || matches!(byte, b'_' | b'-' | b'.' | b':')
                            })
                            && !resembles_credential(value)
                    })
                    .map(str::to_owned),
                acknowledged_at,
                compute_attempts,
            });
        }

        let downstream = upstream.and_then(|row| {
            let contracted = current.and_then(provider);
            let reported = provider_label(row.reported_provider.as_deref()).filter(|name| {
                !contracted.as_ref().is_some_and(|contracted| name.eq_ignore_ascii_case(contracted.as_str()))
            });
            if let Some(provider) = reported {
                return Some(DownstreamProvider::Reported {
                    provider: provider.to_owned(),
                });
            }
            (row.adapter == "openrouter_chat").then_some(match request.status {
                RequestStatus::Queued | RequestStatus::Running => DownstreamProvider::Pending,
                _ => DownstreamProvider::NotDisclosed,
            })
        });

        Self {
            admitted_offer_id: admitted.map(str::to_owned),
            attempts,
            downstream,
        }
    }
}
