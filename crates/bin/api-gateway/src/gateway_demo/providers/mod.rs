mod audio_format;
pub(super) mod binding_check;
pub(crate) mod completion;
pub(super) mod connector;
mod mesh_format;
pub(super) mod local_chat;
pub(super) mod rollout;
mod chat_transport;
mod payloads;
mod queue;
pub(super) mod video;
mod worker;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{ensure, Context};
use db_queries::gateway_requests::{
    self,
    queued_runs::{self, ClaimedQueuePoll},
    ClaimedProviderRun, RequestScope, RunOutcome, UpstreamObservation,
};
use reqwest::Url;
use serde::Deserialize;
use tokio::sync::Semaphore;
use super::artifact_storage::ArtifactStorage;
use super::artifacts;
use super::catalog::{digest, Adapter, Catalog, Offer};
use super::catalog_refresh::CatalogStore;
use super::chat_input::ChatInput;
use super::data_handling::{DataHandling, DataRequirements};
use super::routing_policy::Policies;
use connector::Connectors;
use queue::{Observation, Submission};
use chat_transport::{ChatCall, ChatDispatch, StreamSink};
pub(super) use chat_transport::{stream_chunk_purpose, ChatClient, SubmitOutcome};
use payloads::Payloads;

pub struct ProviderRuntime {
    pub payloads: Payloads,
    completion_tokens: completion::CompletionTokens,
    pub(super) artifact_storage: Option<Arc<ArtifactStorage>>,
    pub(super) content_archive: Option<Arc<super::content_history::Store>>,
    pub(super) vault: Option<super::provider_credentials::Vault>,
    local_chat: ChatBindings,
    connectors: Connectors,
    pub(super) streams: Arc<Semaphore>,
}

struct Binding<T> {
    digest: String,
    client: T,
}

type ChatBindings = BTreeMap<String, Binding<ChatClient>>;

impl ProviderRuntime {
    pub(super) fn load(catalog: &Catalog) -> anyhow::Result<Option<Self>> {
        let (local_chat, connectors) = load_bindings(catalog)?;
        if local_chat.is_empty() && connectors.is_empty() { return Ok(None); }
        let root = std::env::var("TOKENCRAFT_DEMO_PAYLOAD_KEY")
            .context("set TOKENCRAFT_DEMO_PAYLOAD_KEY to a persistent 32-byte hex key")?;
        let mut payload_key = [0; 32];
        hex::decode_to_slice(root, &mut payload_key).map_err(|_| {
            anyhow::anyhow!("TOKENCRAFT_DEMO_PAYLOAD_KEY must be a 32-byte hex key")
        })?;
        let tenant_keys = connectors.has_tenant_keys() || local_chat.values().any(|binding| binding.client.requires_tenant_key());
        let vault = tenant_keys
            .then(|| super::provider_credentials::Vault::load(&payload_key)).transpose()?;
        Ok(Some(Self {
            payloads: Payloads::new(&payload_key),
            completion_tokens: completion::CompletionTokens::new(&payload_key),
            artifact_storage: ArtifactStorage::load()?.map(Arc::new),
            content_archive: super::content_history::Store::load()?.map(Arc::new),
            vault,
            local_chat,
            connectors,
            streams: Arc::new(Semaphore::new(MAX_SSE_READERS)),
        }))
    }

    pub(super) fn has_chat_binding(&self, name: &str) -> bool {
        self.local_chat.contains_key(name)
    }

    pub(super) fn compute_stacks(&self) -> impl Iterator<Item = &local_chat::compute::ComputeStack> {
        self.local_chat.values().filter_map(|binding| binding.client.compute_stack())
    }

    pub(super) fn compute_stack(&self, name: &str) -> Option<&local_chat::compute::ComputeStack> {
        self.local_chat.get(name)?.client.compute_stack()
    }

    pub(super) fn has_compute_route(
        &self,
        offer: &Offer,
        account: &ids::AccountId,
        policies: &Policies,
    ) -> bool {
        self.chat_offer(offer).and_then(|(client, _)| client.compute_stack()).is_none_or(|stack| {
            stack.has_target(account, super::compute_providers::order(policies, &stack.name))
        })
    }

    pub(super) fn chat_offer(&self, offer: &Offer) -> Option<(&ChatClient, &str)> {
        let binding = match offer.adapter {
            Adapter::OpenAiCompatibleChat => self.local_chat.get(offer.local_binding.as_deref()?),
            _ => None,
        }?;
        Some((&binding.client, &binding.digest))
    }

    pub(super) fn tenant_binding(&self, name: &str) -> Option<(Adapter, &str)> {
        self.local_chat.get(name).filter(|binding| binding.client.requires_tenant_key())
            .map(|binding| (Adapter::OpenAiCompatibleChat, binding.digest.as_str()))
            .or_else(|| self.connectors.for_offer(name).filter(|binding| binding.tenant_keys)
                .map(|binding| (Adapter::ConnectorQueue, binding.digest.as_str())))
    }

    async fn pinned_credential(
        &self, pool: &db::PgPool, request: &str, binding: &str, version: Option<i32>,
    ) -> sqlx::Result<Option<reqwest::header::HeaderValue>> {
        match (&self.vault, version) {
            (Some(vault), Some(_)) => vault.pinned(pool, request, binding).await,
            _ => Ok(None),
        }
    }

    pub(super) fn pin_catalog(&self, catalog: &mut Catalog) -> anyhow::Result<()> {
        pin_catalog(catalog, &self.local_chat, &self.connectors)
    }

    pub(super) fn accepts_workflow_frame(&self, offer: &Offer) -> bool {
        self.queue_offer_binding(offer)
            .and_then(|binding| self.connectors.by_digest(binding))
            .is_some_and(|connector| connector.accepts_workflow_frames)
    }

    pub(super) fn queue_offer_binding(&self, offer: &Offer) -> Option<&str> {
        match offer.adapter {
            Adapter::ConnectorQueue => self.connectors
                .for_offer(offer.local_binding.as_deref()?).map(|binding| binding.digest.as_str()),
            _ => None,
        }
    }

    pub(super) fn connector_binding_names(&self) -> impl Iterator<Item = &str> {
        self.connectors.binding_names()
    }

}

fn load_bindings(catalog: &Catalog) -> anyhow::Result<(ChatBindings, Connectors)> {
    let chat = local_chat::load(catalog)?;
    let queue = Connectors::load(catalog)?;
    ensure!(
        !chat.iter().any(|(name, binding)| binding.client.requires_tenant_key()
            && queue.for_offer(name).is_some_and(|connector| connector.tenant_keys)),
        "tenant chat and media bindings must use distinct names"
    );
    Ok((chat, queue))
}

pub(super) fn pin_configured_catalog(
    catalog: &mut Catalog,
    candidate: Option<&mut Catalog>,
) -> anyhow::Result<()> {
    let (local_chat, connectors) = load_bindings(catalog)?;
    pin_catalog(catalog, &local_chat, &connectors)?;
    if let Some(candidate) = candidate {
        pin_catalog(candidate, &local_chat, &connectors)?;
    }
    Ok(())
}

fn pin_catalog(
    catalog: &mut Catalog,
    local_chat: &ChatBindings,
    connectors: &Connectors,
) -> anyhow::Result<()> {
    for offer in &catalog.offers {
        if offer.adapter.supports_chat() {
            let binding = offer.local_binding.as_ref().and_then(|name| local_chat.get(name));
            let regions = offer.data_handling.as_ref().and_then(|policy| policy.processing_regions.as_deref());
            if let (Some(binding), Some(regions)) = (binding, regions) {
                ensure!(binding.client.covers_compute_regions(regions),
                    "processing_regions omit a configured compute target for offer {}", offer.id);
            }
        }
    }
    let digests: Vec<_> = local_chat.iter()
            .filter(|(name, _)| catalog.offers.iter().any(|offer|
                offer.adapter == Adapter::OpenAiCompatibleChat
                    && offer.local_binding.as_ref() == Some(*name)))
            .map(|(_, binding)| binding.digest.as_str())
        .chain(connectors.active_digests()).collect();
    if !digests.is_empty() {
        let encoded = serde_json::to_vec(&(&catalog.revision, digests))?;
        catalog.revision = format!("demo_{}", digest(encoded));
    }
    Ok(())
}

pub(super) fn fixture_endpoint(raw: &str, path: &str) -> anyhow::Result<Url> {
    let endpoint = Url::parse(raw).map_err(|_| anyhow::anyhow!("invalid provider fixture URL"))?;
    ensure!(
        (raw.starts_with("http://127.0.0.1:") || raw.starts_with("http://[::1]:"))
            && endpoint.scheme() == "http"
            && matches!(endpoint.host_str(), Some("127.0.0.1" | "[::1]"))
            && endpoint.port().is_some_and(|port| port > 0)
            && endpoint.path() == path
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "provider fixtures require the exact endpoint on a literal loopback IP and port"
    );
    Ok(endpoint)
}

pub(super) const MAX_SSE_READERS: usize = 32;
pub(super) const MAX_PROVIDER_TASKS: usize = 4;

impl ProviderRuntime {
    pub(super) fn local_binding_digests(&self) -> impl Iterator<Item = &str> {
        self.local_chat.values().map(|binding| binding.digest.as_str())
    }

    pub(super) fn connector_binding_digests(&self) -> impl Iterator<Item = &str> {
        self.connectors.active_digests()
    }
}

pub(super) fn spawn(
    pool: db::PgPool,
    runtime: Arc<ProviderRuntime>,
    catalog: Arc<CatalogStore>,
    readiness: Arc<super::readiness::Readiness>,
) {
    if runtime.vault.is_some() {
        let pool = pool.clone();
        let readiness = Arc::clone(&readiness);
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(30));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                readiness.work_tick(&mut timer).await;
                if let Err(error) = db_queries::gateway_provider_credentials::clear_retired(&pool).await {
                    log_database_error(&error);
                }
            }
        });
    }
    completion::spawn(pool.clone(), Arc::clone(&runtime), Arc::clone(&readiness));
    tokio::spawn(worker::run(pool, runtime, catalog, readiness));
}

async fn dispatch(
    pool: &db::PgPool,
    runtime: &ProviderRuntime,
    catalog: &Catalog,
    run: ClaimedProviderRun,
) -> sqlx::Result<()> {
    let scope = RequestScope {
        account: &run.account_id,
        project: &run.project_id,
        client_ref: None,
    };
    let binding = match run.adapter.as_str() {
        "connector_queue" => runtime.connectors.name_by_digest(&run.binding_digest),
        "openai_compatible_chat" => runtime.local_chat.iter()
            .find(|(_, entry)| entry.digest == run.binding_digest).map(|(name, _)| name.as_str()),
        _ => None,
    };
    let connection = super::provider_access::connection_id(&run.provider, &run.adapter, binding);
    let active_offer = catalog.offers.iter().find(|offer| {
        offer.id == run.offer_id && offer.adapter.as_str() == run.adapter
            && offer.connection_id() == connection
    });
    let rejection = match (run.provider.parse(), Policies::load(pool, scope).await) {
        (Ok(_), Ok(policies)) if active_offer.is_some_and(|offer|
            !runtime.has_compute_route(offer, &run.account_id, &policies)) =>
            Some("compute_targets_excluded"),
        (Ok(provider), Ok(policies)) if active_offer.is_some() && policies.allows_provider(&provider) && policies.allows_connection(catalog, &connection, scope) => {
            let admitted = run.data_requirements.as_ref()
                .map(|value| DataRequirements::deserialize(value)).transpose();
            match admitted {
                Ok(admitted) => {
                    let rules = policies.data_requirements(admitted.as_ref());
                    (!rules.is_empty() && rules.mismatch(offer_data(runtime, catalog, &run)).is_some())
                        .then_some("provider_data_handling_blocked")
                }
                Err(_) => Some("provider_policy_unavailable"),
            }
        }
        (Ok(_), Ok(_)) => Some("provider_blocked_by_policy"),
        _ => {
            tracing::warn!("provider dispatch policy could not be verified");
            Some("provider_policy_unavailable")
        }
    };
    if let Some(reason) = rejection {
        return gateway_requests::finish_provider_run(
            pool,
            &run.request_id,
            run.lease,
            RunOutcome::Rejected(reason),
            Default::default(),
        ).await;
    }
    if run.adapter == "connector_queue" {
        return submit_connector(pool, runtime, run).await;
    }
    if !gateway_requests::begin_provider_submission(pool, &run.request_id, run.lease).await? {
        return Ok(());
    }
    execute(pool, runtime, run).await
}

fn offer_data<'a>(
    runtime: &ProviderRuntime,
    catalog: &'a Catalog,
    run: &ClaimedProviderRun,
) -> Option<&'a DataHandling> {
    let offer = catalog.offers.iter().find(|offer| offer.id == run.offer_id)?;
    if offer.provider.as_str() != run.provider
        || offer.adapter.as_str() != run.adapter
        || offer.upstream_model.as_deref() != Some(run.upstream_model.as_str())
    {
        return None;
    }
    let binding = runtime.queue_offer_binding(offer)
        .or_else(|| runtime.chat_offer(offer).map(|(_, digest)| digest));
    if binding != Some(run.binding_digest.as_str()) {
        return None;
    }
    offer.data_handling.as_ref()
}

async fn execute(
    pool: &db::PgPool,
    runtime: &ProviderRuntime,
    run: ClaimedProviderRun,
) -> sqlx::Result<()> {
    let id = run.request_id.as_str();
    let binding = runtime.local_chat.values().find(|entry| entry.digest == run.binding_digest);
    let outcome = match binding {
        Some(binding) => match runtime
            .payloads
            .open(id, "input", &run.request_payload)
            .and_then(|bytes| serde_json::from_slice::<ChatInput>(&bytes).ok())
        {
            Some(input) => submit_chat(pool, runtime, binding, &run, input).await?,
            None => SubmitOutcome::Rejected("provider_input_unavailable"),
        },
        None => SubmitOutcome::Rejected("provider_binding_unavailable"),
    };
    match outcome {
        SubmitOutcome::Completed(completion) => {
            let payload = serde_json::to_vec(&completion.result)
                .ok()
                .and_then(|body| runtime.payloads.seal(id, "result", body));
            let observation = UpstreamObservation {
                handle: Some(&completion.handle),
                model: completion.model.as_deref(),
                provider: completion.provider.as_deref(),
                finish_reason: completion.result.finish_reason.as_deref(),
                input_tokens: completion.input_tokens,
                output_tokens: completion.output_tokens,
                reported_usd_nanos: completion.cost_usd_nanos,
                partial_usage: false,
            };
            let outcome = match payload.as_deref() {
                Some(payload) => RunOutcome::Completed {
                    payload,
                    outputs: 1,
                    artifacts: &[],
                },
                None => RunOutcome::Uncertain("provider_result_unavailable"),
            };
            gateway_requests::finish_provider_run(pool, id, run.lease, outcome, observation).await
        }
        SubmitOutcome::Rejected(reason) => {
            gateway_requests::finish_provider_run(
                pool,
                id,
                run.lease,
                RunOutcome::Rejected(reason),
                Default::default(),
            )
            .await
        }
        SubmitOutcome::Uncertain(reason) => {
            gateway_requests::finish_provider_run(
                pool,
                id,
                run.lease,
                RunOutcome::Uncertain(reason),
                Default::default(),
            )
            .await
        }
        SubmitOutcome::Interrupted(completion) => {
            let partial_usage = completion.input_tokens.is_some()
                || completion.output_tokens.is_some()
                || completion.cost_usd_nanos.is_some();
            gateway_requests::finish_provider_run(
                pool,
                id,
                run.lease,
                RunOutcome::Uncertain("provider_stream_interrupted"),
                UpstreamObservation {
                    handle: Some(&completion.handle),
                    model: completion.model.as_deref(),
                    provider: completion.provider.as_deref(),
                    finish_reason: None,
                    input_tokens: completion.input_tokens,
                    output_tokens: completion.output_tokens,
                    reported_usd_nanos: completion.cost_usd_nanos,
                    partial_usage,
                },
            )
            .await
        }
    }
}

async fn submit_chat(
    pool: &db::PgPool,
    runtime: &ProviderRuntime,
    binding: &Binding<ChatClient>,
    run: &ClaimedProviderRun,
    input: ChatInput,
) -> sqlx::Result<SubmitOutcome> {
    let missing = SubmitOutcome::Rejected("provider_credential_unavailable");
    if binding.client.requires_tenant_key() != run.credential_version.is_some() { return Ok(missing); }
    let tenant_key = runtime.pinned_credential(pool, &run.request_id, &run.binding_digest, run.credential_version).await?;
    if binding.client.requires_tenant_key() && tenant_key.is_none() { return Ok(missing); }
    if input.stream {
        return binding
            .client
            .stream(
                &run.upstream_model,
                input,
                StreamSink {
                    pool,
                    request: &run.request_id,
                    trace_id: run.trace_id.as_deref(),
                    lease: run.lease,
                    payloads: &runtime.payloads,
                },
                tenant_key.as_ref(),
            )
            .await;
    }
    Ok(binding
        .client
        .submit_for_run(ChatCall {
            model: &run.upstream_model,
            input: &input,
            stream: false,
            tenant_key: tenant_key.as_ref(),
            dispatch: Some(ChatDispatch {
                pool: Some(pool),
                request_id: &run.request_id,
                trace_id: run.trace_id.as_deref(),
                lease: run.lease,
            }),
        })
        .await)
}

async fn submit_connector(
    pool: &db::PgPool,
    runtime: &ProviderRuntime,
    run: ClaimedProviderRun,
) -> sqlx::Result<()> {
    let id = run.request_id.as_str();
    let submitted = match runtime.connectors.by_digest(&run.binding_digest) {
        Some(connector) => match connector::open_input(pool, &runtime.payloads, &run).await? {
            Some(input) => {
                let key = runtime.pinned_credential(pool, id, &run.binding_digest, run.credential_version).await?;
                if connector.tenant_keys != run.credential_version.is_some()
                    || (connector.tenant_keys && key.is_none())
                {
                    Submission::Rejected("provider_credential_unavailable")
                } else {
                    if !gateway_requests::begin_provider_submission(pool, id, run.lease).await? {
                        return Ok(());
                    }
                    connector.submit(&run, input, &runtime.completion_tokens, key.as_ref()).await
                }
            }
            None => Submission::Rejected("provider_input_unavailable"),
        },
        None => Submission::Rejected("provider_binding_unavailable"),
    };
    record_queue_submission(pool, &run, submitted).await
}

async fn record_queue_submission(
    pool: &db::PgPool,
    run: &ClaimedProviderRun,
    submitted: Submission,
) -> sqlx::Result<()> {
    let id = run.request_id.as_str();
    let outcome = match submitted {
        Submission::Accepted(handle) => {
            match queued_runs::acknowledge_queue(pool, id, run.lease, &handle, &run.adapter).await {
                Err(error) if queued_runs::is_handle_conflict(&error) => {
                    tracing::warn!("connector returned a handle assigned to another request");
                    RunOutcome::Uncertain("connector_submission_unconfirmed")
                }
                result => return result,
            }
        }
        Submission::Unavailable(limit) => {
            gateway_requests::queue_cooldowns::record(pool, run, limit.cooldown_ms).await?;
            let scheduled = match limit.retry_after_ms {
                Some(wait) => gateway_requests::reschedule_queue_submission(pool, run, wait.max(50), 1).await?,
                None => false,
            };
            if scheduled {
                return Ok(());
            }
            RunOutcome::Rejected("provider_rate_limited")
        }
        Submission::Rejected(reason) => RunOutcome::Rejected(reason),
        Submission::Uncertain(reason) => RunOutcome::Uncertain(reason),
    };
    gateway_requests::finish_provider_run(pool, id, run.lease, outcome, Default::default()).await
}

async fn observe_queue(
    pool: &db::PgPool,
    runtime: &ProviderRuntime,
    mut task: ClaimedQueuePoll,
) -> sqlx::Result<()> {
    let id = task.request_id.as_str();
    let observed = match runtime.connectors.by_digest(&task.binding_digest) {
            Some(connector) => {
                let key = runtime.pinned_credential(pool, id, &task.binding_digest, task.credential_version).await?;
                if connector.tenant_keys != task.credential_version.is_some()
                    || (connector.tenant_keys && key.is_none())
                {
                    return gateway_requests::finish_provider_run(
                        pool, id, task.lease, RunOutcome::Uncertain("provider_credential_unavailable"), Default::default(),
                    ).await;
                }
                let status = connector.poll(&task, key.as_ref()).await;
                let media = fetch_observed_result(pool, &task, status, async || {
                    connector.fetch(&task, key.as_ref()).await
                }).await?;
                Some(media)
            }
        None => None,
    };
    let Some(result) = observed else {
        return gateway_requests::finish_provider_run(
            pool, id, task.lease, RunOutcome::Uncertain("provider_binding_unavailable"), Default::default(),
        ).await;
    };
    let observation = UpstreamObservation {
        model: Some(&task.upstream_model),
        provider: None,
        ..Default::default()
    };
    match result {
        Observation::StillRunning(state) => {
            queued_runs::defer_queue_poll(pool, &task, Some(state)).await
        }
        Observation::Retry => queued_runs::defer_queue_poll(pool, &task, None).await,
        Observation::Rejected(reason) => {
            gateway_requests::finish_provider_run(
                pool,
                id,
                task.lease,
                RunOutcome::Rejected(reason),
                observation,
            )
            .await
        }
        Observation::Ready(media) => {
            let Ok(request) = id.parse() else {
                return queued_runs::defer_queue_poll(pool, &task, Some("completed")).await;
            };
            let prepare = artifacts::prepare(
                pool, runtime.artifact_storage.as_deref(), &runtime.payloads.media, &request, media,
            );
            tokio::pin!(prepare);
            let staged = if runtime.artifact_storage.as_ref().is_some_and(|store| store.backend() == "s3") {
                // Keep the short recovery lease while network publication makes progress.
                let mut renewals = tokio::time::interval(Duration::from_secs(8));
                renewals.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        biased;
                        _ = renewals.tick() => {
                            let Some(lease) = queued_runs::renew_publication(pool, id, task.lease).await? else {
                                return Ok(());
                            };
                            task.lease = lease;
                        }
                        result = &mut prepare => break result,
                    }
                }
            } else {
                prepare.await
            };
            let stored = staged.and_then(|(manifest, artifacts)| {
                let body = serde_json::to_vec(&manifest).ok()?;
                Some((runtime.payloads.seal(id, "result", body)?, artifacts))
            });
            let Some((payload, media)) = stored else {
                return queued_runs::defer_queue_poll(pool, &task, Some("completed")).await;
            };
            gateway_requests::finish_provider_run(
                pool,
                id,
                task.lease,
                RunOutcome::Completed {
                    payload: &payload,
                    outputs: task.expected_outputs,
                    artifacts: &media,
                },
                observation,
            )
            .await
        }
    }
}

async fn fetch_observed_result(
    pool: &db::PgPool,
    task: &ClaimedQueuePoll,
    status: Observation<Option<queued_runs::ProviderCharge>>,
    fetch: impl AsyncFnOnce() -> Option<artifacts::FetchedMedia>,
) -> sqlx::Result<Observation<artifacts::FetchedMedia>> {
    Ok(match status {
        Observation::Ready(charge) => {
            if !queued_runs::record_result_observed(pool, task, charge.as_ref()).await? {
                return Ok(Observation::Retry);
            }
            fetch().await.map_or(Observation::StillRunning("completed"), Observation::Ready)
        }
        Observation::StillRunning(state) => Observation::StillRunning(state),
        Observation::Rejected(reason) => Observation::Rejected(reason),
        Observation::Retry => Observation::Retry,
    })
}

fn log_database_error(error: &sqlx::Error) {
    tracing::warn!(sqlstate = ?error.as_database_error().and_then(|error| error.code()), "provider worker database operation failed");
}
