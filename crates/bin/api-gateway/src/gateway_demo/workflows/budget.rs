use db_queries::gateway_requests::{monthly_budget, RequestScope};

use super::super::{client_budgets, routing_policy::Policies};
use crate::error::{ApiError, ApiResult};

pub(super) async fn verify_reservation(
    conn: &mut sqlx::PgConnection,
    scope: RequestScope<'_>,
    policies: &Policies,
    application: Option<&str>,
) -> ApiResult<()> {
    let limits = client_budgets::admission_limits(conn, scope, application).await?;
    if !monthly_budget::within_limits(
        conn,
        scope,
        policies.organization.policy.monthly_estimated_budget_usd_nanos,
        policies.project.policy.monthly_estimated_budget_usd_nanos,
        &limits.clients,
        None,
    ).await? {
        return Err(ApiError::BudgetExceeded);
    }
    Ok(())
}
