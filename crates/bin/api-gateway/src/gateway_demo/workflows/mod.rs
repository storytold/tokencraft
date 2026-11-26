use std::ffi::OsStr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use ring::hmac;

use crate::state::AppState;

use super::sealed::SealedBytes;
use super::DemoConfig;

mod activity;
mod endpoints;
mod execution;
mod graph;
mod management;
mod planner;
mod runner;
mod tool_plugins;
mod tool_execution;
mod uploads;

pub(super) struct Security {
    payloads: SealedBytes,
    key_mac: hmac::Key,
    tools: tool_plugins::Bindings,
}

impl Security {
    pub fn load(allow_demo_payload_key: bool) -> anyhow::Result<Option<Arc<Self>>> {
        const WORKFLOW_KEY: &str = "TOKENCRAFT_WORKFLOW_KEY";
        const DEMO_PAYLOAD_KEY: &str = "TOKENCRAFT_DEMO_PAYLOAD_KEY";
        let demo_payload_key = allow_demo_payload_key
            .then(|| std::env::var_os(DEMO_PAYLOAD_KEY))
            .flatten();
        let secret = if let Some(value) = std::env::var_os(WORKFLOW_KEY) {
            decode_key(&value, WORKFLOW_KEY)?
        } else if let Some(value) = demo_payload_key {
            demo_workflow_key(&decode_key(&value, DEMO_PAYLOAD_KEY)?)
        } else {
            anyhow::ensure!(std::env::var_os(tool_plugins::CONFIG).is_none(), "configure a workflow key before attaching workflow tools");
            return Ok(None);
        };
        let mut security = Self::new(&secret);
        security.tools = tool_plugins::Bindings::load()?;
        Ok(Some(Arc::new(security)))
    }

    fn new(secret: &[u8; 32]) -> Self {
        Self {
            payloads: SealedBytes::new(secret, b"tokencraft/workflows/content/1"),
            key_mac: hmac::Key::new(hmac::HMAC_SHA256, secret),
            tools: tool_plugins::Bindings::default(),
        }
    }

    fn digest(&self, value: &[u8]) -> String {
        hex::encode(hmac::sign(&self.key_mac, value))
    }
}

fn decode_key(value: &OsStr, variable: &str) -> anyhow::Result<[u8; 32]> {
    let mut key = [0; 32];
    let value = value
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{variable} must contain 64 hex digits"))?;
    hex::decode_to_slice(value, &mut key)
        .map_err(|_| anyhow::anyhow!("{variable} must contain 64 hex digits"))?;
    Ok(key)
}

fn demo_workflow_key(payload_key: &[u8; 32]) -> [u8; 32] {
    // Keep the derivation used by the original demo launcher for existing runs and app keys.
    let root = hmac::Key::new(hmac::HMAC_SHA256, payload_key);
    hmac::sign(&root, b"tokencraft/workflows/demo/1")
        .as_ref()
        .try_into()
        .expect("HMAC-SHA256 produces 32 bytes")
}

#[cfg(test)]
mod tests {
    use super::{demo_workflow_key, Security};

    #[test]
    fn derived_demo_key_matches_existing_launcher() {
        let security = Security::new(&demo_workflow_key(&[0x42; 32]));
        assert_eq!(
            security.digest(b"workflow-key-fixture"),
            "b413806fd0f211b9fb105c17e3f1787570069ee858d5ad96db598d062431c589"
        );
    }
}

pub(super) fn router(security: Arc<Security>) -> Router<AppState> {
    Router::new()
        .route("/v1/workflows/capabilities", get(management::capabilities))
        .route("/v1/workflows/plan", post(management::plan))
        .route("/v1/projects/:project/workflow-tools", get(tool_plugins::discover))
        .route(
            "/v1/projects/:project/workflow-activity",
            get(activity::list),
        )
        .route(
            "/v1/projects/:project/workflow-activity/parents/:request",
            get(activity::parent),
        )
        .route(
            "/v1/projects/:project/workflow-activity/runs/:run/children",
            get(activity::children),
        )
        .route(
            "/v1/projects/:project/workflows",
            get(management::list).post(management::create),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow",
            get(management::read).put(management::save),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow/versions",
            get(management::versions).post(management::publish),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow/activation",
            post(management::activation),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow/test-runs",
            post(endpoints::test),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow/runs",
            get(endpoints::list_runs),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow/apps",
            get(endpoints::keys).post(endpoints::create_app),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow/apps/:app/keys",
            post(endpoints::rotate),
        )
        .route(
            "/v1/projects/:project/workflows/:workflow/keys/:key",
            delete(endpoints::revoke),
        )
        .route(
            "/v1/projects/:project/workflow-endpoints/:workflow/uploads",
            post(endpoints::upload),
        )
        .route(
            "/v1/projects/:project/workflow-endpoints/:workflow/runs",
            post(endpoints::invoke),
        )
        .route(
            "/v1/projects/:project/workflow-endpoints/:workflow/openapi.json",
            get(endpoints::contract),
        )
        .route(
            "/v1/projects/:project/workflow-endpoints/:workflow/runs/:run",
            get(endpoints::status),
        )
        .route(
            "/v1/projects/:project/workflow-endpoints/:workflow/runs/:run/cancel",
            post(endpoints::cancel),
        )
        .route(
            "/v1/projects/:project/workflow-endpoints/:workflow/runs/:run/outputs/:name",
            get(endpoints::asset),
        )
        .layer(Extension(security))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

pub(super) fn spawn(pool: db::PgPool, config: &DemoConfig) {
    if let Some(security) = &config.workflows {
        runner::spawn(runner::Runtime {
            pool,
            security: Arc::clone(security),
            catalog: Arc::clone(&config.catalog),
            providers: config.providers.clone(),
            readiness: Arc::clone(&config.readiness),
        });
    }
}
