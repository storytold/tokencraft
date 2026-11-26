use ids::{AccountId, GatewayProjectId};
use sqlx::{PgConnection, PgExecutor};

use super::PolicyRow;

pub async fn current<'e>(
    exec: impl PgExecutor<'e>,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
) -> sqlx::Result<Vec<PolicyRow>> {
    sqlx::query_as!(
        PolicyRow,
        r#"(SELECT revision AS "revision!", policy AS "policy!", project_id FROM gateway_routing_policies
          WHERE account_id = $1 AND project_id IS NULL ORDER BY revision DESC LIMIT 1)
         UNION ALL
         (SELECT revision, policy, project_id FROM gateway_routing_policies
          WHERE account_id = $1 AND project_id = $2 ORDER BY revision DESC LIMIT 1)"#,
        account.as_str(),
        project.map(GatewayProjectId::as_str),
    )
    .fetch_all(exec)
    .await
}

#[derive(sqlx::FromRow)]
pub struct ScopedPolicyRow {
    pub account_id: AccountId,
    pub scope_project_id: GatewayProjectId,
    #[sqlx(flatten)]
    pub row: PolicyRow,
}

pub async fn current_for_projects(
    conn: &mut PgConnection,
    projects: &[(&AccountId, &GatewayProjectId)],
) -> sqlx::Result<Vec<ScopedPolicyRow>> {
    let (accounts, projects): (Vec<_>, Vec<_>) = projects.iter()
        .map(|(account, project)| (account.as_str(), project.as_str()))
        .unzip();
    sqlx::query_as(
        r#"SELECT scope.account_id, scope.project_id AS scope_project_id,
                  current.revision, current.policy, current.project_id
             FROM unnest($1::text[], $2::text[]) AS scope(account_id, project_id)
             CROSS JOIN LATERAL (
                 (SELECT revision, policy, project_id FROM gateway_routing_policies
                  WHERE account_id = scope.account_id AND project_id IS NULL
                  ORDER BY revision DESC LIMIT 1)
                 UNION ALL
                 (SELECT revision, policy, project_id FROM gateway_routing_policies
                  WHERE account_id = scope.account_id AND project_id = scope.project_id
                  ORDER BY revision DESC LIMIT 1)
             ) current"#,
    )
    .bind(accounts)
    .bind(projects)
    .fetch_all(conn)
    .await
}
