use std::collections::BTreeMap;

use reqwest::header::{CACHE_CONTROL, CONTENT_TYPE};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::client::validate_idempotency_key;
use crate::content::{valid_content_type, SHA256_HEADER};
use crate::{valid_id, Error, TokenCraftClient, VerifiedArtifact, WaitOptions};

const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum WorkflowInput {
    Text(String),
    Number(serde_json::Number),
    Upload(WorkflowUploadRef),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowUploadRef {
    pub upload_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowUpload {
    pub upload_id: String,
    pub expires_at: String,
}

impl WorkflowUpload {
    pub fn input(&self) -> WorkflowInput {
        WorkflowInput::Upload(WorkflowUploadRef {
            upload_id: self.upload_id.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WorkflowInvokeOptions<'a> {
    pub idempotency_key: &'a str,
    /// Opaque customer reference used for usage attribution.
    pub client_ref: Option<&'a str>,
    /// A successful run from the same application, workflow version, and customer.
    pub reuse_run_id: Option<&'a str>,
}

impl<'a> WorkflowInvokeOptions<'a> {
    pub fn new(idempotency_key: &'a str) -> Self {
        Self {
            idempotency_key,
            client_ref: None,
            reuse_run_id: None,
        }
    }
}

#[derive(Serialize)]
struct Invocation<'a> {
    inputs: &'a BTreeMap<String, WorkflowInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_ref: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reuse_run_id: Option<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowReceipt {
    pub id: String,
    pub workflow_version: i32,
    pub status_url: String,
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    Queued,
    Running,
    Reconciling,
    Succeeded,
    Failed,
    Canceled,
    Expired,
}

impl WorkflowRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Canceled | Self::Expired
        )
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkflowRun {
    pub id: String,
    pub status: WorkflowRunStatus,
    pub workflow_version: i32,
    pub draft_revision: i64,
    pub created_at: String,
    pub client_ref: Option<String>,
    pub steps: Vec<WorkflowStep>,
    #[serde(default)]
    pub tools: BTreeMap<String, WorkflowToolStep>,
    pub reuse: Option<WorkflowReuse>,
    pub error: Option<String>,
    /// Absent when outputs are unavailable or the key lacks `outputs:read`.
    pub outputs: Option<BTreeMap<String, WorkflowOutput>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkflowStep {
    pub node: String,
    pub request: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkflowToolStep {
    pub request_id: String,
    pub status: String,
    pub admitted_at: String,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkflowReuse {
    pub source_run_id: String,
    pub steps: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowOutput {
    Text { text: String },
    Media { kind: String, download_url: String },
    Fixture { kind: String },
}

/// A published endpoint scoped to one project and workflow.
pub struct WorkflowEndpoint<'a> {
    client: &'a TokenCraftClient,
    path: String,
}

impl TokenCraftClient {
    /// Use an application key issued by this published workflow.
    pub fn workflow(
        &self,
        project_id: &str,
        workflow_id: &str,
    ) -> Result<WorkflowEndpoint<'_>, Error> {
        if !valid_id(project_id, "proj_") || !valid_id(workflow_id, "wf_") {
            return Err(Error::InvalidInput(
                "supply valid TokenCraft project and workflow IDs",
            ));
        }
        Ok(WorkflowEndpoint {
            client: self,
            path: format!("v1/projects/{project_id}/workflow-endpoints/{workflow_id}"),
        })
    }
}

impl WorkflowEndpoint<'_> {
    /// Fetch the active version's OpenAPI document. Requires `runs:submit`.
    pub async fn contract(&self) -> Result<Value, Error> {
        self.client
            .send(self.request(Method::GET, "openapi.json")?, None)
            .await
    }

    /// Stage media for a later invocation. Requires `runs:submit`.
    pub async fn upload(
        &self,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<WorkflowUpload, Error> {
        if !valid_content_type(content_type) || !(1..=MAX_FILE_BYTES).contains(&bytes.len()) {
            return Err(Error::InvalidInput(
                "supply a media type and 1 byte to 32 MiB of file content",
            ));
        }
        let checksum = format!("{:x}", Sha256::digest(&bytes));
        let request = self
            .request(Method::POST, "uploads")?
            .header(CONTENT_TYPE, content_type)
            .header(SHA256_HEADER, checksum)
            .body(bytes);
        let upload: WorkflowUpload = self.client.send(request, None).await?;
        if !valid_id(&upload.upload_id, "wfu_") {
            return Err(invalid_response(StatusCode::CREATED, None));
        }
        Ok(upload)
    }

    /// Invoke the endpoint. Reuse the key and exact inputs to recover an admitted run.
    pub async fn invoke(
        &self,
        inputs: &BTreeMap<String, WorkflowInput>,
        options: WorkflowInvokeOptions<'_>,
    ) -> Result<WorkflowReceipt, Error> {
        validate_idempotency_key(options.idempotency_key)?;
        if let Some(id) = options.reuse_run_id {
            self.run_path(id)?;
        }
        let request = self
            .request(Method::POST, "runs")?
            .header("Idempotency-Key", options.idempotency_key)
            .json(&Invocation {
                inputs,
                client_ref: options.client_ref,
                reuse_run_id: options.reuse_run_id,
            });
        let receipt: WorkflowReceipt = self.client.send_idempotent(request).await?;
        let expected = format!("/{}/runs/{}", self.path, receipt.id);
        if !valid_id(&receipt.id, "wfr_")
            || receipt.status_url != expected
            || receipt.workflow_version < 1
        {
            let status = if receipt.replayed {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            };
            return Err(invalid_response(status, None));
        }
        Ok(receipt)
    }

    /// Read the application's run. Requires `runs:read`.
    pub async fn get(&self, id: &str) -> Result<WorkflowRun, Error> {
        let run = self
            .client
            .send(self.request(Method::GET, &self.run_path(id)?)?, Some(id))
            .await?;
        check_run(&run, id)?;
        Ok(run)
    }

    /// Poll to a terminal status with a deadline and conditional requests.
    pub async fn wait(&self, id: &str, options: WaitOptions) -> Result<WorkflowRun, Error> {
        let request = self.request(Method::GET, &self.run_path(id)?)?;
        self.client
            .poll(request, id, options, |run: &WorkflowRun| {
                check_run(run, id)?;
                Ok(run.status.is_terminal())
            })
            .await
    }

    /// Stop further admission for the run. Requires `runs:cancel`.
    pub async fn cancel(&self, id: &str) -> Result<(), Error> {
        let response = self
            .request(Method::POST, &format!("{}/cancel", self.run_path(id)?))?
            .send()
            .await
            .map_err(|source| Error::from_http(source, Some(id.to_owned())))?;
        let (response, _) = TokenCraftClient::checked_response(response, Some(id)).await?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(invalid_response(response.status(), Some(id)));
        }
        Ok(())
    }

    /// Download a named output and verify its response checksum. Requires `outputs:read`.
    pub async fn download_output(&self, id: &str, name: &str) -> Result<VerifiedArtifact, Error> {
        if !(1..=48).contains(&name.len())
            || !name.as_bytes()[0].is_ascii_lowercase()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(Error::InvalidInput("supply a valid workflow output name"));
        }
        let request = self.request(
            Method::GET,
            &format!("{}/outputs/{name}", self.run_path(id)?),
        )?;
        self.client.download_verified(request, id, None, MAX_FILE_BYTES).await
    }

    fn run_path(&self, id: &str) -> Result<String, Error> {
        if !valid_id(id, "wfr_") {
            return Err(Error::InvalidInput(
                "supply a valid TokenCraft workflow run ID",
            ));
        }
        Ok(format!("runs/{id}"))
    }

    fn request(&self, method: Method, suffix: &str) -> Result<RequestBuilder, Error> {
        Ok(self
            .client
            .request(method, &format!("{}/{suffix}", self.path), true)?
            .header(CACHE_CONTROL, "no-store"))
    }
}

fn check_run(run: &WorkflowRun, id: &str) -> Result<(), Error> {
    if run.id != id || run.workflow_version < 1 {
        return Err(invalid_response(StatusCode::OK, Some(id)));
    }
    Ok(())
}

fn invalid_response(status: StatusCode, id: Option<&str>) -> Error {
    Error::Gateway {
        status,
        code: "invalid_response".into(),
        message: "the response did not match the published workflow operation".into(),
        request_id: id.map(str::to_owned),
    }
}
