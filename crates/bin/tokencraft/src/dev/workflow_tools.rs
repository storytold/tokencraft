use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
};

use anyhow::{Context, Result};
use config::workflow_tools::{Registry, ENV, MAX_MANIFEST_BYTES, MAX_REGISTRY_BYTES};
use tokio::process::Command;

use super::profile::{read_config, read_credentials, register, QUEUE_TOKEN_PREFIX};
use crate::state::{write_private, PrivateDirectory, State};

pub(super) struct WorkflowTools {
    registry: Registry,
    manifests: Vec<Vec<u8>>,
    credentials: BTreeMap<String, OsString>,
}

impl WorkflowTools {
    pub(super) fn load(path: &Path) -> Result<Self> {
        let raw = read_config(path, MAX_REGISTRY_BYTES as u64)?
            .with_context(|| format!("workflow tool registry {} was not found", path.display()))?;
        let registry = Registry::parse(&raw)?;
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        let mut names = BTreeSet::new();
        let mut manifests = Vec::with_capacity(registry.bindings.len());
        for binding in &registry.bindings {
            register(&mut names, binding.token_env.clone(), QUEUE_TOKEN_PREFIX)?;
            let path = directory.join(&binding.manifest);
            let manifest = read_config(&path, MAX_MANIFEST_BYTES as u64)?
                .with_context(|| format!("workflow tool manifest {} was not found", path.display()))?;
            manifests.push(manifest);
        }
        Ok(Self {
            registry,
            manifests,
            credentials: read_credentials(names)?,
        })
    }

    pub(super) fn credential_names(&self) -> impl Iterator<Item = &str> {
        self.credentials.keys().map(String::as_str)
    }

    pub(super) fn configure(mut self, gateway: &mut Command, state: &State) -> Result<PrivateDirectory> {
        let stage = PrivateDirectory::create(&state.root)?;
        let snapshots = self.registry.bindings.iter_mut().zip(self.manifests);
        for (index, (binding, bytes)) in snapshots.enumerate() {
            // One-byte filenames keep the rewritten registry within the original byte limit.
            let name = char::from_digit(index as u32, 36)
                .expect("the workflow tool registry has at most 32 bindings")
                .to_string();
            write_private(&stage.path.join(&name), &bytes)?;
            binding.manifest = name.into();
        }
        let registry = stage.path.join("bindings.json");
        write_private(&registry, &serde_json::to_vec(&self.registry)?)?;
        gateway.env(ENV, registry).envs(self.credentials);
        Ok(stage)
    }
}
