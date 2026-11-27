use std::{collections::BTreeSet, path::PathBuf};

use anyhow::{ensure, Context, Result};
use ids::{AccountId, GatewayProjectId};
use serde::{Deserialize, Serialize};

pub const ENV: &str = "TOKENCRAFT_WORKFLOW_TOOL_BINDINGS";
pub const MAX_REGISTRY_BYTES: usize = 32 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 128 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    version: u8,
    pub bindings: Vec<Binding>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub name: String,
    pub revision: String,
    pub origin: String,
    pub token_env: String,
    pub manifest: PathBuf,
    pub account: AccountId,
    pub projects: BTreeSet<GatewayProjectId>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drain_only: bool,
}

impl Registry {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let registry: Self =
            serde_json::from_slice(bytes).context("read workflow tool bindings")?;
        ensure!(
            registry.version == 1 && registry.bindings.len() <= 32,
            "invalid workflow tool binding registry"
        );
        Ok(registry)
    }
}
