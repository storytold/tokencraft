use std::collections::{BTreeMap, BTreeSet};

use db_queries::{
    gateway_requests::RequestScope,
    gateway_routing_policies::current_for_projects,
    workflows::GenerationDependency,
};
use ids::AccountId;
use serde::Serialize;

use super::{graph::Kind, planner};
use crate::gateway_demo::{
    availability::RouteAvailability,
    catalog::{public_model_id, Adapter, Catalog, Offer, Pricing},
    chat_input::input_bound_from_message_lengths,
    quote::{QuoteInput, Workload},
    routing_policy::Policies,
};

#[derive(Default, Serialize)]
pub(in crate::gateway_demo) struct Report<'a> {
    pub checked_workflow_generation_references: i64,
    pub workflow_generation_blockers: Vec<Blocker<'a>>,
}

#[derive(Serialize)]
pub(in crate::gateway_demo) struct Blocker<'a> {
    model: &'a str,
    kind: &'a str,
    first_frame: bool,
    references: i64,
    reason: &'static str,
}

pub(in crate::gateway_demo) async fn check<'a>(
    conn: &mut sqlx::PgConnection,
    dependencies: &'a [GenerationDependency],
    catalog: &Catalog,
    binding_allowed: impl Fn(&Offer, &AccountId, &Policies, bool) -> bool,
) -> anyhow::Result<Report<'a>> {
    if dependencies.is_empty() {
        return Ok(Report::default());
    }
    let scopes: Vec<_> = dependencies
        .iter()
        .map(|dependency| (&dependency.account_id, &dependency.project_id))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut policy_by_project = BTreeMap::<_, BTreeMap<_, Policies>>::new();
    for item in current_for_projects(conn, &scopes).await? {
        policy_by_project
            .entry(item.account_id)
            .or_default()
            .entry(item.scope_project_id)
            .or_default()
            .set_current(item.row)
            .map_err(|_| anyhow::anyhow!("could not use saved workflow routing policies"))?;
    }
    let default_policy = Policies::default();
    let mut blocked = BTreeMap::new();
    for dependency in dependencies {
        let scope = RequestScope {
            account: &dependency.account_id,
            project: &dependency.project_id,
            client_ref: None,
        };
        let policies = policy_by_project
            .get(scope.account)
            .and_then(|projects| projects.get(scope.project))
            .unwrap_or(&default_policy);
        if !has_route(dependency, catalog, scope, policies, &binding_allowed) {
            let key = (
                dependency.model.as_str(),
                dependency.kind.as_str(),
                dependency.first_frame,
            );
            *blocked.entry(key).or_insert(0) += dependency.runs;
        }
    }
    let workflow_generation_blockers = blocked
        .into_iter()
        .map(|((model, kind, first_frame), references)| Blocker {
            model,
            kind,
            first_frame,
            references,
            reason: "generation_route_unavailable",
        })
        .collect();
    Ok(Report {
        checked_workflow_generation_references: dependencies.iter().map(|dependency| dependency.runs).sum(),
        workflow_generation_blockers,
    })
}

fn has_route(
    dependency: &GenerationDependency,
    catalog: &Catalog,
    scope: RequestScope<'_>,
    policies: &Policies,
    binding_allowed: &impl Fn(&Offer, &AccountId, &Policies, bool) -> bool,
) -> bool {
    let kind = match dependency.kind.as_str() {
        "text" => Kind::Text,
        "image" => Kind::Image,
        "video" => Kind::Video,
        _ => return false,
    };
    let offers: Vec<_> = planner::available_offers(catalog, Some((policies, scope)))
        .filter(|offer| {
            public_model_id(&offer.model) == dependency.model && planner::offer_kind(offer) == Some(kind)
        })
        .collect();
    let fixture = offers.iter().all(|offer| offer.adapter == Adapter::Mock);
    let availability = RouteAvailability::default();
    offers.iter().enumerate().any(|(index, offer)| {
        let eligible = if offer.adapter == Adapter::Mock {
            fixture && index == 0 && !dependency.frame_from_upload
        } else {
            binding_allowed(offer, scope.account, policies, dependency.first_frame)
        };
        if !eligible {
            return false;
        }
        let Some(workload) = smallest_workload(dependency, &offer.pricing) else {
            return false;
        };
        let input = QuoteInput {
            model: dependency.model.clone(),
            workload,
            provider: None,
            preferred_providers: None,
            max_total_usd_nanos: Some(dependency.max_usd_nanos),
            min_quality: None,
            min_usage_accounts: None,
            data_requirements: None,
        };
        catalog
            .scoped_media_quote(input, scope, policies, &availability, |candidate| candidate.id == offer.id)
            .is_ok_and(|quote| quote.selected_offer_id.is_some())
    })
}

fn smallest_workload(dependency: &GenerationDependency, pricing: &Pricing) -> Option<Workload> {
    let ratio = |ratios: &[String]| dependency.aspect_ratio.clone().or_else(|| ratios.first().cloned());
    match pricing {
        Pricing::Text { .. } => {
            let prompt = usize::try_from(dependency.prompt_bytes?).ok()?;
            let system = dependency.system_bytes.and_then(|value| usize::try_from(value).ok());
            Some(Workload::Text {
                max_input_tokens: input_bound_from_message_lengths(std::iter::once(prompt).chain(system))?,
                max_output_tokens: u32::try_from(dependency.max_output_tokens.unwrap_or(1)).ok()?,
            })
        }
        Pricing::Image { aspect_ratios, .. } => Some(Workload::Image {
            count: 1,
            aspect_ratio: ratio(aspect_ratios)?,
            max_relative_error_bps: None,
        }),
        Pricing::Video {
            durations_seconds,
            aspect_ratios,
            resolution,
            ..
        } => Some(Workload::Video {
            duration_seconds: match dependency.duration_seconds {
                Some(value) => u16::try_from(value).ok()?,
                None => *durations_seconds.iter().min()?,
            },
            aspect_ratio: ratio(aspect_ratios)?,
            resolution: dependency.resolution.clone().unwrap_or_else(|| resolution.clone()),
            max_relative_error_bps: None,
        }),
        _ => None,
    }
}
