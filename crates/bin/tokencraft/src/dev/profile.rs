use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs::{self, File},
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use tokio::process::Command;

use crate::state::{PrivateFile, State};

const QUEUE_TOKEN_PREFIX: &str = "TOKENCRAFT_CONNECTOR_TOKEN_";
const CHAT_KEY_PREFIX: &str = "TOKENCRAFT_LOCAL_CHAT_KEY_";

pub(super) struct Profile {
    files: Vec<(&'static str, Vec<u8>)>,
    credentials: BTreeMap<String, OsString>,
    rate_feed: Option<PathBuf>,
}

impl Profile {
    pub(super) fn load(directory: &Path) -> Result<Self> {
        let directory = directory.canonicalize().context("resolve the dev profile directory")?;
        let catalog = read_config(&directory.join("catalog.json"), 64 * 1024)?
            .context("the dev profile requires catalog.json")?;
        let mut files = vec![("TOKENCRAFT_CORE_CATALOG", catalog)];
        let mut names = BTreeSet::new();
        if let Some(bytes) = read_config(&directory.join("connector-bindings.json"), 32 * 1024)? {
            let config: Bindings<QueueCredential> = bindings(&bytes, "connector-bindings.json")?;
            for entry in config.bindings {
                register(&mut names, entry.token_env, QUEUE_TOKEN_PREFIX)?;
            }
            files.push(("TOKENCRAFT_CONNECTORS", bytes));
        }
        if let Some(bytes) = read_config(&directory.join("chat-bindings.json"), 16 * 1024)? {
            let config: Bindings<ChatBinding> = bindings(&bytes, "chat-bindings.json")?;
            for entry in config.bindings {
                for name in entry.key_env.into_iter().chain(
                    entry
                        .targets
                        .into_iter()
                        .filter_map(|target| target.key_env),
                ) {
                    register(&mut names, name, CHAT_KEY_PREFIX)?;
                }
            }
            files.push(("TOKENCRAFT_LOCAL_CHAT_BINDINGS", bytes));
        }
        let credentials = names
            .into_iter()
            .map(|name| {
                let value = env::var_os(&name)
                    .with_context(|| format!("set {name} for the dev profile"))?;
                Ok((name, value))
            })
            .collect::<Result<_>>()?;
        let rate_path = directory.join("rates.json");
        let rate_feed = config_path(&rate_path, 64 * 1024)?.map(Path::to_owned);
        Ok(Self { files, credentials, rate_feed })
    }

    pub(super) fn configure(
        &self,
        gateway: &mut Command,
        state: &State,
    ) -> Result<Vec<PrivateFile>> {
        // Keep the same credential vault on restart and restore of this dev state.
        gateway.env(
            "TOKENCRAFT_PROVIDER_CREDENTIAL_KEY",
            state.secrets.derive_key(b"TokenCraft dev provider credential key v1")?,
        );
        gateway.envs(&self.credentials);
        if let Some(rate_feed) = &self.rate_feed {
            gateway.env("TOKENCRAFT_DEMO_RATE_FEED", rate_feed);
        }
        self.files
            .iter()
            .map(|(name, bytes)| {
                let file = PrivateFile::create(&state.root, bytes)?;
                gateway.env(name, &file.path);
                Ok(file)
            })
            .collect()
    }

    pub(super) fn exclude_credentials(&self, build: &mut Command) {
        for name in self.credentials.keys() {
            build.env_remove(name);
        }
    }
}

#[derive(Deserialize)]
struct Bindings<T> {
    bindings: Vec<T>,
}

#[derive(Deserialize)]
struct QueueCredential {
    token_env: String,
}

#[derive(Deserialize)]
struct ChatCredential {
    key_env: Option<String>,
}

#[derive(Deserialize)]
struct ChatBinding {
    key_env: Option<String>,
    #[serde(default)]
    targets: Vec<ChatCredential>,
}

fn bindings<T: serde::de::DeserializeOwned>(bytes: &[u8], label: &str) -> Result<Bindings<T>> {
    serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid {label} credential references"))
}

fn register(names: &mut BTreeSet<String>, name: String, prefix: &str) -> Result<()> {
    ensure!(
        name.len() <= 96
            && name.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            }),
        "binding credentials must reference a {prefix} environment variable"
    );
    names.insert(name);
    Ok(())
}

fn config_path(path: &Path, limit: u64) -> Result<Option<&Path>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    ensure!(
        metadata.is_file(),
        "{} must be a regular file",
        path.display()
    );
    ensure!(metadata.len() <= limit, "{} exceeds {} KiB", path.display(), limit / 1024);
    Ok(Some(path))
}

pub(super) fn read_config(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let Some(path) = config_path(path, limit)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "{} exceeds {} KiB",
        path.display(),
        limit / 1024
    );
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_only_binding_credential_names() {
        let mut names = BTreeSet::new();
        register(
            &mut names,
            "TOKENCRAFT_CONNECTOR_TOKEN_FAL".into(),
            QUEUE_TOKEN_PREFIX,
        )
        .unwrap();
        for name in [
            "PATH",
            "SESSION_SECRET",
            "TOKENCRAFT_CONNECTOR_TOKEN_",
            "TOKENCRAFT_CONNECTOR_TOKEN_BAD=NAME",
        ] {
            assert!(register(&mut names, name.into(), QUEUE_TOKEN_PREFIX).is_err());
        }
        assert_eq!(names.len(), 1);
    }
}
