use std::collections::BTreeMap;

use db_queries::gateway_requests::{active_requests, RequestScope};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use super::{catalog::Provider, routing_policy::Policies};
use crate::error::{ApiError, ApiResult};

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ActiveRequestLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u32>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    by_provider: BTreeMap<Provider, u32>,
}

impl ActiveRequestLimits {
    pub fn is_empty(&self) -> bool {
        self.total.is_none() && self.by_provider.is_empty()
    }
}

/// The admission transaction already holds its shared routing policy lock.
/// Each check includes the newly inserted request and earlier committed ones.
pub(super) async fn enforce(
    conn: &mut PgConnection,
    scope: RequestScope<'_>,
    policies: &Policies,
    provider: &Provider,
) -> ApiResult<()> {
    let layers = [
        (None, &policies.organization.policy.active_request_limits),
        (
            Some(scope.project),
            &policies.project.policy.active_request_limits,
        ),
    ];
    if !layers.iter().any(|(_, limits)| !limits.is_empty()) {
        return Ok(());
    }
    // A single account lock coordinates overlapping project, account, and provider caps.
    active_requests::lock(&mut *conn, scope.account).await?;
    for (project, limits) in layers {
        let provider_limit = limits.by_provider.get(provider);
        if limits.total.is_none() && provider_limit.is_none() {
            continue;
        }
        let usage = active_requests::usage(&mut *conn, scope.account, project).await?;
        let provider_usage = usage
            .by_provider
            .get(provider.as_str())
            .copied()
            .unwrap_or(0);
        if limits
            .total
            .is_some_and(|limit| usage.total > i64::from(limit))
            || provider_limit.is_some_and(|limit| provider_usage > i64::from(*limit))
        {
            return Err(ApiError::ActiveRequestLimit { retry_after_seconds: Some(1) });
        }
    }
    Ok(())
}
