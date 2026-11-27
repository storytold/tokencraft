use std::sync::Arc;

use anyhow::{Context, ensure};
use axum::{Extension, Json, extract::{Path, Query, State}, http::{HeaderValue, StatusCode}};
use chrono::{DateTime, Utc};
use db_queries::{gateway_provider_credentials as db_keys, gateway_requests::RequestScope, gateway_routing_policies};
use enums::{AuditEvent, MemberRole};
use ids::{AccountId, GatewayProjectId};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::{authz::require_account_role, error::{ApiError, ApiResult}, middleware::AuthContext, state::AppState};

use super::{
    access::parse_id, catalog::{valid_identifier, Adapter, Catalog}, catalog_refresh::CatalogStore,
    projects::{OrganizationQuery, audit, managed_project}, providers::ProviderRuntime,
    routing_policy::Policies, sealed::SealedBytes,
};

pub(super) struct Vault(SealedBytes);

impl Vault {
    pub(super) fn load<'a>(request_keys: impl IntoIterator<Item = &'a [u8; 32]>) -> anyhow::Result<Self> {
        let raw = std::env::var("TOKENCRAFT_PROVIDER_CREDENTIAL_KEY")
            .context("set TOKENCRAFT_PROVIDER_CREDENTIAL_KEY for tenant bindings")?;
        let mut key = [0u8; 32];
        ensure!(hex::decode_to_slice(raw, &mut key).is_ok() && key.iter().any(|byte| *byte != key[0]),
            "TOKENCRAFT_PROVIDER_CREDENTIAL_KEY must be a generated 32-byte hex key");
        ensure!(request_keys.into_iter().all(|root| root != &key),
            "provider credential and request content keys must be independent");
        Ok(Self(SealedBytes::new(&key, b"tokencraft/chat-connector-credentials/aes256/v1")))
    }

    pub(super) async fn pinned(&self, pool: &db::PgPool, request: &str, binding: &str)
        -> sqlx::Result<Option<HeaderValue>>
    {
        let Some(row) = db_keys::pinned(pool, request, binding).await? else { return Ok(None); };
        let aad = credential_context(&row.adapter, &row.account_id, &row.scope_id, &row.binding_slot, row.version, binding);
        let key = self.0.open(&aad, &row.encrypted_key).and_then(|mut plaintext| {
            let header = HeaderValue::from_bytes(&plaintext);
            plaintext.fill(0);
            let mut value = header.ok()?;
            value.set_sensitive(true);
            Some(value)
        });
        Ok(key)
    }
}

fn associated_data(account: &str, scope: &str, slot: &str, version: i32, binding: &str) -> String {
    format!("{account}/{scope}/{slot}/{version}/{binding}")
}

fn credential_context(adapter: &str, account: &str, scope: &str, slot: &str, version: i32, binding: &str) -> String {
    let context = associated_data(account, scope, slot, version, binding);
    // Chat versions sealed before media support keep their original associated data.
    match adapter {
        "connector_queue" => format!("connector_queue/{context}"),
        _ => context,
    }
}

fn valid_key(key: &str) -> bool {
    (16..=128).contains(&key.len()) && key.bytes().all(|byte| byte.is_ascii_alphanumeric()
        || matches!(byte, b'_' | b'-' | b'.' | b':' | b'~' | b'+' | b'/' | b'='))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct CredentialQuery {
    account_id: String,
    project_id: Option<String>,
    adapter: Option<String>,
}

impl CredentialQuery {
    async fn scope(self, state: &AppState, auth: &AuthContext)
        -> ApiResult<(AccountId, Option<GatewayProjectId>)>
    {
        if let Some(project) = self.project_id {
            let (account, project) = managed_project(state, auth, &OrganizationQuery::new(self.account_id), &project).await?;
            return Ok((account, Some(project.id)));
        }
        let account: AccountId = parse_id(&self.account_id, "account_id")?;
        require_account_role(&state.pool, &auth.user.id, &account, MemberRole::ADMINS).await?;
        Ok((account, None))
    }
}

fn resolve_binding<'a>(
    runtime: Option<&'a ProviderRuntime>, binding: &str, requested: Option<&str>,
) -> ApiResult<(&'static str, Option<&'a str>)> {
    let configured = runtime.and_then(|runtime| runtime.tenant_binding(binding));
    let adapter = match requested {
        Some("openai_compatible_chat") => Adapter::OpenAiCompatibleChat,
        Some("connector_queue") => Adapter::ConnectorQueue,
        Some(_) => return Err(ApiError::BadRequest("unknown provider credential adapter".into())),
        None => configured.map_or(Adapter::OpenAiCompatibleChat, |(adapter, _)| adapter),
    };
    Ok((adapter.as_str(), configured.filter(|(kind, _)| *kind == adapter).map(|(_, digest)| digest)))
}

async fn can_use_binding(
    connection: &mut sqlx::PgConnection,
    catalog: &Catalog,
    slot: &db_keys::Scope<'_>,
) -> ApiResult<bool> {
    let connections: Vec<_> = catalog.offers.iter().filter(|offer| {
        offer.local_binding.as_deref() == Some(slot.binding)
            && offer.adapter.as_str() == slot.adapter
            && catalog.visible_to(offer, Some(slot.account))
    }).map(|offer| offer.connection_id()).collect();
    if connections.is_empty() {
        return Ok(false);
    }
    let Some(project) = slot.project else {
        return Ok(true);
    };
    let scope = RequestScope { account: slot.account, project, client_ref: None };
    let policies = Policies::load(connection, scope).await?;
    Ok(connections.iter().any(|connection| policies.allows_connection(catalog, connection, scope)))
}

#[derive(Serialize, ToSchema)]
pub(super) struct CredentialList {
    enabled: bool,
    versions: Vec<CredentialVersion>,
    organization_default: Option<CredentialVersion>,
}

#[derive(Serialize, ToSchema)]
struct CredentialVersion {
    version: i32,
    created_at: DateTime<Utc>,
    status: &'static str,
}

fn active(row: &db_keys::Version, binding: Option<&str>) -> bool {
    binding == Some(row.binding_digest.as_str()) && row.has_key
        && row.revoked_at.is_none() && row.retired_at.is_none()
}

impl CredentialVersion {
    fn metadata(row: db_keys::Version, digest: Option<&str>) -> Self {
        let status = match (row.revoked_at, row.retired_at, row.has_key) {
            (Some(_), _, _) => "revoked",
            (_, Some(_), true) => "retiring",
            (_, Some(_), false) => "retired",
            _ if row.inherits_organization => "inheriting",
            _ if active(&row, digest) => "active",
            _ => "unavailable",
        };
        Self { version: row.version, created_at: row.created_at, status }
    }
}

fn validate_binding(binding: &str) -> ApiResult<()> {
    if !valid_identifier(binding) {
        return Err(ApiError::BadRequest("invalid provider binding name".into()));
    }
    Ok(())
}

#[utoipa::path(get, path = "/v1/provider-credentials/{binding}", tag = "native",
    security(("session_cookie" = [])),
    params(("binding" = String, Path, description = "Catalog connector binding name."), CredentialQuery),
    responses((status = 200, description = "Key metadata for the requested scope.", body = CredentialList)))]
pub(super) async fn list(
    State(state): State<AppState>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    auth: AuthContext,
    Query(query): Query<CredentialQuery>,
    Path(binding): Path<String>,
) -> ApiResult<Json<CredentialList>> {
    let requested = query.adapter.clone();
    let (account, project) = query.scope(&state, &auth).await?;
    validate_binding(&binding)?;
    let (adapter, digest) = resolve_binding(runtime.as_deref(), &binding, requested.as_deref())?;
    let scope = db_keys::Scope { account: &account, project: project.as_ref(), binding: &binding, adapter };
    let mut connection = state.pool.acquire().await?;
    let digest = if digest.is_some() && can_use_binding(&mut connection, &catalog.current(), &scope).await? {
        digest
    } else {
        None
    };
    let versions = db_keys::versions(&mut connection, scope, 25).await?.into_iter()
        .map(|row| CredentialVersion::metadata(row, digest)).collect();
    let organization_default = if project.is_some() && digest.is_some() {
        let scope = db_keys::Scope { account: &account, project: None, binding: &binding, adapter };
        db_keys::versions(&mut connection, scope, 1).await?.pop()
            .map(|row| CredentialVersion::metadata(row, digest))
    } else {
        None
    };
    Ok(Json(CredentialList { enabled: digest.is_some(), versions, organization_default }))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveKey {
    expected_version: i32,
    #[schema(write_only)]
    key: Option<String>,
    #[serde(default)]
    inherit: bool,
}

#[utoipa::path(put, path = "/v1/provider-credentials/{binding}", tag = "native",
    security(("session_cookie" = [])),
    params(("binding" = String, Path, description = "Catalog connector binding name."), CredentialQuery),
    request_body = SaveKey,
    responses((status = 204, description = "A new version was saved."),
              (status = 409, description = "The expected version or binding is unavailable.")))]
pub(super) async fn save(
    State(state): State<AppState>,
    Extension(catalog): Extension<Arc<CatalogStore>>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    auth: AuthContext,
    Query(query): Query<CredentialQuery>,
    Path(binding): Path<String>,
    Json(input): Json<SaveKey>,
) -> ApiResult<StatusCode> {
    let requested = query.adapter.clone();
    let (account, project) = query.scope(&state, &auth).await?;
    validate_binding(&binding)?;
    let (adapter, digest) = resolve_binding(runtime.as_deref(), &binding, requested.as_deref())?;
    let vault = runtime.as_deref().and_then(|runtime| runtime.vault.as_ref())
        .ok_or(ApiError::ProviderCredentialUnavailable)?;
    let digest = digest.ok_or(ApiError::ProviderCredentialUnavailable)?;
    if input.expected_version < 0 || input.key.is_some() == input.inherit
        || (input.inherit && project.is_none())
    {
        return Err(ApiError::BadRequest("supply a key or select organization inheritance for a project".into()));
    }
    if input.key.as_deref().is_some_and(|key| !valid_key(key)) {
        return Err(ApiError::BadRequest("invalid provider bearer key format".into()));
    }
    let scope = || db_keys::Scope { account: &account, project: project.as_ref(), binding: &binding, adapter };
    let mut tx = state.pool.begin().await?;
    gateway_routing_policies::lock_for_update(&mut *tx, &account).await?;
    if !can_use_binding(&mut tx, &catalog.current(), &scope()).await? {
        return Err(ApiError::ProviderCredentialUnavailable);
    }
    let latest = db_keys::versions(&mut tx, scope(), 1).await?.pop().map_or(0, |row| row.version);
    if latest != input.expected_version {
        return Err(ApiError::Conflict("the provider key version changed; reload and try again".into()));
    }
    let version = latest.checked_add(1)
        .ok_or_else(|| ApiError::Conflict("provider key version limit reached".into()))?;
    let encrypted = if let Some(key) = input.key {
        let aad = credential_context(adapter, account.as_str(), scope().id(), &binding, version, digest);
        Some(vault.0.seal(&aad, key.into_bytes())
            .ok_or_else(|| ApiError::Internal("could not seal provider key".into()))?)
    } else {
        let organization = db_keys::Scope { account: &account, project: None, binding: &binding, adapter };
        if !db_keys::versions(&mut tx, organization, 1).await?.pop().is_some_and(|row| active(&row, Some(digest))) {
            return Err(ApiError::Conflict("save an active organization key before enabling inheritance".into()));
        }
        None
    };
    db_keys::save(&mut tx, scope(), version, digest, encrypted.as_deref()).await?;
    audit(&mut tx, &account, &auth.user.id, AuditEvent::GatewayProviderCredentialSaved,
        "gateway_provider_credential", &format!("{}/{binding}/{version}", scope().id())).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/provider-credentials/{binding}/{version}", tag = "native",
    security(("session_cookie" = [])),
    params(("binding" = String, Path, description = "Catalog connector binding name."),
           ("version" = i32, Path, description = "Key version."), CredentialQuery),
    responses((status = 204, description = "The specified key version is unavailable for dispatch.")))]
pub(super) async fn revoke(
    State(state): State<AppState>,
    Extension(runtime): Extension<Option<Arc<ProviderRuntime>>>,
    auth: AuthContext,
    Query(query): Query<CredentialQuery>,
    Path((binding, version)): Path<(String, i32)>,
) -> ApiResult<StatusCode> {
    let requested = query.adapter.clone();
    let (account, project) = query.scope(&state, &auth).await?;
    validate_binding(&binding)?;
    let (adapter, _) = resolve_binding(runtime.as_deref(), &binding, requested.as_deref())?;
    if version <= 0 {
        return Err(ApiError::BadRequest("supply a positive credential version".into()));
    }
    let scope = db_keys::Scope { account: &account, project: project.as_ref(), binding: &binding, adapter };
    let target = format!("{}/{binding}/{version}", scope.id());
    let mut tx = state.pool.begin().await?;
    gateway_routing_policies::lock_for_update(&mut *tx, &account).await?;
    if db_keys::revoke(&mut tx, scope, version).await? {
        audit(&mut tx, &account, &auth.user.id, AuditEvent::GatewayProviderCredentialRevoked,
            "gateway_provider_credential", &target).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ciphertext_is_bound_to_organization_scope_slot_version_and_binding() {
        let vault = Vault(SealedBytes::new(&[21; 32], b"tokencraft/chat-connector-credentials/aes256/v1"));
        let key = "fixture-bearer";
        let original = associated_data("account", "project", "router", 2, "binding");
        let ciphertext = vault.0.seal(&original, key.as_bytes().to_vec()).unwrap();
        assert_eq!(vault.0.open(&original, &ciphertext).unwrap(), key.as_bytes());
        for wrong in [
            associated_data("sibling", "project", "router", 2, "binding"),
            associated_data("account", "account", "router", 2, "binding"),
            associated_data("account", "project", "other", 2, "binding"),
            associated_data("account", "project", "router", 3, "binding"),
            associated_data("account", "project", "router", 2, "retired-binding"),
            credential_context("connector_queue", "account", "project", "router", 2, "binding"),
        ] {
            assert!(vault.0.open(&wrong, &ciphertext).is_none());
        }
    }
}
