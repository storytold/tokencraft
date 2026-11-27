use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use reqwest::header::HeaderMap;
use serde::Deserialize;
use sha2::Sha256;

use crate::valid_id;

#[derive(Clone, Copy, Debug)]
pub struct WebhookOptions {
    pub now_seconds: Option<u64>,
    pub tolerance: Duration,
}

impl Default for WebhookOptions {
    fn default() -> Self {
        Self {
            now_seconds: None,
            tolerance: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VerificationError {
    #[error("a required webhook header is invalid")]
    Headers,
    #[error("the webhook timestamp is outside the allowed window")]
    Timestamp,
    #[error("a TokenCraft webhook signing secret is invalid")]
    Secret,
    #[error("no signature matched the webhook body")]
    Signature,
    #[error("the signed event does not match the TokenCraft v1 contract")]
    Payload,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum WebhookKind {
    #[serde(rename = "request.succeeded")]
    Succeeded,
    #[serde(rename = "request.canceled")]
    Canceled,
    #[serde(rename = "request.failed")]
    Failed,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WebhookData {
    pub request_id: String,
    #[serde(flatten)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

impl WebhookData {
    /// The optional client reference recorded when the request was admitted.
    pub fn client_ref(&self) -> Option<&str> {
        self.attributes
            .get("client_ref")?
            .as_str()
            .filter(|value| valid_client_ref(value))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct VerifiedWebhook {
    pub id: String,
    pub schema_version: u32,
    #[serde(rename = "type")]
    pub kind: WebhookKind,
    pub occurred_at: String,
    pub sequence: u32,
    pub data: WebhookData,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum WorkflowWebhookKind {
    #[serde(rename = "workflow.run.succeeded")]
    Succeeded,
    #[serde(rename = "workflow.run.failed")]
    Failed,
    #[serde(rename = "workflow.run.canceled")]
    Canceled,
    #[serde(rename = "workflow.run.expired")]
    Expired,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowWebhookStatus {
    Succeeded,
    Failed,
    Canceled,
    Expired,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkflowWebhookData {
    pub run_id: String,
    pub workflow_id: String,
    pub project_id: String,
    pub workflow_version: u32,
    pub status: WorkflowWebhookStatus,
    pub fetch_url: String,
    /// The customer reference supplied when the published run was invoked.
    pub client_ref: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct VerifiedWorkflowWebhook {
    pub id: String,
    pub schema_version: u32,
    #[serde(rename = "type")]
    pub kind: WorkflowWebhookKind,
    pub occurred_at: String,
    pub sequence: u64,
    pub data: WorkflowWebhookData,
}

/// Check the original received bytes before deserializing them into a v1 event.
pub fn verify_webhook(
    headers: &HeaderMap,
    body: &[u8],
    signing_secrets: &[&str],
    options: WebhookOptions,
) -> Result<VerifiedWebhook, VerificationError> {
    let id = verify_signature(headers, body, signing_secrets, options)?;
    let event: VerifiedWebhook =
        serde_json::from_slice(body).map_err(|_| VerificationError::Payload)?;
    if event.id != id
        || event.schema_version != 1
        || event.sequence == 0
        || event.occurred_at.is_empty()
        || !valid_id(&event.data.request_id, "req_")
        || event.data.attributes.get("client_ref").is_some_and(|value| {
            !value.is_null() && event.data.client_ref().is_none()
        })
    {
        return Err(VerificationError::Payload);
    }
    Ok(event)
}

/// Verify a signed terminal notification for a published workflow run.
pub fn verify_workflow_webhook(
    headers: &HeaderMap,
    body: &[u8],
    signing_secrets: &[&str],
    options: WebhookOptions,
) -> Result<VerifiedWorkflowWebhook, VerificationError> {
    let id = verify_signature(headers, body, signing_secrets, options)?;
    let event: VerifiedWorkflowWebhook =
        serde_json::from_slice(body).map_err(|_| VerificationError::Payload)?;
    let data = &event.data;
    let status = match event.kind {
        WorkflowWebhookKind::Succeeded => WorkflowWebhookStatus::Succeeded,
        WorkflowWebhookKind::Failed => WorkflowWebhookStatus::Failed,
        WorkflowWebhookKind::Canceled => WorkflowWebhookStatus::Canceled,
        WorkflowWebhookKind::Expired => WorkflowWebhookStatus::Expired,
    };
    if event.id != id
        || event.schema_version != 1
        || event.sequence == 0
        || event.occurred_at.is_empty()
        || !valid_id(&data.run_id, "wfr_")
        || !valid_id(&data.workflow_id, "wf_")
        || !valid_id(&data.project_id, "proj_")
        || data.workflow_version == 0
        || data.status != status
        || data
            .client_ref
            .as_deref()
            .is_some_and(|value| !valid_client_ref(value))
        || data.fetch_url != format!(
            "/v1/projects/{}/workflow-endpoints/{}/runs/{}",
            data.project_id, data.workflow_id, data.run_id
        )
    {
        return Err(VerificationError::Payload);
    }
    Ok(event)
}

fn valid_client_ref(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 64
        && bytes.next().is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

fn verify_signature<'a>(
    headers: &'a HeaderMap,
    body: &[u8],
    signing_secrets: &[&str],
    options: WebhookOptions,
) -> Result<&'a str, VerificationError> {
    let id = single_header(headers, "webhook-id")?;
    let timestamp = single_header(headers, "webhook-timestamp")?;
    let signature = single_header(headers, "webhook-signature")?;
    if !valid_id(id, "whd_")
        || timestamp.len() > 12
        || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
        || signature.len() > 1024
    {
        return Err(VerificationError::Headers);
    }
    let sent: u64 = timestamp.parse().map_err(|_| VerificationError::Headers)?;
    let now = match options.now_seconds {
        Some(now) => now,
        None => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| VerificationError::Timestamp)?
            .as_secs(),
    };
    if now.abs_diff(sent) > options.tolerance.as_secs() {
        return Err(VerificationError::Timestamp);
    }
    if body.len() > 1_048_576 {
        return Err(VerificationError::Payload);
    }
    if !(1..=4).contains(&signing_secrets.len()) {
        return Err(VerificationError::Secret);
    }
    let keys = signing_secrets
        .iter()
        .map(|secret| {
            secret
                .strip_prefix("whsec_")
                .and_then(decode_sha256)
                .ok_or(VerificationError::Secret)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signatures: Vec<_> = signature
        .split_ascii_whitespace()
        .filter_map(|token| token.strip_prefix("v1,").and_then(decode_sha256))
        .collect();

    let prefix = format!("{id}.{timestamp}.");
    let verified = keys.iter().any(|key| {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
        mac.update(prefix.as_bytes());
        mac.update(body);
        signatures
            .iter()
            .any(|candidate| mac.clone().verify_slice(candidate).is_ok())
    });
    if !verified {
        return Err(VerificationError::Signature);
    }

    Ok(id)
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<&'a str, VerificationError> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    values
        .next()
        .filter(|_| values.next().is_none())
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or(VerificationError::Headers)
}

fn decode_sha256(raw: &str) -> Option<[u8; 32]> {
    if raw.len() != 44 {
        return None;
    }
    STANDARD.decode(raw).ok()?.try_into().ok()
}
