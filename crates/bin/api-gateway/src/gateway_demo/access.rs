use std::str::FromStr;

use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts, HeaderValue},
};
use db_queries::{
    gateway_projects::{self, CredentialScope, ProjectAccess, ProjectFilter},
    gateway_requests::RequestScope,
};
use enums::MemberRole;
use ids::{AccountId, GatewayProjectId, UserId};
use serde::{Deserialize, Serialize};

use super::requests::validate_client_ref;
use crate::{
    auth::service::sha256_hex,
    error::{ApiError, ApiResult},
    middleware::{AuthContext, Caller},
    state::AppState,
};

pub const PROJECT_KEY_PREFIX: &str = "tc_project_";

pub(super) fn project_key_suffix(value: &HeaderValue) -> Option<&str> {
    let (scheme, key) = value.to_str().ok()?.trim().split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") {
        key.trim_start().strip_prefix(PROJECT_KEY_PREFIX)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Permission {
    #[serde(rename = "requests:read")]
    ReadRequests,
    #[serde(rename = "requests:submit")]
    SubmitRequests,
    #[serde(rename = "requests:cancel")]
    CancelRequests,
    #[serde(rename = "usage:read")]
    ReadUsage,
    #[serde(rename = "exports:read")]
    ReadExports,
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadRequests => "requests:read",
            Self::SubmitRequests => "requests:submit",
            Self::CancelRequests => "requests:cancel",
            Self::ReadUsage => "usage:read",
            Self::ReadExports => "exports:read",
        }
    }
}

pub enum GatewayCaller {
    Account(Caller),
    Project(CredentialScope),
}

#[async_trait]
impl FromRequestParts<AppState> for GatewayCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let project_key = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(project_key_suffix);
        if let Some(suffix) = project_key {
            let invalid = || ApiError::Unauthorized("invalid project credential".into());
            if suffix.len() != 64 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            let hash = sha256_hex(&format!("{PROJECT_KEY_PREFIX}{suffix}"));
            return gateway_projects::find_credential(&state.pool, &hash)
                .await?
                .map(Self::Project)
                .ok_or_else(invalid);
        }
        Ok(Self::Account(
            Caller::from_request_parts(parts, state).await?,
        ))
    }
}

#[derive(Default, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ScopeQuery {
    /// Account ID for session callers. A bearer key already carries its account.
    pub account_id: Option<String>,
    /// Project ID. Sessions without this value use their account's default project.
    pub project_id: Option<String>,
}

pub struct Access {
    account: AccountId,
    project: GatewayProjectId,
    pub actor_id: String,
    client_ref: Option<String>,
    pub(super) can_read_requests: bool,
    pub(super) can_restore_content: bool,
}

impl Access {
    pub(super) fn workflow(scope: RequestScope<'_>, actor_id: String) -> Self {
        Self {
            account: scope.account.clone(),
            project: scope.project.clone(),
            actor_id,
            client_ref: scope.client_ref.map(str::to_owned),
            can_read_requests: true,
            can_restore_content: false,
        }
    }

    pub fn scope(&self) -> RequestScope<'_> {
        RequestScope {
            account: &self.account,
            project: &self.project,
            client_ref: self.client_ref.as_deref(),
        }
    }

    pub fn for_client<'a>(&'a self, client_ref: Option<&'a str>) -> ApiResult<RequestScope<'a>> {
        validate_client_ref(client_ref)?;
        let mut scope = self.scope();
        if let (Some(bound), Some(requested)) = (scope.client_ref, client_ref) {
            if bound != requested {
                return Err(ApiError::NotFound("client not found".into()));
            }
        }
        scope.client_ref = scope.client_ref.or(client_ref);
        Ok(scope)
    }
}

pub async fn resolve(
    state: &AppState,
    caller: GatewayCaller,
    query: ScopeQuery,
    permission: Permission,
) -> ApiResult<Access> {
    let account = query
        .account_id
        .as_deref()
        .map(|id| parse_id(id, "account_id"))
        .transpose()?;
    let project = query
        .project_id
        .as_deref()
        .map(|id| parse_id(id, "project_id"))
        .transpose()?;
    let missing = || ApiError::NotFound("project not found".into());
    let denied = || ApiError::Forbidden(format!("requires {}", permission.as_str()));

    match caller {
        GatewayCaller::Project(key) => {
            if account.as_ref().is_some_and(|id| id != &key.account_id)
                || project.as_ref().is_some_and(|id| id != &key.project_id)
            {
                return Err(missing());
            }
            if !key.scopes.iter().any(|value| value == permission.as_str()) {
                return Err(denied());
            }
            let can_read_requests = key.scopes.iter().any(|scope| scope == Permission::ReadRequests.as_str());
            Ok(Access {
                can_read_requests,
                can_restore_content: false,
                account: key.account_id,
                project: key.project_id,
                actor_id: key.principal_id.as_str().to_owned(),
                client_ref: key.client_ref,
            })
        }
        GatewayCaller::Account(Caller::Session(auth)) => {
            let account =
                account.ok_or_else(|| ApiError::BadRequest("account_id is required".into()))?;
            let mut entries = session_projects(
                &state.pool,
                &auth,
                &account,
                project.as_ref(),
                project.is_none(),
            )
            .await?;
            let entry = entries.pop().ok_or_else(missing)?;
            if !role_allows(&entry.role, permission) {
                return Err(denied());
            }
            Ok(Access {
                account,
                project: entry.id,
                actor_id: auth.user.id.as_str().to_owned(),
                client_ref: entry.client_ref,
                can_read_requests: role_allows(&entry.role, Permission::ReadRequests),
                can_restore_content: entry.role == "admin",
            })
        }
        GatewayCaller::Account(Caller::ApiKey(key)) => {
            if permission == Permission::ReadExports {
                return Err(denied());
            }
            if account.as_ref().is_some_and(|id| id != &key.account_id) {
                return Err(missing());
            }
            gateway_projects::ensure_default(&state.pool, &key.account_id).await?;
            let mut entries = gateway_projects::list_projects(
                &state.pool,
                &key.account_id,
                ProjectFilter {
                    project: project.as_ref(),
                    default_only: true,
                    ..Default::default()
                },
            )
            .await?;
            let entry = entries.pop().ok_or_else(missing)?;
            Ok(Access {
                account: key.account_id,
                project: entry.id,
                actor_id: key.id.as_str().to_owned(),
                client_ref: None,
                can_read_requests: true,
                can_restore_content: false,
            })
        }
    }
}

pub async fn session_projects(
    pool: &db::PgPool,
    auth: &AuthContext,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
    default_only: bool,
) -> ApiResult<Vec<ProjectAccess>> {
    member_projects(pool, &auth.user.id, account, project, default_only).await
}

pub(super) fn role_allows(role: &str, permission: Permission) -> bool {
    match role {
        "admin" => true,
        "developer" => permission != Permission::ReadExports,
        "billing_viewer" => permission == Permission::ReadUsage,
        "client_viewer" => matches!(permission, Permission::ReadRequests | Permission::ReadUsage),
        _ => false,
    }
}

pub(super) async fn delegated(
    pool: &db::PgPool,
    grant: &db_queries::gateway_mcp::Grant,
    permission: Permission,
) -> ApiResult<Access> {
    if !grant.scopes.iter().any(|scope| scope == permission.as_str()) {
        return Err(ApiError::Forbidden(format!("requires {}", permission.as_str())));
    }
    let mut entries = member_projects(
        pool, &grant.user_id, &grant.account_id, Some(&grant.project_id), false,
    ).await?;
    let entry = entries.pop().ok_or_else(|| ApiError::NotFound("project not found".into()))?;
    if !role_allows(&entry.role, permission) {
        return Err(ApiError::Forbidden("current project role does not allow this action".into()));
    }
    if let (Some(granted), Some(current)) = (&grant.client_ref, &entry.client_ref) {
        if granted != current {
            return Err(ApiError::Forbidden("client scope changed; reconnect TokenCraft".into()));
        }
    }
    Ok(Access {
        account: grant.account_id.clone(),
        project: grant.project_id.clone(),
        actor_id: format!("mcp:{}:{}", grant.oauth_client_id, grant.user_id),
        client_ref: grant.client_ref.clone().or(entry.client_ref),
        can_restore_content: false,
        can_read_requests: role_allows(&entry.role, Permission::ReadRequests)
            && grant.scopes.iter().any(|scope| scope == Permission::ReadRequests.as_str()),
    })
}

async fn member_projects(
    pool: &db::PgPool,
    user: &UserId,
    account: &AccountId,
    project: Option<&GatewayProjectId>,
    default_only: bool,
) -> ApiResult<Vec<ProjectAccess>> {
    let membership = db_queries::members::find_membership(pool, account, user)
        .await?
        .ok_or_else(|| ApiError::NotFound("account not found".into()))?;
    if project.is_none() {
        gateway_projects::ensure_default(pool, account).await?;
    }
    Ok(gateway_projects::list_projects(
        pool,
        account,
        ProjectFilter {
            project,
            membership: Some(&membership.id),
            admin: MemberRole::ADMINS.contains(&membership.role),
            default_only,
        },
    )
    .await?)
}

pub fn parse_id<T: FromStr>(value: &str, name: &'static str) -> ApiResult<T> {
    value
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("invalid {name}")))
}
