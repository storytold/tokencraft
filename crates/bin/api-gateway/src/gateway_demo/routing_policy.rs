use std::collections::{BTreeMap, BTreeSet};

use db_queries::{gateway_requests::RequestScope, gateway_routing_policies};
use serde::{Deserialize, Serialize};

use super::{
    catalog::{Catalog, Pricing, Provider},
    data_handling::DataRequirements,
    quote::{QuoteInput, Workload},
    request_capacity::ActiveRequestLimits,
};
use crate::error::{ApiError, ApiResult};

pub(crate) const PROVIDER_PREFERENCE_HEADER: &str = "x-tokencraft-preferred-providers";
pub(super) const MAX_TOLERANCE_BPS: u16 = 5000;
pub(super) const MAX_COST_NANOS: i64 = 1_000_000_000_000;
pub(super) const MAX_MONTHLY_BUDGET_NANOS: i64 = 1_000_000_000_000_000;

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    defaults: Bounds,
    limits: Bounds,
    offer_overrides: BTreeMap<String, OfferLimit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_providers: Option<BTreeSet<Provider>>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) excluded_connections: BTreeSet<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) compute_order: BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) connection_projects: BTreeMap<String, Option<BTreeSet<ids::GatewayProjectId>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_providers: Option<Vec<Provider>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route_ranking: Option<RouteRanking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_quality: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    advisor_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_usage_accounts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) monthly_estimated_budget_usd_nanos: Option<i64>,
    #[serde(skip_serializing_if = "ActiveRequestLimits::is_empty")]
    pub(super) active_request_limits: ActiveRequestLimits,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_requirements: Option<DataRequirements>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteRanking {
    Price,
    RecentReliability,
    CostPerCompletion,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Bounds {
    max_relative_error_bps: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video_max_relative_error_bps: Option<u16>,
    max_total_usd_nanos: Option<i64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OfferLimit {
    max_relative_error_bps: u16,
}

impl Policy {
    pub(super) fn validate(&mut self, catalog: &Catalog) -> ApiResult<()> {
        validate_provider_order(self.preferred_providers.as_deref())?;
        validate_model_floors(self.min_quality, self.min_usage_accounts)?;
        if let Some(data) = &mut self.data_requirements {
            data.normalize().map_err(|message| ApiError::BadRequest(message.into()))?;
            if data.is_empty() {
                self.data_requirements = None;
            }
        }
        if self
            .monthly_estimated_budget_usd_nanos
            .is_some_and(|limit| !(0..=MAX_MONTHLY_BUDGET_NANOS).contains(&limit))
        {
            return Err(ApiError::BadRequest(
                "monthly estimated budget must be between $0 and $1,000,000".into(),
            ));
        }
        for bound in [&self.defaults, &self.limits] {
            if bound
                .max_relative_error_bps
                .is_some_and(|value| value > MAX_TOLERANCE_BPS)
                || bound.video_max_relative_error_bps.is_some_and(|value| value > MAX_TOLERANCE_BPS)
                || bound
                    .max_total_usd_nanos
                    .is_some_and(|value| !(0..=MAX_COST_NANOS).contains(&value))
            {
                return Err(ApiError::BadRequest(
                    "policy tolerance or cost is out of range".into(),
                ));
            }
        }
        if self.offer_overrides.len() > 32 {
            return Err(ApiError::BadRequest(
                "policy supports up to 32 offer overrides".into(),
            ));
        }
        for (id, limit) in &self.offer_overrides {
            if limit.max_relative_error_bps > MAX_TOLERANCE_BPS
                || !catalog
                    .offers
                    .iter()
                    .any(|offer| {
                        &offer.id == id
                            && matches!(offer.pricing, Pricing::Image { .. } | Pricing::Video { .. })
                    })
            {
                return Err(ApiError::BadRequest(
                    "offer override requires a catalog image or video offer and a valid tolerance".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Default, Serialize)]
pub struct Revision {
    pub revision: i32,
    pub policy: Policy,
}

#[derive(Default)]
pub(super) struct Policies {
    pub organization: Revision,
    pub project: Revision,
}

impl Policies {
    pub(super) fn connection_projects<'a>(
        &'a self, catalog: &'a Catalog, id: &str,
    ) -> Option<&'a BTreeSet<ids::GatewayProjectId>> {
        match self.organization.policy.connection_projects.get(id) {
            Some(projects) => projects.as_ref(),
            None => catalog.connection_access.get(id).and_then(|access| access.projects.as_ref()),
        }
    }

    pub(super) fn shares_connection(
        &self, catalog: &Catalog, id: &str, scope: RequestScope<'_>,
    ) -> bool {
        catalog.connection_access.get(id).is_none_or(|access| &access.account == scope.account)
            && self.connection_projects(catalog, id).is_none_or(|projects| projects.contains(scope.project))
    }

    pub(super) fn allows_connection(
        &self, catalog: &Catalog, id: &str, scope: RequestScope<'_>,
    ) -> bool {
        self.shares_connection(catalog, id, scope)
            && !self.project.policy.excluded_connections.contains(id)
    }

    pub(super) fn allows_offer(&self, catalog: &Catalog, offer: &super::catalog::Offer, scope: RequestScope<'_>) -> bool {
        self.allows_provider(&offer.provider)
            && self.allows_connection(catalog, &offer.connection_id(), scope)
    }

    pub fn allows_provider(&self, provider: &Provider) -> bool {
        [&self.organization.policy, &self.project.policy].into_iter().all(|policy| {
            policy.allowed_providers.as_ref().is_none_or(|allowed| allowed.contains(provider))
        })
    }

    pub async fn load<'e>(
        exec: impl sqlx::PgExecutor<'e>,
        scope: RequestScope<'_>,
    ) -> ApiResult<Self> {
        let rows =
            gateway_routing_policies::current(exec, scope.account, Some(scope.project)).await?;
        let mut policies = Self::default();
        for row in rows {
            policies.set_current(row)?;
        }
        Ok(policies)
    }

    pub(super) fn set_current(&mut self, row: gateway_routing_policies::PolicyRow) -> ApiResult<()> {
        let target = if row.project_id.is_some() { &mut self.project } else { &mut self.organization };
        *target = Revision::from_row(row)?;
        Ok(())
    }

    pub(super) fn advisor_enabled(&self) -> bool {
        self.project.policy.advisor_enabled
            .or(self.organization.policy.advisor_enabled)
            .unwrap_or(false)
    }

    pub(super) fn has_revisions(&self) -> bool {
        self.organization.revision != 0 || self.project.revision != 0
    }

    pub(super) fn data_requirements(&self, requested: Option<&DataRequirements>) -> DataRequirements {
        let mut effective = requested.cloned().unwrap_or_default();
        for revision in [&self.organization, &self.project] {
            if let Some(requirements) = &revision.policy.data_requirements {
                effective.intersect(requirements);
            }
        }
        effective
    }

    pub fn resolution(&self, input: &QuoteInput) -> Option<Resolution> {
        if !input.is_auto() && !self.has_revisions() && input.preferred_providers.is_none() && input.data_requirements.is_none() {
            return None;
        }
        let data_requirements = self.data_requirements(input.data_requirements.as_ref());
        Some(Resolution {
            data_requirements: (!data_requirements.is_empty()).then_some(data_requirements),
            organization_revision: self.organization.revision,
            project_revision: self.project.revision,
            provider_limits: self.provider_limits(),
            provider_preference: self.provider_preference(input),
            route_ranking: self.route_ranking(input),
            min_quality: self.model_floor(input, input.min_quality, |policy| policy.min_quality),
            min_usage_accounts: self.model_floor(input, input.min_usage_accounts, |policy| policy.min_usage_accounts),
            cost_cap: self.resolve_field(input.max_total_usd_nanos, None, |bounds| {
                bounds.max_total_usd_nanos
            }),
            aspect_ratio_tolerance: match &input.workload {
                Workload::Image {
                    max_relative_error_bps,
                    ..
                } => Some(
                    self.resolve_field(*max_relative_error_bps, Some(0), |bounds| {
                        bounds.max_relative_error_bps
                    })
                    .unwrap_or(Setting::new(0, Source::ModelDefault)),
                ),
                Workload::Video { max_relative_error_bps, .. }
                    if max_relative_error_bps.is_some()
                        || self.project.policy.defaults.video_max_relative_error_bps.is_some()
                        || self.organization.policy.defaults.video_max_relative_error_bps.is_some() =>
                {
                    self.resolve_field(*max_relative_error_bps, Some(0), |bounds| {
                        bounds.video_max_relative_error_bps
                    })
                }
                _ => None,
            },
        })
    }

    fn model_floor<T: Copy>(
        &self,
        input: &QuoteInput,
        requested: Option<T>,
        field: impl Fn(&Policy) -> Option<T>,
    ) -> Option<Setting<T>> {
        if !input.is_auto() {
            return None;
        }
        [
            (requested, Source::Request),
            (field(&self.project.policy), Source::ProjectDefault),
            (field(&self.organization.policy), Source::OrganizationDefault),
        ]
        .into_iter()
        .find_map(|(value, source)| value.map(|value| Setting::new(value, source)))
    }

    fn provider_preference(&self, input: &QuoteInput) -> Option<Setting<Vec<Provider>>> {
        [
            (input.preferred_providers.as_ref(), Source::Request),
            (self.project.policy.preferred_providers.as_ref(), Source::ProjectDefault),
            (self.organization.policy.preferred_providers.as_ref(), Source::OrganizationDefault),
        ].into_iter().find_map(|(order, source)| order.map(|order| Setting::new(order.clone(), source)))
    }

    fn route_ranking(&self, input: &QuoteInput) -> Option<Setting<RouteRanking>> {
        let configured = self.project.policy.route_ranking
            .map(|value| Setting::new(value, Source::ProjectDefault))
            .or_else(|| self.organization.policy.route_ranking
                .map(|value| Setting::new(value, Source::OrganizationDefault)))?;
        if input.preferred_providers.as_ref().is_some_and(Vec::is_empty) {
            return Some(Setting::new(RouteRanking::Price, Source::Request));
        }
        Some(configured)
    }

    fn provider_limits(&self) -> Option<ProviderLimits> {
        let organization = self.organization.policy.allowed_providers.is_some()
            || !self.organization.policy.connection_projects.is_empty();
        let project = self.project.policy.allowed_providers.is_some()
            || !self.project.policy.excluded_connections.is_empty();
        (organization || project).then_some(ProviderLimits { organization, project })
    }

    fn resolve_field<T: Copy + Ord>(
        &self,
        request: Option<T>,
        baseline: Option<T>,
        field: impl Fn(&Bounds) -> Option<T>,
    ) -> Option<Setting<T>> {
        let organization = &self.organization.policy;
        let project = &self.project.policy;
        let mut setting = request
            .map(|value| Setting::new(value, Source::Request))
            .or_else(|| {
                field(&project.defaults).map(|value| Setting::new(value, Source::ProjectDefault))
            })
            .or_else(|| {
                field(&organization.defaults)
                    .map(|value| Setting::new(value, Source::OrganizationDefault))
            })
            .or_else(|| baseline.map(|value| Setting::new(value, Source::ModelDefault)));
        tighten(
            &mut setting,
            field(&organization.limits),
            Source::OrganizationLimit,
        );
        tighten(&mut setting, field(&project.limits), Source::ProjectLimit);
        setting
    }

    pub fn for_offer(&self, id: &str, base: Option<Setting<u16>>) -> Option<Setting<u16>> {
        let mut setting = base;
        if setting.is_some() {
            tighten(
                &mut setting,
                self.organization
                    .policy
                    .offer_overrides
                    .get(id)
                    .map(|item| item.max_relative_error_bps),
                Source::OrganizationOffer,
            );
            tighten(
                &mut setting,
                self.project
                    .policy
                    .offer_overrides
                    .get(id)
                    .map(|item| item.max_relative_error_bps),
                Source::ProjectOffer,
            );
        }
        setting
    }
}

impl Revision {
    pub(super) fn from_row(row: gateway_routing_policies::PolicyRow) -> ApiResult<Self> {
        let policy = serde_json::from_value(row.policy)
            .map_err(|_| ApiError::Internal("stored routing policy is invalid".into()))?;
        Ok(Self {
            revision: row.revision,
            policy,
        })
    }

    pub(super) fn next(&self, policy: Policy) -> ApiResult<Self> {
        Ok(Self {
            revision: self.revision.checked_add(1).ok_or_else(|| {
                ApiError::Conflict("routing policy has reached its revision limit".into())
            })?,
            policy,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Request,
    ModelDefault,
    OrganizationDefault,
    ProjectDefault,
    OrganizationLimit,
    ProjectLimit,
    OrganizationOffer,
    ProjectOffer,
}

impl Source {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Request => "Request",
            Self::ModelDefault => "Model default",
            Self::OrganizationDefault => "Organization default",
            Self::ProjectDefault => "Project default",
            Self::OrganizationLimit => "Organization limit",
            Self::ProjectLimit => "Project limit",
            Self::OrganizationOffer => "Organization offer limit",
            Self::ProjectOffer => "Project offer limit",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct Setting<T> {
    pub value: T,
    pub source: Source,
}

impl<T> Setting<T> {
    fn new(value: T, source: Source) -> Self {
        Self { value, source }
    }
}

fn tighten<T: Copy + Ord>(setting: &mut Option<Setting<T>>, limit: Option<T>, source: Source) {
    if let Some(value) = limit.filter(|limit| setting.is_none_or(|current| *limit < current.value))
    {
        *setting = Some(Setting::new(value, source));
    }
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct Resolution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_requirements: Option<DataRequirements>,
    pub organization_revision: i32,
    pub project_revision: i32,
    #[schema(inline)]
    pub cost_cap: Option<Setting<i64>>,
    #[schema(inline)]
    pub aspect_ratio_tolerance: Option<Setting<u16>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_limits: Option<ProviderLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(inline)]
    pub provider_preference: Option<Setting<Vec<Provider>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(inline)]
    pub route_ranking: Option<Setting<RouteRanking>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(inline)]
    pub min_quality: Option<Setting<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(inline)]
    pub min_usage_accounts: Option<Setting<u32>>,
}

pub(super) fn validate_model_floors(quality: Option<u8>, usage: Option<u32>) -> ApiResult<()> {
    if quality.is_some_and(|value| !(1..=10).contains(&value)) {
        return Err(ApiError::BadRequest("min_quality must be an integer from 1 to 10".into()));
    }
    if usage.is_some_and(|value| !(5..=10_000).contains(&value)) {
        return Err(ApiError::BadRequest("min_usage_accounts must be an integer from 5 to 10000".into()));
    }
    Ok(())
}

pub(super) fn validate_provider_order(order: Option<&[Provider]>) -> ApiResult<()> {
    if order.is_some_and(|order| order.iter().enumerate().any(|(index, provider)| order[..index].contains(provider))) {
        return Err(ApiError::BadRequest("preferred_providers must contain unique providers".into()));
    }
    Ok(())
}

pub(super) fn parse_provider_order(value: &str) -> ApiResult<Vec<Provider>> {
    if value.trim() == "cheapest" {
        return Ok(Vec::new());
    }
    let order = value.split(',').map(|part| part.trim().parse()).collect::<Result<Vec<_>, _>>()
        .map_err(|error: &'static str| ApiError::BadRequest(error.into()))?;
    validate_provider_order(Some(&order))?;
    Ok(order)
}

#[derive(Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct ProviderLimits {
    pub organization: bool,
    pub project: bool,
}

#[cfg(test)]
mod advisor_tests {
    use super::*;

    #[test]
    fn advisor_is_opt_in_and_project_can_override_the_account() {
        let mut policies = Policies::default();
        assert!(!policies.advisor_enabled());
        policies.organization.policy = serde_json::from_str(r#"{"advisor_enabled":true}"#).unwrap();
        assert!(policies.advisor_enabled());
        policies.project.policy = serde_json::from_str(r#"{"advisor_enabled":false}"#).unwrap();
        assert!(!policies.advisor_enabled());
    }
}
