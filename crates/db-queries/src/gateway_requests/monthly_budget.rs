use chrono::{DateTime, Utc};
use ids::{AccountId, GatewayProjectId};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};

use super::RequestScope;

#[derive(Serialize)]
pub struct MonthlyBudget {
    pub reserved_usd_nanos: String,
    pub limit_usd_nanos: Option<i64>,
    pub resets_at: DateTime<Utc>,
    #[serde(skip)]
    pub within_limit: bool,
}

// Each admission sees its own request and any commits from earlier lock holders.
pub async fn within_limits(
    conn: &mut PgConnection,
    scope: RequestScope<'_>,
    organization: Option<i64>,
    project: Option<i64>,
    client: Option<i64>,
    month_start: Option<DateTime<Utc>>,
) -> sqlx::Result<bool> {
    let client_ref = scope.client_ref;
    // Policy read locks keep the configured limits stable through admission. Every request
    // contributing to an enabled budget holds the same aggregate lock or a wider one.
    let lock = match (organization, project, client.zip(client_ref)) {
        (Some(_), _, _) => scope.account.as_str().to_owned(),
        (None, Some(_), _) => scope.project.as_str().to_owned(),
        (None, None, Some((_, reference))) => format!("{}:{reference}", scope.project),
        (None, None, None) => return Ok(true),
    };
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('gateway_monthly_budget:' || $1, 0))",
        lock
    )
    .execute(&mut *conn)
    .await?;
    for (target, reference, cap) in [
        (None, None, organization),
        (Some(scope.project), None, project),
        (Some(scope.project), client_ref, client),
    ] {
        if cap.is_some() {
            if !usage_in_month(&mut *conn, scope.account, target, reference, cap, month_start)
                .await?
                .within_limit
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

pub async fn usage<'e>(
    exec: impl PgExecutor<'e>,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
    client_ref: Option<&str>,
    limit: Option<i64>,
) -> sqlx::Result<MonthlyBudget> {
    usage_in_month(exec, account, project, client_ref, limit, None).await
}

async fn usage_in_month<'e>(
    exec: impl PgExecutor<'e>,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
    client_ref: Option<&str>,
    limit: Option<i64>,
    month_start: Option<DateTime<Utc>>,
) -> sqlx::Result<MonthlyBudget> {
    sqlx::query_as!(
        MonthlyBudget,
        r#"WITH calendar AS (
             SELECT coalesce($5::timestamptz, date_trunc('month', now(), 'UTC')) AS start
           ), period AS (
             SELECT start, ((start AT TIME ZONE 'UTC') + interval '1 month') AT TIME ZONE 'UTC' AS next_month
             FROM calendar
           ), amounts AS (
             SELECT COALESCE(SUM(GREATEST(r.budget_reserved_usd_nanos, r.estimated_usd_nanos)), 0) AS total
             FROM gateway_requests r CROSS JOIN period p
             WHERE r.account_id = $1 AND ($2::text IS NULL OR r.project_id = $2)
               AND ((r.workflow_budget_month_start IS NULL AND r.created_at >= p.start AND r.created_at < p.next_month)
                    OR r.workflow_budget_month_start = p.start)
               AND (r.canceled_at IS NULL OR r.dispatched_at IS NOT NULL)
               AND ($4::text IS NULL OR r.client_ref = $4)
             UNION ALL
             SELECT coalesce(sum(w.budget_remaining_usd_nanos), 0)
             FROM workflow_runs w CROSS JOIN period p
             WHERE w.account_id = $1 AND ($2::text IS NULL OR w.project_id = $2)
               AND w.budget_month_start = p.start AND w.budget_remaining_usd_nanos > 0
               AND w.status IN ('queued', 'running', 'reconciling')
               AND ($4::text IS NULL OR (w.version IS NOT NULL AND w.actor_id = $4))
           ), usage AS (
             SELECT sum(total) AS total FROM amounts
           )
           SELECT total::text AS "reserved_usd_nanos!",
                  $3::bigint AS "limit_usd_nanos?",
                  next_month AS "resets_at!",
                  ($3::bigint IS NULL OR total <= $3::bigint) AS "within_limit!"
           FROM usage CROSS JOIN period"#,
        account.as_str(),
        project.map(GatewayProjectId::as_str),
        limit,
        client_ref,
        month_start,
    )
    .fetch_one(exec)
    .await
}
