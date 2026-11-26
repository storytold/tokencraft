use chrono::{DateTime, Utc};
use ids::{AccountId, GatewayProjectId, WorkflowAppId, WorkflowId, WorkflowKeyId, WorkflowRunId};

#[derive(Clone, Copy)]
pub struct Scope<'a> {
    pub account: &'a AccountId,
    pub project: &'a GatewayProjectId,
    pub workflow: &'a WorkflowId,
}

#[derive(serde::Serialize)]
pub struct Workflow {
    pub id: WorkflowId,
    pub title: String,
    pub draft: serde_json::Value,
    pub revision: i64,
    pub active_version: Option<i32>,
    pub paused: bool,
}

#[derive(serde::Serialize)]
pub struct Summary {
    pub id: WorkflowId,
    pub title: String,
    pub revision: i64,
    pub active_version: Option<i32>,
    pub paused: bool,
}

#[derive(serde::Serialize)]
pub struct Version {
    pub version: i32,
    pub draft_revision: i64,
    pub graph: serde_json::Value,
    pub published_at: DateTime<Utc>,
}

pub struct Run {
    pub id: WorkflowRunId,
    pub account_id: AccountId,
    pub project_id: GatewayProjectId,
    pub workflow_id: WorkflowId,
    pub actor_id: String,
    pub status: String,
    pub revision: i64,
    pub lease: i64,
    pub version: Option<i32>,
    pub draft_revision: i64,
    pub graph: serde_json::Value,
    pub content: Option<Vec<u8>>,
    pub deadline: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(serde::Serialize)]
pub struct RunSummary {
    pub id: WorkflowRunId,
    pub status: String,
    pub version: Option<i32>,
    pub draft_revision: i64,
    pub created_at: DateTime<Utc>,
}

pub struct Replay {
    pub id: WorkflowRunId,
    pub version: Option<i32>,
    pub intent_digest: String,
}

#[derive(serde::Serialize)]
pub struct AppKey {
    pub app_id: WorkflowAppId,
    pub name: String,
    pub scopes: Vec<String>,
    pub key_id: WorkflowKeyId,
    pub prefix: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub struct Credential {
    pub account_id: AccountId,
    pub project_id: GatewayProjectId,
    pub workflow_id: WorkflowId,
    pub app_id: WorkflowAppId,
    pub scopes: Vec<String>,
}

mod list_workflows;
pub use list_workflows::*;

mod find_workflow;
pub use find_workflow::*;

mod lock_workflow;
pub use lock_workflow::*;

mod create_workflow;
pub use create_workflow::*;

mod save_draft;
pub use save_draft::*;

mod list_versions;
pub use list_versions::*;

mod find_version;
pub use find_version::*;

mod next_version;
pub use next_version::*;

mod insert_version;
pub use insert_version::*;

mod configure_endpoint;
pub use configure_endpoint::*;

mod has_passed_test;
pub use has_passed_test::*;

mod find_replay;
pub use find_replay::*;

mod count_active_runs;
pub use count_active_runs::*;

mod create_run;
pub use create_run::*;

mod consume_budget;
pub use consume_budget::*;

mod list_runs;
pub use list_runs::*;

mod find_run;
pub use find_run::*;

mod claim_run;
pub use claim_run::*;

mod checkpoint_run;
pub use checkpoint_run::*;

mod cancel_run;
pub use cancel_run::*;

mod lock_active_run;
pub use lock_active_run::*;

mod forget_expired_content;
pub use forget_expired_content::*;

mod list_keys;
pub use list_keys::*;

mod find_credential;
pub use find_credential::*;

mod insert_app;
pub use insert_app::*;

mod insert_key;
pub use insert_key::*;

mod revoke_key;
pub use revoke_key::*;

mod tool_artifact;
pub use tool_artifact::*;

mod upload;
pub use upload::*;

mod rollout;
pub use rollout::{tool_dependencies, ToolDependency};
