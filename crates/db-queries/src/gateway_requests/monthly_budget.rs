use std::collections::BTreeMap;

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
    clients: &BTreeMap<&str, i64>,
    month_start: Option<DateTime<Utc>>,
) -> sqlx::Result<bool> {
    // Policy read locks keep configured limits stable. With no shared cap, take every
    // applicable client lock in the same order, including the workflow application's lock.
    if organization.is_some() {
        lock(conn, scope.account.as_str()).await?;
    } else if project.is_some() {
        lock(conn, scope.project.as_str()).await?;
    } else {
        for reference in clients.keys() {
            lock(conn, &format!("{}:{reference}", scope.project)).await?;
        }
    }
    let aggregate = [(None, None, organization), (Some(scope.project), None, project)];
    let client = clients.iter().map(|(&reference, &cap)| (Some(scope.project), Some(reference), Some(cap)));
    for (target, reference, cap) in aggregate.into_iter().chain(client) {
        if cap.is_some()
            && !usage_in_month(&mut *conn, scope.account, target, reference, cap, month_start)
                .await?
                .within_limit
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn lock(conn: &mut PgConnection, key: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended('gateway_monthly_budget:' || $1, 0))",
        key,
    )
    .execute(conn)
    .await
    .map(|_| ())
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
    // Keep scope equality visible to generic plans on reused connections. The
    // client and workflow-app branches are disjoint even when their references match.
    macro_rules! select_usage {
        ($project:literal, $request_attribution:literal, $workflow_attribution:literal) => {
            sqlx::query_as!(
                MonthlyBudget,
                r#"WITH calendar AS (
             SELECT coalesce($5::timestamptz, date_trunc('month', now(), 'UTC')) AS start
           ), period AS (
             SELECT start, ((start AT TIME ZONE 'UTC') + interval '1 month') AT TIME ZONE 'UTC' AS next_month
             FROM calendar
           ), request_reservations AS (
             SELECT GREATEST(r.budget_reserved_usd_nanos, r.estimated_usd_nanos) AS amount
             FROM gateway_requests r CROSS JOIN period p
             WHERE r.account_id = $1 AND "# + $project + r#"
               AND (r.canceled_at IS NULL OR r.dispatched_at IS NOT NULL)
               AND ((r.workflow_budget_month_start IS NULL AND r.created_at >= p.start AND r.created_at < p.next_month)
                    OR r.workflow_budget_month_start = p.start)
               AND "# + $request_attribution + r#"
             UNION ALL
             SELECT GREATEST(r.budget_reserved_usd_nanos, r.estimated_usd_nanos) AS amount
             FROM gateway_requests r CROSS JOIN period p
             WHERE r.account_id = $1 AND "# + $project + r#"
               AND (r.canceled_at IS NULL OR r.dispatched_at IS NOT NULL)
               AND ((r.workflow_budget_month_start IS NULL AND r.created_at >= p.start AND r.created_at < p.next_month)
                    OR r.workflow_budget_month_start = p.start)
               AND r.workflow_app_id = $4 AND r.client_ref IS DISTINCT FROM $4
           ), amounts AS (
             SELECT COALESCE(SUM(r.amount), 0) AS total
             FROM request_reservations r
             UNION ALL
             SELECT coalesce(sum(r.budget_remaining_usd_nanos), 0)
             FROM workflow_runs r CROSS JOIN period p
             WHERE r.account_id = $1 AND "# + $project + r#"
               AND r.budget_month_start = p.start AND r.budget_remaining_usd_nanos > 0
               AND r.status IN ('queued', 'running', 'reconciling')
               AND "# + $workflow_attribution + r#"
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
        };
    }

    match (project, client_ref) {
        (Some(_), Some(_)) => select_usage!(
            "r.project_id = $2",
            "r.client_ref = $4",
            "(r.client_ref = $4 OR (r.version IS NOT NULL AND r.actor_id = $4))"
        ),
        (Some(_), None) => select_usage!("r.project_id = $2", "$4::text IS NULL", "$4::text IS NULL"),
        (None, Some(_)) => select_usage!(
            "$2::text IS NULL",
            "r.client_ref = $4",
            "(r.client_ref = $4 OR (r.version IS NOT NULL AND r.actor_id = $4))"
        ),
        (None, None) => select_usage!("$2::text IS NULL", "$4::text IS NULL", "$4::text IS NULL"),
    }
}
