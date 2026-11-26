use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use axum::{extract::{Path as RoutePath, Query, State}, Extension, Json};
use ids::{AccountId, GatewayProjectId};
use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::graph::{Graph, Node, ValueType};
use super::management::{ConsoleQuery, Project};
use super::Security;
use crate::{error::{ApiError, ApiResult}, middleware::AuthContext, state::AppState};
use crate::gateway_demo::{
    catalog::{digest, valid_identifier},
    providers::connector::{valid_origin, validate_token_env},
};

pub(super) const CONFIG: &str = "TOKENCRAFT_WORKFLOW_TOOL_BINDINGS";
const MAX_CONFIG: usize = 32 * 1024;
const MAX_MANIFEST: usize = 128 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    version: u8,
    bindings: Vec<BindingConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingConfig {
    name: String,
    revision: String,
    origin: String,
    token_env: String,
    manifest: String,
    account: AccountId,
    projects: BTreeSet<GatewayProjectId>,
    #[serde(default)]
    drain_only: bool,
}

#[derive(Default)]
pub(super) struct Bindings(BTreeMap<String, Binding>);

pub(super) struct Binding {
    name: String,
    digest: String,
    account: AccountId,
    projects: BTreeSet<GatewayProjectId>,
    drain_only: bool,
    manifest: Value,
    parsed: Manifest,
    origin: Url,
    authorization: header::HeaderValue,
    client: Client,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    plugin: String,
    tools: Vec<Tool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tool {
    id: String,
    revision: u32,
    label: String,
    inputs: BTreeMap<String, Input>,
    parameters: Parameters,
    output: Output,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MediaKind {
    Image,
    Video,
    Audio,
    Media,
    Metadata,
    Mesh,
    Document,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    kind: MediaKind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    kind: MediaKind,
    content_types: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Parameters {
    #[serde(rename = "type")]
    shape: String,
    properties: BTreeMap<String, Constraint>,
    required: BTreeSet<String>,
    #[serde(rename = "additionalProperties")]
    extra: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
enum Constraint {
    Integer {
        minimum: i64,
        maximum: i64,
        #[serde(rename = "multipleOf")]
        multiple_of: Option<i64>,
        default: Option<i64>,
    },
    Number {
        minimum: f64,
        maximum: f64,
        default: Option<f64>,
    },
    String {
        #[serde(rename = "enum")]
        choices: BTreeSet<String>,
        default: Option<String>,
    },
}

impl Bindings {
    pub(super) fn load() -> anyhow::Result<Self> {
        let Some(path) = std::env::var_os(CONFIG) else {
            return Ok(Self::default());
        };
        let path = Path::new(&path);
        let bytes = bounded_file(path, MAX_CONFIG)?;
        let config: Configuration = serde_json::from_slice(&bytes).context("read workflow tool bindings")?;
        ensure!(config.version == 1 && config.bindings.len() <= 32, "invalid workflow tool binding registry");
        let mut entries = BTreeMap::new();
        for entry in config.bindings {
            let binding = entry.build(path.parent().unwrap_or_else(|| Path::new(".")))?;
            ensure!(entries.insert(binding.name.clone(), binding).is_none(), "duplicate workflow tool binding name");
        }
        Ok(Self(entries))
    }

    fn visible<'a>(&'a self, account: &'a AccountId, project: &'a GatewayProjectId) -> impl Iterator<Item = &'a Binding> {
        self.0.values().filter(move |binding| {
            !binding.drain_only && &binding.account == account && binding.projects.contains(project)
        })
    }
}

impl Bindings {
    pub(super) fn validate(&self, graph: &Graph, account: &AccountId, project: &GatewayProjectId) -> ApiResult<()> {
        for node in &graph.nodes {
            if node.tool.is_some() {
                self.select(node, account, project, false)?;
            }
        }
        Ok(())
    }

    pub(super) fn select(
        &self, node: &Node, account: &AccountId, project: &GatewayProjectId, in_flight: bool,
    ) -> ApiResult<(&Binding, &Tool)> {
        let invalid = || ApiError::BadRequest(format!("{} needs a tool binding and operation approved for this project", node.title));
        let call = node.tool.as_ref().ok_or_else(invalid)?;
        let binding = self.0.values().find(|entry| {
            &entry.account == account && entry.projects.contains(project)
                && entry.digest == call.binding_revision && (in_flight || !entry.drain_only)
        }).ok_or_else(invalid)?;
        let tool = binding.parsed.tools.iter().find(|tool| {
            tool.id == node.model && tool.revision == call.revision
        }).ok_or_else(invalid)?;
        if call.output != tool.output.kind.value_type() || tool.inputs.len() != call.inputs.len()
            || tool.inputs.iter().any(|(name, input)| call.inputs.get(name) != Some(&input.kind.value_type()))
            || tool.parameters.required.iter().any(|name| !call.parameters.contains_key(name))
            || call.parameters.iter().any(|(name, value)| {
                tool.parameters.properties.get(name).is_none_or(|constraint| !constraint.matches(value))
            })
        {
            return Err(ApiError::BadRequest(format!("{} must match the reviewed tool's inputs, output, and parameters", node.title)));
        }
        Ok((binding, tool))
    }
}

impl Tool {
    pub(super) fn accepts_source(&self, slot: &str, mime: &str) -> bool {
        self.inputs.get(slot).is_some_and(|input| input.kind.accepts_type(mime))
    }

    pub(super) fn accepts_result(&self, mime: &str) -> bool {
        self.output.content_types.contains(mime)
    }
}

impl BindingConfig {
    fn build(self, location: &Path) -> anyhow::Result<Binding> {
        let origin = Url::parse(&self.origin).context("invalid workflow tool origin")?;
        ensure!(
            valid_identifier(&self.name) && valid_identifier(&self.revision) && valid_origin(&origin)
                && (1..=64).contains(&self.projects.len()),
            "invalid workflow tool connection or project list"
        );
        validate_token_env(&self.token_env)?;
        let credential = std::env::var(&self.token_env)
            .with_context(|| format!("set {} for tool binding {}", self.token_env, self.name))?;
        ensure!(
            (32..=128).contains(&credential.len())
                && credential.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'),
            "invalid workflow tool service token"
        );
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {credential}"))?;
        authorization.set_sensitive(true);
        let bytes = bounded_file(&location.join(self.manifest), MAX_MANIFEST)?;
        let (manifest, parsed) = checked_manifest(&bytes)?;
        let canonical = serde_json::to_string(&manifest)?;
        let identity = digest(format!(
            "workflow_tool/v1/{}/{}/{}/{}",
            self.name, self.revision, origin, digest(canonical),
        ));
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .build()?;
        Ok(Binding {
            name: self.name, digest: identity, account: self.account, projects: self.projects,
            drain_only: self.drain_only, manifest, parsed, origin, authorization, client,
        })
    }
}

fn checked_manifest(bytes: &[u8]) -> anyhow::Result<(Value, Manifest)> {
    let manifest: Value = serde_json::from_slice(bytes).context("read workflow tool manifest")?;
    // Keep JSON numbers in the JSON value representation before interpreting the tagged schema.
    let typed: Manifest = serde_json::from_value(manifest.clone()).context("validate workflow tool manifest")?;
    typed.validate()?;
    Ok((manifest, typed))
}

impl Manifest {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.version == 1 && valid_identifier(&self.plugin) && (1..=128).contains(&self.tools.len()),
            "unsupported workflow tool manifest"
        );
        let mut ids = BTreeSet::new();
        for tool in &self.tools {
            ensure!(
                (1..=96).contains(&tool.id.len())
                    && tool.id.split('.').all(valid_identifier)
                    && tool.revision > 0 && (1..=100).contains(&tool.label.len())
                    && ids.insert((&tool.id, tool.revision)),
                "invalid or duplicate workflow tool"
            );
            ensure!(
                (1..=8).contains(&tool.inputs.len())
                    && tool.inputs.iter().all(|(slot, input)| slot_name(slot) && !matches!(input.kind, MediaKind::Metadata)),
                "invalid workflow tool input slots"
            );
            ensure!(
                tool.output.kind.accepts(&tool.output.content_types),
                "invalid workflow tool output media types"
            );
            let parameters = &tool.parameters;
            ensure!(
                parameters.shape == "object" && !parameters.extra && parameters.properties.len() <= 32
                    && parameters.required.iter().all(|key| parameters.properties.contains_key(key))
                    && parameters.properties.iter().all(|(name, rule)| slot_name(name) && rule.valid()),
                "invalid workflow tool argument schema"
            );
        }
        Ok(())
    }
}

impl MediaKind {
    fn value_type(self) -> ValueType {
        match self {
            Self::Image => ValueType::Image, Self::Video => ValueType::Video,
            Self::Audio => ValueType::Audio, Self::Media => ValueType::Media,
            Self::Metadata => ValueType::Metadata, Self::Mesh => ValueType::Mesh,
            Self::Document => ValueType::Document,
        }
    }

    fn accepts(self, values: &BTreeSet<String>) -> bool {
        self.value_type() != ValueType::Media && !values.is_empty()
            && values.iter().all(|value| self.accepts_type(value))
    }

    fn accepts_type(self, value: &str) -> bool {
        let allowed: &[&str] = match self {
            Self::Image => &["image/png", "image/jpeg", "image/webp", "image/gif"],
            Self::Video => &["video/mp4", "video/webm", "video/quicktime"],
            Self::Audio => &["audio/wav", "audio/mpeg", "audio/flac", "audio/ogg"],
            Self::Metadata => &["application/json"],
            Self::Mesh => &["model/gltf-binary"],
            Self::Document => &["application/pdf", "text/plain", "text/vtt"],
            Self::Media => return [Self::Image, Self::Video, Self::Audio, Self::Mesh, Self::Document]
                .into_iter().any(|kind| kind.accepts_type(value)),
        };
        allowed.contains(&value)
    }
}

pub(super) fn upload_kind(content_type: &str) -> Option<ValueType> {
    [MediaKind::Image, MediaKind::Video, MediaKind::Audio, MediaKind::Mesh, MediaKind::Document]
        .into_iter()
        .find(|kind| kind.accepts_type(content_type))
        .map(MediaKind::value_type)
}

fn slot_name(value: &str) -> bool {
    (1..=32).contains(&value.len()) && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

impl Constraint {
    fn matches(&self, value: &Value) -> bool {
        match self {
            Self::Integer { minimum, maximum, multiple_of, .. } => value.as_i64().is_some_and(|number| {
                (*minimum..=*maximum).contains(&number) && multiple_of.is_none_or(|stride| number % stride == 0)
            }),
            Self::Number { minimum, maximum, .. } => value.as_f64().is_some_and(|number| (*minimum..=*maximum).contains(&number)),
            Self::String { choices, .. } => value.as_str().is_some_and(|value| choices.contains(value)),
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::Integer { minimum, maximum, multiple_of, default } => {
                minimum <= maximum && multiple_of.is_none_or(|stride| stride > 0)
                    && default.is_none_or(|value| (*minimum..=*maximum).contains(&value)
                        && multiple_of.is_none_or(|stride| stride > 0 && value % stride == 0))
            }
            Self::Number { minimum, maximum, default } => {
                minimum.is_finite() && maximum.is_finite() && minimum <= maximum
                    && default.is_none_or(|value| (*minimum..=*maximum).contains(&value))
            }
            Self::String { choices, default } => {
                (1..=64).contains(&choices.len()) && choices.iter().all(|choice| (1..=80).contains(&choice.len()))
                    && default.as_ref().is_none_or(|value| choices.contains(value))
            }
        }
    }
}

fn bounded_file(path: &Path, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "workflow tool file exceeds its byte limit");
    Ok(bytes)
}

impl Binding {
    pub(super) fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut url = self.origin.clone();
        url.set_path(path);
        self.client.request(method, url).header(header::AUTHORIZATION, self.authorization.clone())
    }

    async fn verify(&self) -> bool {
        let mut url = self.origin.clone();
        url.set_path("/v1/tools");
        let Ok(mut response) = self.client.get(url)
            .header(header::AUTHORIZATION, self.authorization.clone())
            .header(header::ACCEPT, "application/json")
            .send().await else { return false; };
        if response.status() != reqwest::StatusCode::OK
            || response.content_length().is_some_and(|size| size > MAX_MANIFEST as u64)
            || response.headers().get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next()) != Some("application/json")
        {
            return false;
        }
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if bytes.len() + chunk.len() <= MAX_MANIFEST => bytes.extend_from_slice(&chunk),
                Ok(None) => break,
                _ => return false,
            }
        }
        serde_json::from_slice::<Value>(&bytes).is_ok_and(|live| live == self.manifest)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolQuery {
    account_id: AccountId,
    #[serde(default)]
    verify: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ToolView {
    name: String,
    revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reachable: Option<bool>,
    manifest: Value,
}

pub(super) async fn discover(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<ToolQuery>,
    RoutePath(project): RoutePath<GatewayProjectId>,
    Extension(security): Extension<Arc<Security>>,
) -> ApiResult<Json<Vec<ToolView>>> {
    let console = ConsoleQuery { account_id: query.account_id };
    Project { pool: &state.pool, auth: &auth, query: &console, project: &project }
        .authorize(query.verify.is_some()).await?;
    let mut tools = Vec::new();
    for connector in security.tools.visible(&console.account_id, &project) {
        if query.verify.as_ref().is_some_and(|name| name != &connector.name) {
            continue;
        }
        tools.push(ToolView {
            name: connector.name.clone(),
            revision: connector.digest.clone(),
            reachable: if query.verify.is_some() { Some(connector.verify().await) } else { None },
            manifest: connector.manifest.clone(),
        });
    }
    if query.verify.is_some() && tools.is_empty() {
        return Err(ApiError::NotFound("workflow tool binding".into()));
    }
    Ok(Json(tools))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_plugin_has_a_valid_gateway_manifest() {
        let manifest = checked_manifest(include_bytes!(
            "../../../../../../plugins/tools/media/manifest.json"
        )).unwrap();
        assert_eq!(manifest.1.tools.len(), 23);
    }
}
