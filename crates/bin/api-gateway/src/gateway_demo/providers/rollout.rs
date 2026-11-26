use std::{env, ffi::OsString, io, net::IpAddr, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Utc};
use db_queries::gateway_requests::{BindingDependency, binding_dependencies};
use db_queries::workflows::{generation_dependencies, tool_dependencies};
use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{ChatBindings, Connectors, load_bindings, pin_catalog};
use crate::gateway_demo::{
    catalog::{Adapter, Catalog, Offer},
    catalog_refresh::CatalogStore,
    compute_providers,
    routing_policy::Policies,
    workflows::{generation_rollout, rollout as workflow_rollout},
};

const MAX_GROUPS: i64 = 1024;
const HELP: &str = "Usage: api-gateway catalog check-drain --catalog FILE

Compare candidate providers, workflow generation routes, and tool bindings with outstanding work in DATABASE_URL.
Use the candidate's binding environment. The database connection is read-only.
The JSON report lists blockers without exposing individual requests or workflow runs.
";

#[derive(Serialize)]
struct Report<'a> {
    version: u8,
    checked_at: DateTime<Utc>,
    base_revision: &'a str,
    ready: bool,
    checked_requests: i64,
    blockers: Vec<Blocker<'a>>,
    #[serde(flatten)]
    workflows: workflow_rollout::Report<'a>,
    #[serde(flatten)]
    generation: generation_rollout::Report<'a>,
}

#[derive(Serialize)]
struct Blocker<'a> {
    phase: &'a str,
    adapter: &'a str,
    offer_id: &'a str,
    binding_sha256: &'a str,
    requests: i64,
    reason: &'static str,
}

pub(in crate::gateway_demo) async fn run(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let catalog = match args.next().as_deref().and_then(|value| value.to_str()) {
        Some("--catalog") => args.next().context("set --catalog FILE")?,
        None | Some("--help" | "-h") => {
            print!("{HELP}");
            return Ok(());
        }
        _ => bail!("unknown drain check option. Use --help"),
    };
    ensure!(args.next().is_none(), "supply one --catalog FILE");
    let mut catalog = Catalog::load(Path::new(&catalog))?;
    let (chat, connectors) = load_bindings(&catalog)?;
    pin_catalog(&mut catalog, &chat, &connectors)?;

    let url = env::var("DATABASE_URL").context("set DATABASE_URL for the drain check")?;
    let options: PgConnectOptions = url
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid drain check database URL"))?;
    let local = options.get_socket().is_some()
        || options.get_host() == "localhost"
        || options
            .get_host()
            .parse::<IpAddr>()
            .is_ok_and(|host| host.is_loopback());
    if !local {
        db::verify_hosted_url(&url)?;
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .context("connect to the drain check database")?;
    let mut tx = pool.begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").await?;
    sqlx::query("SET LOCAL statement_timeout = '5s'")
        .execute(&mut *tx)
        .await?;
    let checked_at = Utc::now();
    let dependencies = binding_dependencies(&mut tx, MAX_GROUPS + 1).await?;
    let workflow_dependencies = tool_dependencies(&mut tx, MAX_GROUPS + 1).await?;
    let generation_dependencies = generation_dependencies(&mut tx, MAX_GROUPS + 1).await?;
    ensure!(
        dependencies.len() <= MAX_GROUPS as usize,
        "too many binding groups to certify this rollout"
    );
    ensure!(
        workflow_dependencies.len() <= MAX_GROUPS as usize,
        "too many workflow tool binding groups to certify this rollout"
    );
    ensure!(
        generation_dependencies.len() <= MAX_GROUPS as usize,
        "too many workflow generation groups to certify this rollout"
    );
    let generation = if generation_dependencies.is_empty() {
        generation_rollout::Report::default()
    } else {
        let candidate = CatalogStore::load(Arc::new(catalog.clone()))?.current();
        generation_rollout::check(
            &mut tx,
            &generation_dependencies,
            &candidate,
            |offer, account, policies, frame| generation_binding(offer, account, policies, frame, &chat, &connectors),
        )
        .await?
    };
    tx.commit().await?;
    let workflows = workflow_rollout::check(&workflow_dependencies)?;
    let blockers: Vec<_> = dependencies
        .iter()
        .filter_map(|entry| {
            let reason = blocker(entry, &catalog, &chat, &connectors)?;
            Some(Blocker {
                phase: &entry.phase,
                adapter: &entry.adapter,
                offer_id: &entry.offer_id,
                binding_sha256: &entry.binding_digest,
                requests: entry.requests,
                reason,
            })
        })
        .collect();
    let report = Report {
        version: 1,
        checked_at,
        base_revision: &catalog.revision,
        ready: blockers.is_empty()
            && workflows.workflow_tool_blockers.is_empty()
            && generation.workflow_generation_blockers.is_empty(),
        checked_requests: dependencies.iter().map(|entry| entry.requests).sum(),
        blockers,
        workflows,
        generation,
    };
    serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
    println!();
    ensure!(
        report.generation.workflow_generation_blockers.is_empty(),
        "candidate does not retain all outstanding workflow generation routes"
    );
    ensure!(
        report.ready,
        "candidate does not retain all outstanding provider or workflow tool dependencies"
    );
    Ok(())
}

fn generation_binding(
    offer: &Offer,
    account: &ids::AccountId,
    policies: &Policies,
    frame: bool,
    chat: &ChatBindings,
    connectors: &Connectors,
) -> bool {
    let Some(slot) = offer.local_binding.as_deref() else {
        return false;
    };
    match offer.adapter {
        Adapter::OpenAiCompatibleChat => chat.get(slot).is_some_and(|binding| {
            binding.client.compute_stack().is_none_or(|stack| {
                stack.has_target(account, compute_providers::order(policies, &stack.name))
            })
        }),
        Adapter::ConnectorQueue => connectors
            .for_offer(slot)
            .is_some_and(|binding| !frame || binding.accepts_workflow_frames),
        Adapter::Mock => false,
    }
}

fn blocker(
    entry: &BindingDependency,
    catalog: &Catalog,
    chat: &ChatBindings,
    connectors: &Connectors,
) -> Option<&'static str> {
    let binding = entry.binding_digest.as_str();
    let retained = match entry.adapter.as_str() {
        "connector_queue" if entry.phase == "submission_recovery" => connectors
            .completion_digests()
            .any(|digest| digest == binding),
        "connector_queue" => connectors.by_digest(binding).is_some(),
        "openai_compatible_chat" => chat.values().any(|connection| connection.digest == binding),
        _ => return Some("legacy_adapter"),
    };
    if !retained {
        return Some("binding_unavailable");
    }
    if entry.phase == "queued"
        && !catalog.offers.iter().any(|offer| {
            offer.id == entry.offer_id
                && offer.provider.as_str() == entry.provider
                && offer.adapter.as_str() == entry.adapter
                && offer.upstream_model.as_deref() == Some(entry.upstream_model.as_str())
                && offer.local_binding.as_deref().is_some_and(|slot| {
                    let configured = match offer.adapter {
                        Adapter::ConnectorQueue => connectors
                            .for_offer(slot)
                            .map(|value| value.digest.as_str()),
                        Adapter::OpenAiCompatibleChat => {
                            chat.get(slot).map(|value| value.digest.as_str())
                        }
                        Adapter::Mock => None,
                    };
                    configured == Some(binding)
                })
        })
    {
        return Some("queued_offer_changed");
    }
    None
}
