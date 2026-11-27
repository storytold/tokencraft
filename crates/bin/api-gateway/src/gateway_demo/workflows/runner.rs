use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use db_queries::{gateway_requests::RequestScope, workflows as db};
use ids::GatewayRequestId;
use serde::{Deserialize, Serialize};

use super::super::access::Access;
use super::super::artifacts;
use super::super::catalog::{public_model_id, Adapter};
use super::super::catalog_refresh::CatalogStore;
use super::super::chat_input::{message_input_bound, Message, Role};
use super::super::providers::ProviderRuntime;
use super::super::quote::{QuoteInput, Workload};
use super::super::requests::{
    self, AdmissionInput, NativeProviderResult, QuoteSelection, RequestStatus,
};
use super::super::routing_policy::Policies;
use super::graph::{Binding, InputValues, Kind, Node, Output, Value, ValueType};
use super::{management, planner, reuse, tool_execution, uploads};
use super::Security;
use crate::error::{ApiError, ApiResult};

pub(super) struct Runtime {
    pub pool: sqlx::PgPool,
    pub security: Arc<Security>,
    pub catalog: Arc<CatalogStore>,
    pub providers: Option<Arc<ProviderRuntime>>,
    pub readiness: Arc<super::super::readiness::Readiness>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Step {
    pub node: String,
    pub request: GatewayRequestId,
    pub status: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct State {
    pub inputs: InputValues,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_fingerprints: Option<BTreeMap<String, String>>,
    pub outputs: BTreeMap<String, Output>,
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, tool_execution::Progress>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse: Option<reuse::Reuse>,
}

impl State {
    pub fn new(inputs: InputValues, security: &Security) -> ApiResult<Self> {
        Ok(Self {
            input_fingerprints: Some(reuse::fingerprints(security, &inputs)?),
            inputs,
            outputs: BTreeMap::new(),
            steps: Vec::new(),
            tools: BTreeMap::new(),
            error: None,
            failed_node: None,
            reuse: None,
        })
    }

    pub fn seal(&self, security: &Security, run: &ids::WorkflowRunId) -> ApiResult<Vec<u8>> {
        let encoded = serde_json::to_vec(self).map_err(|_| unavailable())?;
        security
            .payloads
            .seal(run.as_str(), encoded)
            .ok_or_else(unavailable)
    }

    pub fn read(security: &Security, row: &db::Run) -> ApiResult<Option<Self>> {
        row.content
            .as_ref()
            .map(|bytes| {
                let plain = security
                    .payloads
                    .open(row.id.as_str(), bytes)
                    .ok_or_else(unavailable)?;
                serde_json::from_slice(&plain).map_err(|_| unavailable())
            })
            .transpose()
    }
}

pub(super) fn unavailable() -> ApiError {
    ApiError::Internal("workflow state is unavailable".into())
}

pub(super) fn scope(row: &db::Run) -> RequestScope<'_> {
    RequestScope {
        account: &row.account_id,
        project: &row.project_id,
        client_ref: row.client_ref.as_deref().or_else(|| row.version.is_some().then_some(row.actor_id.as_str())),
    }
}

pub(super) async fn advance(runtime: &Runtime, row: db::Run) -> ApiResult<()> {
    let mut state = State::read(&runtime.security, &row)?.ok_or_else(unavailable)?;
    if let Some(reuse) = &mut state.reuse {
        if let Some((node, output)) = reuse.pending.pop_first() {
            // Finish all reuse checks before starting any new tool or paid request.
            let artifact = match reuse::materialize(runtime, &row, &reuse.source_run_id, &node, &output).await {
                Ok(artifact) => artifact,
                Err(ApiError::NotFound(_)) => {
                    return fail(runtime, &row, state, &node, "reused_output_unavailable").await;
                }
                Err(error) => return Err(error),
            };
            reuse.completed.insert(node.clone());
            state.outputs.insert(node.clone(), output);
            return match artifact {
                Some(artifact) => checkpoint_artifact(runtime, &row, state, &node, artifact).await,
                None => checkpoint(runtime, &row, state, "running", 0).await,
            };
        }
    }
    let mut graph = management::decode(row.graph.clone())?;
    if graph.schema == reuse::STORED_GRAPH_SCHEMA {
        if state.reuse.is_none() {
            return Err(unavailable());
        }
        graph.schema = 1;
    }
    let plan = graph.compile().map_err(|_| unavailable())?;
    let Some(node) = plan
        .nodes
        .into_iter()
        .find(|node| !state.outputs.contains_key(&node.id))
    else {
        state.inputs.clear();
        return checkpoint(runtime, &row, state, "succeeded", 0).await;
    };

    if node.kind == Kind::Tool {
        return match tool_execution::advance(runtime, &row, node, &mut state).await {
            Ok(Some(artifact)) => {
                checkpoint_artifact(runtime, &row, state, &node.id, artifact).await
            }
            Ok(None) => checkpoint(runtime, &row, state, "running", 1).await,
            Err(ApiError::ServiceUnavailable(_)) => {
                if let Some(step) = state.tools.get_mut(&node.id) {
                    step.status = "reconciling".into();
                }
                checkpoint(runtime, &row, state, "reconciling", 3).await
            }
            Err(ApiError::Conflict(_)) => Ok(()),
            Err(error @ ApiError::Internal(_)) => Err(error),
            Err(_) => fail(runtime, &row, state, &node.id, "tool_step_failed").await,
        };
    }

    if let Some(step) = state.steps.last_mut().filter(|step| step.node == node.id) {
        let child = requests::get(&runtime.pool, scope(&row), &step.request).await?;
        match child.status {
            RequestStatus::Queued | RequestStatus::Running => {
                return checkpoint(runtime, &row, state, "running", 2).await;
            }
            RequestStatus::AwaitingReconciliation => {
                step.status = "reconciling".into();
                return checkpoint(runtime, &row, state, "reconciling", 5).await;
            }
            RequestStatus::Failed | RequestStatus::Canceled => {
                step.status = "failed".into();
                return fail(runtime, &row, state, &node.id, "model_request_failed").await;
            }
            RequestStatus::Succeeded => {
                let result =
                    child_output(runtime, &row, node, &step.request, child.fixture_output()).await;
                match result {
                    Ok(output) => {
                        step.status = "succeeded".into();
                        state.outputs.insert(node.id.clone(), output);
                        return checkpoint(runtime, &row, state, "running", 0).await;
                    }
                    Err(ApiError::NotFound(_)) => {
                        return fail(runtime, &row, state, &node.id, "model_output_unavailable").await
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    match submit(runtime, &row, node, &state).await {
        Ok(request) => {
            state.steps.push(Step {
                node: node.id.clone(),
                request,
                status: "running".into(),
            });
            checkpoint(runtime, &row, state, "running", 1).await
        }
        Err(ApiError::ActiveRequestLimit { .. } | ApiError::ServiceUnavailable(_) | ApiError::QuoteStale) => {
            checkpoint(runtime, &row, state, "queued", 3).await
        }
        Err(ApiError::SubmissionRateLimit(seconds)) => {
            checkpoint(runtime, &row, state, "queued", seconds.clamp(1, 60) as i32).await
        }
        Err(ApiError::BudgetExceeded) => {
            fail(runtime, &row, state, &node.id, "project_budget_exceeded").await
        }
        Err(ApiError::Internal(message)) => Err(ApiError::Internal(message)),
        Err(_) => {
            fail(
                runtime,
                &row,
                state,
                &node.id,
                "step_configuration_or_policy_rejected",
            )
            .await
        }
    }
}

async fn fail(
    runtime: &Runtime,
    row: &db::Run,
    mut state: State,
    node: &str,
    error: &'static str,
) -> ApiResult<()> {
    state.error = Some(error.into());
    state.failed_node = Some(node.to_owned());
    if let Some(step) = state.tools.get_mut(node) {
        step.status = "failed".into();
        step.finished_at = Some(Utc::now());
    }
    state.inputs.clear();
    checkpoint(runtime, row, state, "failed", 0).await
}

async fn checkpoint(
    runtime: &Runtime,
    row: &db::Run,
    state: State,
    status: &str,
    delay_seconds: i32,
) -> ApiResult<()> {
    let content = state.seal(&runtime.security, &row.id)?;
    db::checkpoint_run(
        &runtime.pool,
        db::Checkpoint {
            id: &row.id,
            lease: row.lease,
            status,
            content: &content,
            delay_seconds,
        },
    )
    .await?;
    Ok(())
}

async fn checkpoint_artifact(
    runtime: &Runtime, row: &db::Run, state: State, node: &str, artifact: db::ToolArtifact,
) -> ApiResult<()> {
    let content = state.seal(&runtime.security, &row.id)?;
    let mut tx = runtime.pool.begin().await?;
    if db::lock_active_run(&mut *tx, &row.id, row.lease).await?.is_none() {
        return Ok(());
    }
    db::insert_tool_artifact(&mut *tx, &row.id, node, &artifact).await?;
    if db::checkpoint_run(&mut *tx, db::Checkpoint {
        id: &row.id, lease: row.lease, status: "running", content: &content, delay_seconds: 0,
    }).await? {
        tx.commit().await?;
    }
    Ok(())
}

async fn child_output(
    runtime: &Runtime,
    row: &db::Run,
    node: &Node,
    request: &GatewayRequestId,
    fixture: bool,
) -> ApiResult<Output> {
    if fixture {
        return Ok(match node.kind {
            Kind::Text => Output::Text {
                text: "Simulated model output.".into(),
            },
            Kind::Tool => return Err(unavailable()),
            Kind::Image | Kind::Video => Output::Fixture {
                kind: node.kind.output(),
            },
        });
    }
    let result = requests::native_provider_result(
        &runtime.pool,
        runtime.providers.as_deref(),
        scope(row),
        request,
    )
    .await?;
    match (node.kind, result) {
        (Kind::Text, NativeProviderResult::Chat(result))
            if !result.result.text.trim().is_empty() =>
        {
            Ok(Output::Text {
                text: result.result.text,
            })
        }
        (
            Kind::Image,
            NativeProviderResult::Media {
                result: super::super::artifacts::MediaResult::Images { images },
                ..
            },
        ) if images.len() == 1 => Ok(Output::Image {
            request_id: request.clone(),
        }),
        (
            Kind::Video,
            NativeProviderResult::Media {
                result: super::super::artifacts::MediaResult::Video { .. },
                ..
            },
        ) => Ok(Output::Video {
            request_id: request.clone(),
        }),
        _ => Err(ApiError::NotFound(
            "the model did not produce the expected output".into(),
        )),
    }
}

async fn submit(
    runtime: &Runtime,
    row: &db::Run,
    node: &Node,
    progress: &State,
) -> ApiResult<GatewayRequestId> {
    let catalog = runtime.catalog.current();
    let scope = scope(row);
    let policies = Policies::load(&runtime.pool, scope).await?;
    let mut matching = planner::available_offers(&catalog, Some((&policies, scope)))
        .filter(|offer| public_model_id(&offer.model) == node.model && planner::offer_kind(offer) == Some(node.kind));
    let candidate = matching.next();
    let pin = candidate
        .filter(|offer| {
            offer.adapter == Adapter::Mock && matching.all(|offer| offer.adapter == Adapter::Mock)
        })
        .map(|offer| offer.id.clone());
    let fixture = pin.is_some();
    let prompt = text(node, progress, "prompt")?;
    let invalid = || ApiError::BadRequest("workflow setting is out of range".into());

    let mut messages = Vec::new();
    let workload = match node.kind {
        Kind::Tool => return Err(unavailable()),
        Kind::Text => {
            if node.settings.contains_key("system") {
                messages.push(Message::plain(
                    Role::System,
                    text(node, progress, "system")?,
                ));
            }
            messages.push(Message::plain(Role::User, prompt.clone()));
            Workload::Text {
                max_input_tokens: message_input_bound(&messages).ok_or_else(invalid)?,
                max_output_tokens: u32::try_from(number(node, progress, "max_output_tokens")?)
                    .map_err(|_| invalid())?,
            }
        }
        Kind::Image => Workload::Image {
            count: 1,
            aspect_ratio: text(node, progress, "aspect_ratio")?,
            max_relative_error_bps: None,
        },
        Kind::Video => Workload::Video {
            duration_seconds: u16::try_from(number(node, progress, "duration_seconds")?)
                .map_err(|_| invalid())?,
            aspect_ratio: text(node, progress, "aspect_ratio")?,
            resolution: text(node, progress, "resolution")?,
            max_relative_error_bps: None,
        },
    };
    let mut first_frame = None;
    if let Some(binding) = node.settings.get("first_frame") {
        let invalid = || ApiError::BadRequest("connect a compatible image for the first frame".into());
        let provider = runtime.providers.as_deref();
        let source = match binding {
            Binding::Step { step } => match progress.outputs.get(step) {
                Some(Output::Fixture { kind: ValueType::Image }) if fixture => None,
                Some(Output::Image { request_id }) if !fixture => {
                    Some(artifacts::read_verified(artifacts::ScopedDownload {
                        pool: &runtime.pool,
                        scope,
                        runtime: provider,
                        request: request_id,
                        slot: 0,
                    }).await?)
                }
                Some(Output::Tool { node, kind: ValueType::Image }) if !fixture => {
                    Some(tool_execution::read_artifact(&runtime.pool, &runtime.security, row, node).await?)
                }
                _ => return Err(invalid()),
            },
            Binding::Request { field } => match progress.inputs.get(field) {
                Some(Value::Upload(reference)) if !fixture => {
                    Some(uploads::read(runtime, row, &reference.upload_id).await?)
                }
                _ => return Err(invalid()),
            },
            _ => return Err(invalid()),
        };
        if let Some((bytes, media_type, _)) = source {
            if media_type != artifacts::PNG || bytes.len() > artifacts::MAX_BYTES {
                return Err(ApiError::BadRequest("the first frame must be a bounded PNG".into()));
            }
            first_frame = Some(bytes);
        }
    }
    let selection = if first_frame.is_some() {
        QuoteSelection::WorkflowFrame
    } else if pin.is_some() {
        QuoteSelection::WorkflowPinned
    } else if node.kind == Kind::Text {
        QuoteSelection::Chat
    } else {
        QuoteSelection::Automatic
    };
    let access = Access::workflow(scope, row.id.as_str().to_owned());
    let idempotency = step_key(&row.id, &node.id);
    let (accepted, _) = requests::submit_workflow(
        requests::WorkflowAdmission {
            pool: &runtime.pool,
            catalog: &runtime.catalog,
            providers: runtime.providers.as_deref(),
            access: &access,
            idempotency_key: &idempotency,
            run: &row.id,
            lease: row.lease,
            first_frame,
        },
        AdmissionInput {
            selection,
            pinned_offer_id: pin,
            input: QuoteInput {
                model: node.model.clone(),
                workload,
                provider: None,
                preferred_providers: None,
                max_total_usd_nanos: Some(node.max_usd_nanos),
                min_quality: None,
                min_usage_accounts: None,
                data_requirements: None,
            },
            client_ref: None,
            app_ref: Some(row.workflow_id.as_str().to_owned()),
            webhook_subscription_id: None,
            messages: (!fixture && node.kind == Kind::Text).then_some(messages),
            image_prompt: (!fixture && node.kind == Kind::Image).then(|| prompt.clone()),
            video_prompt: (!fixture && node.kind == Kind::Video).then_some(prompt),
            audio_prompt: None,
            mesh_prompt: None,
            chat_options: Default::default(),
            stream: false,
        },
    )
    .await?;
    Ok(accepted.id)
}

pub(super) fn step_key(run: &ids::WorkflowRunId, node: &str) -> String {
    format!("{run}:{node}")
}

fn text(node: &Node, progress: &State, field: &str) -> ApiResult<String> {
    let value = node
        .resolve(field, &progress.inputs, &progress.outputs)
        .map_err(|error| ApiError::BadRequest(error.into()))?;
    match value {
        Some(Value::Text(text))
            if text.len() <= super::graph::MAX_INPUT_BYTES && !text.trim().is_empty() =>
        {
            Ok(text)
        }
        _ => Err(ApiError::BadRequest(
            "a valid text value is required".into(),
        )),
    }
}

fn number(node: &Node, progress: &State, field: &str) -> ApiResult<i64> {
    let value = node
        .resolve(field, &progress.inputs, &progress.outputs)
        .map_err(|error| ApiError::BadRequest(error.into()))?;
    match value {
        Some(Value::Integer(number)) => Ok(number),
        _ => Err(ApiError::BadRequest(
            "a valid numeric value is required".into(),
        )),
    }
}
