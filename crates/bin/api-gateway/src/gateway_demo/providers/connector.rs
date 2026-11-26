use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
    time::Duration,
};

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{codecs::png::PngDecoder, ImageDecoder, Limits};
use reqwest::{header, Client, StatusCode, Url};
use serde::{Deserialize, Serialize};

use super::super::{
    artifacts::{self, FetchedImage, FetchedMedia},
    catalog::{digest, valid_identifier, Adapter, Catalog},
    quote::Workload,
};
use super::{
    audio_format, mesh_format,
    completion::{CompletionGrant, CompletionTokens},
    payloads::Payloads,
    queue::{read_body, Observation, RateLimit, Submission},
    video,
};
use db_queries::gateway_requests::{queued_runs::{ClaimedQueuePoll, ProviderCharge}, ClaimedProviderRun};

use crate::middleware::trace_context::attach_provider_trace;

const CONFIG: &str = "TOKENCRAFT_CONNECTORS";
const VERSION: u8 = 1;
const MAX_CONFIG_BYTES: u64 = 32 * 1024;
pub(in crate::gateway_demo) const MAX_BINDINGS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    bindings: Vec<ConfigBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigBinding {
    name: String,
    revision: String,
    origin: String,
    token_env: String,
    #[serde(default)]
    completion_webhooks: bool,
    #[serde(default)]
    drain_only: bool,
    #[serde(default)]
    tenant_keys: bool,
    #[serde(default)]
    accepts_workflow_frames: bool,
}

#[derive(Default)]
pub(super) struct Connectors {
    bindings: BTreeMap<String, Connector>,
}

pub(super) struct Connector {
    pub digest: String,
    origin: Url,
    client: Client,
    authorization: header::HeaderValue,
    completion_webhooks: bool,
    drain_only: bool,
    pub(super) tenant_keys: bool,
    pub(super) accepts_workflow_frames: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::gateway_demo) enum MediaInput {
    Image {
        prompt: String,
        count: u16,
        aspect_ratio: String,
    },
    Video {
        prompt: String,
        duration_seconds: u16,
        aspect_ratio: String,
        resolution: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_frame: Option<WorkflowFrame>,
    },
    Audio {
        prompt: String,
        duration_seconds: u16,
        format: String,
    },
    Mesh {
        prompt: String,
        format: String,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::gateway_demo) struct WorkflowFrame {
    pub media_type: String,
    pub data_base64: String,
}

impl WorkflowFrame {
    pub(in crate::gateway_demo) fn png(bytes: &[u8]) -> Self {
        Self { media_type: artifacts::PNG.into(), data_base64: STANDARD.encode(bytes) }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub(in crate::gateway_demo) enum StoredInput {
    StagedFrame { frame_input: MediaInput },
    Inline(MediaInput),
}

pub(super) async fn open_input(
    pool: &db::PgPool,
    payloads: &Payloads,
    run: &ClaimedProviderRun,
) -> sqlx::Result<Option<MediaInput>> {
    let id = run.request_id.as_str();
    let Some(input) = payloads.open(id, "input", &run.request_payload)
        .and_then(|bytes| serde_json::from_slice::<StoredInput>(&bytes).ok())
    else {
        return Ok(None);
    };
    match input {
        StoredInput::Inline(input) => Ok(Some(input)),
        StoredInput::StagedFrame { mut frame_input } => {
            let MediaInput::Video { first_frame, .. } = &mut frame_input else {
                return Ok(None);
            };
            let png = db_queries::gateway_requests::workflow_frames::read_for_submit(pool, id, run.lease)
                .await?
                .and_then(|bytes| payloads.open(id, "workflow-frame", &bytes));
            let Some(png) = png else { return Ok(None) };
            *first_frame = Some(WorkflowFrame::png(&png));
            Ok(Some(frame_input))
        }
    }
}

#[derive(Serialize)]
struct Submit<'a> {
    version: u8,
    request_id: &'a str,
    model: &'a str,
    input: MediaInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion: Option<CompletionGrant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitReply {
    version: u8,
    request_id: String,
    outcome: SubmitState,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum SubmitState {
    Accepted { handle: String },
    Rejected,
    Unavailable { retry_after_ms: Option<u16> },
    Uncertain,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupReply {
    version: u8,
    request_id: String,
    handle: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PollReply {
    version: u8,
    handle: String,
    status: PollState,
    provider_charge: Option<serde_json::Value>,
}

fn provider_charge(value: Option<serde_json::Value>) -> Option<ProviderCharge> {
    let charge: ProviderCharge = serde_json::from_value(value?).ok()?;
    let unit = charge.unit.as_bytes();
    let (whole, fraction) = charge.quantity.split_once('.').map_or((charge.quantity.as_str(), None),
        |(whole, fraction)| (whole, Some(fraction)));
    let valid = unit.len() <= 32 && unit.first().is_some_and(u8::is_ascii_lowercase)
        && unit.iter().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        && (1..=15).contains(&whole.len()) && whole.bytes().all(|byte| byte.is_ascii_digit())
        && (whole.len() == 1 || !whole.starts_with('0'))
        && fraction.is_none_or(|digits| (1..=9).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit()));
    valid.then_some(charge)
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PollState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl Connectors {
    pub(super) fn load(catalog: &Catalog) -> anyhow::Result<Self> {
        let offers = catalog
            .offers
            .iter()
            .filter(|offer| offer.adapter == Adapter::ConnectorQueue);
        let Some(path) = std::env::var_os(CONFIG) else {
            ensure!(
                offers.count() == 0,
                "set TOKENCRAFT_CONNECTORS for connector offers"
            );
            return Ok(Self::default());
        };
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .context("open connector configuration")?
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_CONFIG_BYTES,
            "connector configuration exceeds 32 KiB"
        );
        let config: Config = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid connector configuration"))?;
        ensure!(
            config.version == VERSION && (1..=MAX_BINDINGS).contains(&config.bindings.len()),
            "connectors require version 1 and one to 64 bindings"
        );

        let mut bindings = BTreeMap::new();
        for binding in config.bindings {
            ensure!(
                !bindings.contains_key(&binding.name),
                "duplicate connector binding name"
            );
            let name = binding.name.clone();
            bindings.insert(name, binding.build()?);
        }
        let registry = Self { bindings };
        for offer in offers {
            let name = offer.local_binding.as_deref().context("connector offer requires a binding name")?;
            ensure!(
                registry.for_offer(name).is_some(),
                "offer {} requires active queue binding {name}", offer.id
            );
        }
        Ok(registry)
    }

    pub(super) fn for_offer(&self, name: &str) -> Option<&Connector> {
        self.bindings.get(name).filter(|entry| !entry.drain_only)
    }

    pub(super) fn binding_names(&self) -> impl Iterator<Item = &str> {
        self.bindings.keys().map(String::as_str)
    }

    pub(super) fn name_by_digest(&self, digest: &str) -> Option<&str> {
        self.bindings.iter().find(|(_, connector)| connector.digest == digest)
            .map(|(name, _)| name.as_str())
    }

    pub(super) fn by_digest(&self, digest: &str) -> Option<&Connector> {
        self.bindings.values().find(|entry| entry.digest == digest)
    }

    pub(super) fn digests(&self) -> impl Iterator<Item = &str> {
        self.bindings.values().map(|entry| entry.digest.as_str())
    }

    pub(super) fn worker_destinations(&self) -> impl Iterator<Item = (&str, String)> {
        self.bindings.values().map(|entry| {
            (entry.digest.as_str(), entry.origin.origin().ascii_serialization())
        })
    }

    pub(super) fn completion_digests(&self) -> impl Iterator<Item = &str> {
        self.bindings.values().filter(|entry| entry.completion_webhooks).map(|entry| entry.digest.as_str())
    }

    pub(super) fn active_digests(&self) -> impl Iterator<Item = &str> {
        self.bindings
            .values()
            .filter(|entry| !entry.drain_only)
            .map(|entry| entry.digest.as_str())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    pub(super) fn has_tenant_keys(&self) -> bool {
        self.bindings.values().any(|entry| entry.tenant_keys)
    }
}

impl ConfigBinding {
    fn build(self) -> anyhow::Result<Connector> {
        let origin = Url::parse(&self.origin)
            .map_err(|_| anyhow::anyhow!("invalid connector origin"))?;
        ensure!(
            valid_identifier(&self.name)
                && valid_identifier(&self.revision)
                && valid_origin(&origin),
            "invalid connector name, revision, or origin"
        );
        validate_token_env(&self.token_env)?;
        let secret = std::env::var(&self.token_env)
            .map_err(|_| anyhow::anyhow!("set {} for queue binding {}", self.token_env, self.name))?;
        ensure!(
            (24..=128).contains(&secret.len()) && valid_handle(&secret)
                && (!self.tenant_keys || secret.len() >= 32),
            "invalid service token for queue binding {}", self.name
        );
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {secret}"))?;
        authorization.set_sensitive(true);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(4))
            .build()?;
        let completion = if self.completion_webhooks { "/completion-v1" } else { "" };
        let credential = if self.tenant_keys { "/tenant-keys-v1" } else { "" };
        let frame = if self.accepts_workflow_frames { "/workflow-frame-v1" } else { "" };
        let digest = digest(format!(
            "connector_queue/{VERSION}/{}/{}/{}{completion}{credential}{frame}",
            self.name, self.revision, origin
        ));
        Ok(Connector {
            digest,
            origin,
            client,
            authorization,
            completion_webhooks: self.completion_webhooks,
            drain_only: self.drain_only,
            tenant_keys: self.tenant_keys,
            accepts_workflow_frames: self.accepts_workflow_frames,
        })
    }
}

pub(in crate::gateway_demo) fn validate_token_env(name: &str) -> anyhow::Result<()> {
    ensure!(
        name.len() <= 96 && name.strip_prefix("TOKENCRAFT_CONNECTOR_TOKEN_").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte|
                byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        }),
        "use a TOKENCRAFT_CONNECTOR_TOKEN_ environment variable name"
    );
    Ok(())
}

pub(in crate::gateway_demo) fn valid_origin(origin: &Url) -> bool {
    let transport_allowed = match (origin.scheme(), origin.host()) {
        ("https", Some(_)) => true,
        ("http", Some(_)) => origin
            .host_str()
            .and_then(|host| {
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|ip| ip.is_loopback()),
        _ => false,
    };
    transport_allowed
        && origin.path() == "/"
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.query().is_none()
        && origin.fragment().is_none()
}

fn valid_handle(handle: &str) -> bool {
    (1..=128).contains(&handle.len())
        && handle
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl Connector {
    fn authorize(&self, request: reqwest::RequestBuilder, key: Option<&header::HeaderValue>)
        -> reqwest::RequestBuilder
    {
        let request = request.header(header::AUTHORIZATION, self.authorization.clone());
        match key {
            Some(key) => request.header("x-tokencraft-upstream-key", key.clone()),
            None => request,
        }
    }

    fn endpoint(&self, segments: &[&str]) -> Url {
        let mut endpoint = self.origin.clone();
        endpoint
            .path_segments_mut()
            .expect("validated connector origin")
            .pop_if_empty()
            .extend(segments);
        endpoint
    }

    pub(super) async fn submit(
        &self, run: &ClaimedProviderRun, input: MediaInput, tokens: &CompletionTokens,
        key: Option<&header::HeaderValue>,
    ) -> Submission {
        let request_id = run.request_id.as_str();
        let request = attach_provider_trace(
            self.authorize(self.client.post(self.endpoint(&["v1", "jobs"])), key),
            run.trace_id.as_deref(),
        )
        .header("idempotency-key", request_id)
        .json(&Submit {
            version: VERSION,
            request_id,
            model: &run.upstream_model,
            input,
            completion: self.completion_webhooks.then(|| tokens.issue(request_id)),
        });
        let Some(reply) = json::<SubmitReply>(request)
            .await
            .filter(|reply| reply.version == VERSION && reply.request_id == request_id)
        else {
            return Submission::Uncertain("connector_submission_unconfirmed");
        };
        match reply.outcome {
            SubmitState::Accepted { handle } if valid_handle(&handle) => {
                Submission::Accepted(handle)
            }
            SubmitState::Rejected => Submission::Rejected("connector_rejected"),
            SubmitState::Unavailable { retry_after_ms } => {
                let retry_after_ms = retry_after_ms
                    .map(i32::from)
                    .filter(|delay| *delay <= 2_000);
                Submission::Unavailable(RateLimit {
                    retry_after_ms,
                    cooldown_ms: retry_after_ms.unwrap_or(2_000).max(50),
                })
            }
            _ => Submission::Uncertain("connector_submission_unconfirmed"),
        }
    }

    pub(super) async fn lookup(&self, request_id: &str, trace_id: Option<&str>) -> Option<String> {
        let url = self.endpoint(&["v1", "jobs", "by-request", request_id]);
        let request = attach_provider_trace(self.authorize(self.client.get(url), None), trace_id);
        let reply: LookupReply = json(request).await?;
        (reply.version == VERSION && reply.request_id == request_id && valid_handle(&reply.handle))
            .then_some(reply.handle)
    }

    pub(super) async fn poll(&self, task: &ClaimedQueuePoll, key: Option<&header::HeaderValue>)
        -> Observation<Option<ProviderCharge>>
    {
        let handle = task.handle.as_str();
        if !valid_handle(handle) {
            return Observation::Retry;
        }
        let request = attach_provider_trace(
            self.authorize(self.client.get(self.endpoint(&["v1", "jobs", handle])), key),
            task.trace_id.as_deref(),
        )
        .header("x-tokencraft-provider-charge", "1");
        let Some(reply) = json::<PollReply>(request)
            .await
            .filter(|reply| reply.version == VERSION && reply.handle == handle)
        else {
            return Observation::Retry;
        };
        match reply.status {
            PollState::Queued => Observation::StillRunning("in_queue"),
            PollState::Running => Observation::StillRunning("in_progress"),
            PollState::Succeeded => Observation::Ready(provider_charge(reply.provider_charge)),
            PollState::Failed => Observation::Rejected("connector_job_failed"),
        }
    }

    pub(super) async fn fetch(&self, task: &ClaimedQueuePoll, key: Option<&header::HeaderValue>) -> Option<FetchedMedia> {
        tokio::time::timeout(Duration::from_secs(11), self.fetch_media(task, key))
            .await
            .ok()?
    }

    async fn fetch_media(&self, task: &ClaimedQueuePoll, key: Option<&header::HeaderValue>) -> Option<FetchedMedia> {
        let workload: Workload = serde_json::from_value(task.workload.clone()?).ok()?;
        let handle = &task.handle;
        if !valid_handle(handle) {
            return None;
        }
        match workload {
            Workload::Image { count, .. } if (1..=8).contains(&count) => {
                let ratio = task.applied_aspect_ratio.as_deref().and_then(parse_ratio)?;
                let mut images = Vec::with_capacity(usize::from(count));
                for slot in 0..i16::try_from(count).ok()? {
                    let bytes = self.artifact(task, slot, ArtifactSpec::Png, key).await?;
                    images.push(
                        tokio::task::spawn_blocking(move || png(bytes, slot, ratio))
                            .await
                            .ok()??,
                    );
                }
                Some(FetchedMedia::Images(images))
            }
            Workload::Video {
                duration_seconds,
                resolution,
                ..
            } => {
                let ratio = task.applied_aspect_ratio.as_deref().and_then(parse_ratio)?;
                let expected_size = video_size(ratio, &resolution)?;
                let bytes = self.artifact(task, 0, ArtifactSpec::Mp4, key).await?;
                video::parse(bytes, duration_seconds, expected_size).map(FetchedMedia::Video)
            }
            Workload::Audio {
                duration_seconds,
                format,
            } if format == "wav" => {
                let bytes = self.artifact(task, 0, ArtifactSpec::Wav, key).await?;
                audio_format::parse(bytes, duration_seconds).map(FetchedMedia::Audio)
            }
            Workload::Mesh { format } if format == "glb" => {
                let bytes = self.artifact(task, 0, ArtifactSpec::Glb, key).await?;
                mesh_format::parse(bytes).map(FetchedMedia::Mesh)
            }
            _ => None,
        }
    }

    async fn artifact(
        &self,
        task: &ClaimedQueuePoll,
        slot: i16,
        spec: ArtifactSpec,
        key: Option<&header::HeaderValue>,
    ) -> Option<Vec<u8>> {
        let url = self.endpoint(&["v1", "jobs", &task.handle, "artifacts", &slot.to_string()]);
        let response = attach_provider_trace(
            self.authorize(self.client.get(url), key),
            task.trace_id.as_deref(),
        )
        .send()
        .await
        .ok()?;
        let (media_type, limit) = spec.contents();
        if !successful_media(&response, media_type) {
            return None;
        }
        read_body(response, limit).await.ok()
    }
}

enum ArtifactSpec {
    Png,
    Mp4,
    Wav,
    Glb,
}

impl ArtifactSpec {
    fn contents(self) -> (&'static str, usize) {
        match self {
            Self::Png => (artifacts::PNG, artifacts::MAX_BYTES),
            Self::Mp4 => (artifacts::MP4, artifacts::MAX_VIDEO_BYTES),
            Self::Wav => (artifacts::WAV, artifacts::MAX_VIDEO_BYTES),
            Self::Glb => (artifacts::GLB, artifacts::MAX_MESH_BYTES),
        }
    }
}

fn successful_media(response: &reqwest::Response, expected: &str) -> bool {
    response.status() == StatusCode::OK
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()?.split(';').next())
            .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(expected))
}

async fn json<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Option<T> {
    let response = request.send().await.ok()?;
    if !successful_media(&response, "application/json") {
        return None;
    }
    serde_json::from_slice(&read_body(response, 16 * 1024).await.ok()?).ok()
}

fn parse_ratio(ratio: &str) -> Option<(u32, u32)> {
    let (width, height) = ratio.split_once(':')?;
    let (width, height): (u32, u32) = (width.parse().ok()?, height.parse().ok()?);
    ((1..=100).contains(&width) && (1..=100).contains(&height)).then_some((width, height))
}

pub(in crate::gateway_demo) fn supports_video(ratio: &str, resolution: &str) -> bool {
    parse_ratio(ratio)
        .and_then(|ratio| video_size(ratio, resolution))
        .is_some()
}

fn video_size((width, height): (u32, u32), label: &str) -> Option<(u32, u32)> {
    let short: u32 = label.strip_suffix('p')?.parse().ok()?;
    if !(240..=2160).contains(&short) || width == 0 || height == 0 {
        return None;
    }
    let long = short * width.max(height) / width.min(height);
    let long = long - long % 2;
    if long > 4096 {
        return None;
    }
    Some(if width >= height {
        (long, short)
    } else {
        (short, long)
    })
}

fn png(bytes: Vec<u8>, slot: i16, (ratio_width, ratio_height): (u32, u32)) -> Option<FetchedImage> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    let decoder = PngDecoder::with_limits(Cursor::new(&bytes), limits).ok()?;
    let (width, height) = decoder.dimensions();
    if width.checked_mul(ratio_height)? != height.checked_mul(ratio_width)? {
        return None;
    }
    let mut pixels = vec![0; usize::try_from(decoder.total_bytes()).ok()?];
    decoder.read_image(&mut pixels).ok()?;
    Some(FetchedImage {
        slot,
        width,
        height,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_requires_https_or_literal_loopback_and_no_credentials() {
        for origin in [
            "https://connector.example/",
            "http://127.0.0.1:9999/",
            "http://[::1]:9999/",
        ] {
            assert!(valid_origin(&Url::parse(origin).unwrap()), "{origin}");
        }
        for origin in [
            "http://connector.example/",
            "http://localhost:9999/",
            "https://user@connector.example/",
            "https://connector.example/path",
            "https://connector.example/?port=1",
            "https://connector.example/#fragment",
        ] {
            assert!(!valid_origin(&Url::parse(origin).unwrap()), "{origin}");
        }
        for handle in ["../../jobs", "a%2Fb", "a/b", ""] {
            assert!(!valid_handle(handle));
        }
    }

    #[test]
    fn wire_requires_correlation_fields_and_known_status() {
        let reply: SubmitReply = serde_json::from_str(
            r#"{"version":1,"request_id":"req_example","outcome":{"status":"accepted","handle":"job_1"}}"#
        ).unwrap();
        assert!(matches!(reply.outcome, SubmitState::Accepted { handle } if handle == "job_1"));
        assert!(serde_json::from_str::<SubmitReply>(
            r#"{"version":1,"outcome":{"status":"accepted","handle":"job_1"}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<PollReply>(
            r#"{"version":1,"handle":"job_1","status":"other"}"#
        )
        .is_err());
    }

    #[test]
    fn staged_frames_and_legacy_inline_frames_have_distinct_queue_payloads() {
        let legacy = r#"{"kind":"video","prompt":"clip","duration_seconds":5,"aspect_ratio":"4:3","resolution":"540p","first_frame":{"media_type":"image/png","data_base64":"AQID"}}"#;
        let input: StoredInput = serde_json::from_str(legacy).unwrap();
        assert!(matches!(input, StoredInput::Inline(MediaInput::Video {
            first_frame: Some(frame), ..
        }) if frame.data_base64 == "AQID"));

        let pending = StoredInput::StagedFrame { frame_input: MediaInput::Video {
            prompt: "clip".into(), duration_seconds: 5, aspect_ratio: "4:3".into(),
            resolution: "540p".into(), first_frame: None,
        }};
        let encoded = serde_json::to_vec(&pending).unwrap();
        assert!(serde_json::from_slice::<MediaInput>(&encoded).is_err());
        assert!(matches!(serde_json::from_slice::<StoredInput>(&encoded).unwrap(),
            StoredInput::StagedFrame { frame_input: MediaInput::Video { first_frame: None, .. } }
        ));
    }

    #[test]
    fn video_size_uses_short_edge_in_both_orientations() {
        assert_eq!(video_size((16, 9), "720p"), Some((1280, 720)));
        assert_eq!(video_size((9, 16), "720p"), Some((720, 1280)));
        assert_eq!(video_size((100, 1), "720p"), None);
    }
}
