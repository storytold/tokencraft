use db_queries::gateway_requests::RequestScope;
use serde::{Deserialize, Serialize};

use super::{
    availability::RouteAvailability,
    catalog::Catalog,
    providers::ProviderRuntime,
    quote::Quote,
    requests, route_performance,
    routing_policy::Policies,
};
use crate::error::{ApiError, ApiResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorSource {
    Request,
    Policy,
}

pub(super) struct AdvisorSelection {
    pub source: AdvisorSource,
}

impl AdvisorSelection {
    pub(super) fn validate_policy(&self, policies: &Policies) -> ApiResult<()> {
        if self.source == AdvisorSource::Policy && !policies.advisor_enabled() {
            return Err(ApiError::BadRequest(
                "supply a model or enable Advisor routing in policy".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
pub struct AdvisorDecision {
    pub model: String,
    #[schema(inline)]
    pub source: AdvisorSource,
    pub used_advisor: bool,
}

pub(super) struct AdvisorRoute<'a, 'scope> {
    pub scope: RequestScope<'scope>,
    pub catalog: &'a Catalog,
    pub providers: Option<&'a ProviderRuntime>,
    pub policies: &'a Policies,
    pub availability: &'a RouteAvailability,
}

pub(super) async fn concrete_quote(
    connection: &mut sqlx::PgConnection,
    route: AdvisorRoute<'_, '_>,
    ranked: Quote,
    source: AdvisorSource,
) -> ApiResult<Quote> {
    let Some(model) = ranked.selected_offer_id.as_ref().and_then(|id| {
        ranked.candidates.iter().find(|candidate| &candidate.offer_id == id)?.model.clone()
    }) else {
        return Ok(ranked);
    };

    // Use the same named-model quote that a client can submit after previewing.
    let mut input = ranked.input;
    input.model = model.clone();
    input.min_quality = None;
    input.min_usage_accounts = None;
    let quote = requests::media_quote(
        route.catalog, input, route.scope, route.policies, route.providers, route.availability,
    )?;
    if quote.selected_offer_id.is_none() {
        return Err(ApiError::Conflict("the advised model no longer meets the routing requirements".into()));
    }
    let mut quote = route_performance::rank(connection, route.scope, route.catalog, route.providers, quote).await?;
    quote.advisor = Some(AdvisorDecision { model, source, used_advisor: false });
    Ok(quote)
}
