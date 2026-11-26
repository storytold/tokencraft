use db_queries::gateway_requests::{client_budgets, monthly_budget, RequestScope};

use super::super::routing_policy::Policies;
use crate::error::{ApiError, ApiResult};

pub(super) async fn verify_reservation(
    conn: &mut sqlx::PgConnection,
    scope: RequestScope<'_>,
    policies: &Policies,
) -> ApiResult<()> {
    let client_limit = match scope.client_ref {
        Some(reference) => client_budgets::find(&mut *conn, scope.account, scope.project, reference)
            .await?
            .and_then(|budget| budget.limit_usd_nanos),
        None => None,
    };
    if !monthly_budget::within_limits(
        conn,
        scope,
        policies.organization.policy.monthly_estimated_budget_usd_nanos,
        policies.project.policy.monthly_estimated_budget_usd_nanos,
        client_limit,
        None,
    ).await? {
        return Err(ApiError::BudgetExceeded);
    }
    Ok(())
}
