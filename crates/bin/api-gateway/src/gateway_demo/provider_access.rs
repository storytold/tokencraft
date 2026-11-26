use std::{collections::BTreeSet, sync::Arc};

use axum::{
    extract::{Query, State},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use db_queries::gateway_routing_policies as storage;
use enums::{AuditEvent, MemberRole};
use ids::{AccountId, GatewayProjectId};
use serde::{Deserialize, Serialize};

use super::{
    access::ScopeQuery,
    catalog_refresh::CatalogStore,
    projects::{self, OrganizationQuery},
    routing_settings::{current, managed_target},
};
use crate::{
    authz::require_account_role,
    error::{ApiError, ApiResult},
    middleware::AuthContext,
    state::AppState,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CatalogAccess {
    pub account: AccountId,
    pub projects: Option<BTreeSet<GatewayProjectId>>,
}

pub(super) fn connection_id(provider: &str, adapter: &str, binding: Option<&str>) -> String {
    match binding {
        Some(binding) => format!("{provider}/{adapter}/{binding}"),
        None => provider.to_owned(),
    }
}

pub(super) async fn managed_account(
    state: &AppState,
    auth: &AuthContext,
    query: &OrganizationQuery,
) -> ApiResult<AccountId> {
    let account = query.account()?;
    require_account_role(&state.pool, &auth.user.id, &account, MemberRole::ADMINS).await?;
    Ok(account)
}

pub(super) async fn validate_projects(
    state: &AppState,
    account: &AccountId,
    selected: Option<&BTreeSet<GatewayProjectId>>,
) -> ApiResult<()> {
    let Some(selected) = selected else {
        return Ok(());
    };
    if selected.is_empty() || selected.len() > 64 {
        return Err(ApiError::BadRequest(
            "select between 1 and 64 projects".into(),
        ));
    }
    let project_ids: Vec<_> = selected.iter().map(|id| id.as_str().to_owned()).collect();
    let found: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM gateway_projects WHERE account_id = $1 AND id = ANY($2::text[])",
    )
    .bind(account.as_str())
    .bind(&project_ids)
    .fetch_one(&state.pool)
    .await?;
    if found != project_ids.len() as i64 {
        return Err(ApiError::BadRequest(
            "select projects in this organization".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Update {
    connection_id: String,
    expected_revision: i32,
    change: Change,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Change {
    Share {
        projects: Option<BTreeSet<GatewayProjectId>>,
    },
    Exclude {
        excluded: bool,
    },
}

#[derive(Serialize)]
pub(super) struct Saved {
    revision: i32,
}

pub(super) async fn update(
    State(state): State<AppState>,
    Extension(store): Extension<Arc<CatalogStore>>,
    auth: AuthContext,
    Query(query): Query<ScopeQuery>,
    Json(input): Json<Update>,
) -> ApiResult<Json<Saved>> {
    let (account, project) = managed_target(&state, &auth, query).await?;
    let catalog = store.current();
    if !catalog.offers.iter().any(|offer| {
        offer.connection_id() == input.connection_id && catalog.visible_to(offer, Some(&account))
    }) {
        return Err(ApiError::NotFound("provider connection".into()));
    }
    if let Change::Share { projects } = &input.change {
        validate_projects(&state, &account, projects.as_ref()).await?;
    }
    let mut tx = state.pool.begin().await?;
    storage::lock_for_update(&mut *tx, &account).await?;
    let mut prior = current(&mut *tx, &account, project.as_ref()).await?;
    if prior.revision != input.expected_revision {
        return Err(ApiError::Conflict(
            "routing policy changed; reload provider access".into(),
        ));
    }
    let changed = match (project.as_ref(), input.change) {
        (None, Change::Share { projects }) => {
            let current = prior
                .policy
                .connection_projects
                .get(&input.connection_id)
                .map(|value| value.as_ref())
                .unwrap_or_else(|| {
                    catalog
                        .connection_access
                        .get(&input.connection_id)
                        .and_then(|access| access.projects.as_ref())
                });
            let changed = current != projects.as_ref();
            if changed {
                prior
                    .policy
                    .connection_projects
                    .insert(input.connection_id, projects);
            }
            changed
        }
        (Some(_), Change::Exclude { excluded: true }) => prior
            .policy
            .excluded_connections
            .insert(input.connection_id),
        (Some(_), Change::Exclude { excluded: false }) => prior
            .policy
            .excluded_connections
            .remove(&input.connection_id),
        _ => {
            return Err(ApiError::BadRequest(
                "provider change requires the matching scope".into(),
            ))
        }
    };
    if !changed {
        return Ok(Json(Saved {
            revision: prior.revision,
        }));
    }
    let revision = prior
        .revision
        .checked_add(1)
        .ok_or_else(|| ApiError::Internal("routing revision is out of range".into()))?;
    let policy = serde_json::to_value(&prior.policy)
        .map_err(|_| ApiError::Internal("could not encode routing policy".into()))?;
    storage::insert(
        &mut *tx,
        &account,
        project.as_ref(),
        revision,
        &policy,
        &auth.user.id,
    )
    .await?;
    projects::audit(
        &mut tx,
        &account,
        &auth.user.id,
        AuditEvent::GatewayRoutingPolicyChanged,
        "gateway_routing_policy",
        project
            .as_ref()
            .map_or(account.as_str(), GatewayProjectId::as_str),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(Saved { revision }))
}

#[derive(Serialize, sqlx::FromRow)]
pub(super) struct HistoryEntry {
    revision: i32,
    created_at: DateTime<Utc>,
    created_by: String,
    policy: serde_json::Value,
}

pub(super) async fn history(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<Vec<HistoryEntry>>> {
    let (account, project) = managed_target(&state, &auth, query).await?;
    let rows = sqlx::query_as::<_, HistoryEntry>(
        "SELECT revision, created_at, created_by, policy FROM gateway_routing_policies
         WHERE account_id = $1 AND project_id IS NOT DISTINCT FROM $2::text
         ORDER BY revision DESC LIMIT 20",
    )
    .bind(account.as_str())
    .bind(project.as_ref().map(GatewayProjectId::as_str))
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_demo::{catalog::Catalog, routing_policy::Policies};
    use db_queries::gateway_requests::RequestScope;

    #[test]
    fn provider_access_combines_ownership_sharing_and_exclusions() {
        let mut catalog = Catalog::parse(include_bytes!(
            "../../../../../examples/connector/catalog.json"
        ))
        .unwrap();
        let connection = catalog.offers[0].connection_id();
        let account = AccountId::generate();
        let other_account = AccountId::generate();
        let first = GatewayProjectId::generate();
        let second = GatewayProjectId::generate();
        let first_scope = RequestScope {
            account: &account,
            project: &first,
            client_ref: None,
        };
        let second_scope = RequestScope {
            account: &account,
            project: &second,
            client_ref: None,
        };
        let outside_scope = RequestScope {
            account: &other_account,
            ..first_scope
        };
        let mut policies = Policies::default();

        assert!(policies.allows_offer(&catalog, &catalog.offers[0], first_scope));
        assert!(policies.allows_offer(&catalog, &catalog.offers[0], second_scope));
        catalog.connection_access.insert(
            connection.clone(),
            CatalogAccess {
                account: account.clone(),
                projects: Some(BTreeSet::from([first.clone()])),
            },
        );
        let offer = &catalog.offers[0];
        assert!(!catalog.visible_to(offer, None));
        assert!(catalog.visible_to(offer, Some(&account)));
        assert!(!catalog.visible_to(offer, Some(&other_account)));
        assert!(policies.allows_offer(&catalog, offer, first_scope));
        assert!(!policies.allows_offer(&catalog, offer, second_scope));
        assert!(!policies.allows_offer(&catalog, offer, outside_scope));

        policies
            .organization
            .policy
            .connection_projects
            .insert(connection.clone(), None);
        assert!(policies.allows_offer(&catalog, offer, second_scope));
        assert!(!policies.allows_offer(&catalog, offer, outside_scope));
        policies
            .project
            .policy
            .excluded_connections
            .insert(connection.clone());
        assert!(!policies.allows_offer(&catalog, offer, first_scope));
        policies
            .project
            .policy
            .excluded_connections
            .remove(&connection);
        assert!(policies.allows_offer(&catalog, offer, first_scope));
    }

    #[test]
    fn model_catalog_only_advertises_providers_available_to_scope() {
        use serde_json::{json, to_value};

        let mut catalog = Catalog::parse(include_bytes!(
            "../../../../../examples/connector/catalog.json"
        ))
        .unwrap();
        let account = AccountId::generate();
        let project = GatewayProjectId::generate();
        let sibling = GatewayProjectId::generate();
        let scope = RequestScope {
            account: &account,
            project: &project,
            client_ref: None,
        };
        let mut private = catalog.offers[0].clone();
        private.id = "private_image".into();
        private.model = "private_image".into();
        private.provider = "private-connector".parse().unwrap();
        private.local_binding = Some("private".into());
        let connection = private.connection_id();
        catalog.offers.push(private);
        catalog.connection_access.insert(
            connection.clone(),
            CatalogAccess { account: account.clone(), projects: Some(BTreeSet::from([project.clone()])) },
        );

        let public = to_value(catalog.model_catalog(None)).unwrap();
        assert!(public.get("providers").is_none());
        assert_eq!(public["models"].as_array().unwrap().len(), 1);

        let mut policies = Policies::default();
        let providers = |policies: &Policies| to_value(catalog.model_catalog(Some((policies, scope)))).unwrap();
        assert_eq!(providers(&policies)["providers"], json!(["private-connector", "self_hosted"]));
        let sibling_scope = RequestScope { project: &sibling, ..scope };
        let unshared = to_value(catalog.model_catalog(Some((&policies, sibling_scope)))).unwrap();
        assert_eq!(unshared["providers"], json!(["self_hosted"]));
        assert_eq!(unshared["models"], public["models"]);

        policies.project.policy.excluded_connections.insert(connection);
        assert_eq!(providers(&policies)["providers"], json!(["self_hosted"]));
        policies.project.policy = serde_json::from_value(json!({ "allowed_providers": [] })).unwrap();
        let denied = providers(&policies);
        assert_eq!(denied["providers"], json!([]));
        assert_eq!(denied["models"], json!([]));
    }

    #[test]
    fn scoped_quotes_remove_an_excluded_connection_from_selection_and_offer_pins() {
        use crate::gateway_demo::{
            availability::RouteAvailability,
            quote::{QuoteInput, QuoteTarget, Reason},
        };

        let mut catalog = Catalog::parse(include_bytes!(
            "../../../../../examples/connector/catalog.json"
        ))
        .unwrap();
        let mut fallback = catalog.offers[0].clone();
        fallback.id = "fallback_image".into();
        fallback.local_binding = Some("fallback".into());
        catalog.offers.push(fallback);
        let account = AccountId::generate();
        let project = GatewayProjectId::generate();
        let scope = RequestScope {
            account: &account,
            project: &project,
            client_ref: None,
        };
        let mut policies = Policies::default();
        policies.project.revision = 1;
        policies
            .project
            .policy
            .excluded_connections
            .insert(catalog.offers[0].connection_id());
        let input = || -> QuoteInput {
            serde_json::from_value(serde_json::json!({
                "model": "example_image",
                "provider": "self_hosted",
                "workload": { "kind": "image", "count": 1, "aspect_ratio": "1:1" }
            }))
            .unwrap()
        };
        let availability = RouteAvailability::default();
        let quote = catalog
            .scoped_quote(input(), scope, &policies, &availability, QuoteTarget::Any)
            .unwrap();
        assert_eq!(quote.selected_offer_id.as_deref(), Some("fallback_image"));
        assert_eq!(quote.routing.unwrap().project_revision, 1);
        let pinned = catalog
            .scoped_quote(
                input(),
                scope,
                &policies,
                &availability,
                QuoteTarget::Offer("example_image"),
            )
            .unwrap();
        assert!(pinned.selected_offer_id.is_none());
        assert_eq!(pinned.candidates[0].reason, Reason::ProviderPolicyExcluded);

        policies.organization.policy.connection_projects.insert(
            catalog.offers[0].connection_id(),
            Some(BTreeSet::from([GatewayProjectId::generate()])),
        );
        let unshared = catalog
            .scoped_quote(input(), scope, &policies, &availability, QuoteTarget::Any)
            .unwrap();
        assert_eq!(unshared.candidates.len(), 1);
        assert_eq!(unshared.candidates[0].offer_id, "fallback_image");
        assert!(matches!(
            catalog.scoped_quote(input(), scope, &policies, &availability, QuoteTarget::Offer("example_image")),
            Err(ApiError::NotFound(_))
        ));
    }
}
