use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{ErrorKind, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const SECRETS_FILE: &str = "dev-keys.json";

pub(crate) struct State {
    pub root: PathBuf,
    pub secrets: Secrets,
    _lock: File,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Secrets {
    pub database: String,
    pub session: String,
    pub payload: String,
}

impl Secrets {
    pub(crate) fn derive_key(&self, purpose: &[u8]) -> Result<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.payload.as_bytes())?;
        mac.update(purpose);
        Ok(hex::encode(mac.finalize().into_bytes()))
    }
}

impl State {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Self::load(path, true)
    }

    pub(crate) fn open_existing(path: &Path) -> Result<Self> {
        ensure!(path.join(SECRETS_FILE).is_file(), "choose an initialized dev state directory");
        Self::load(path, false)
    }

    fn load(path: &Path, initialize: bool) -> Result<Self> {
        private_dir(path)?;
        let root = path.canonicalize().context("resolve dev state directory")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join(".tokencraft.lock"))
            .context("open the dev state lock")?;
        lock.try_lock()
            .context("this dev state is already in use or could not be locked")?;

        let secrets_path = root.join(SECRETS_FILE);
        let secrets = match fs::symlink_metadata(&secrets_path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && metadata.permissions().mode() & 0o777 == 0o600,
                    "dev-keys.json must be a regular file with mode 0600"
                );
                serde_json::from_slice(&fs::read(&secrets_path)?).context("read the dev keys")?
            }
            Err(error) if initialize && error.kind() == ErrorKind::NotFound => {
                for entry in fs::read_dir(&root)? {
                    ensure!(
                        entry?.file_name() == ".tokencraft.lock",
                        "choose an empty state directory or one initialized by tokencraft dev"
                    );
                }
                let secrets = Secrets {
                    database: random_secret()?,
                    session: random_secret()?,
                    payload: random_secret()?,
                };
                write_private(&secrets_path, &serde_json::to_vec(&secrets)?)?;
                secrets
            }
            Err(error) => return Err(error).context("inspect the dev keys"),
        };
        ensure!(
            [&secrets.database, &secrets.session, &secrets.payload]
                .into_iter()
                .all(|key| valid_secret(key)),
            "the dev state contains an invalid key"
        );
        Ok(Self {
            root,
            secrets,
            _lock: lock,
        })
    }

    pub(crate) fn local_archives(&self) -> Result<(PathBuf, String)> {
        let directory = self.root.join("archives");
        private_dir(&directory)?;
        let key_path = self.root.join("archive-key");
        let key = match fs::symlink_metadata(&key_path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && metadata.permissions().mode() & 0o777 == 0o600,
                    "archive-key must be a regular file with mode 0600"
                );
                fs::read_to_string(&key_path).context("read the local archive key")?
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                ensure!(
                    fs::read_dir(&directory)?.next().transpose()?.is_none(),
                    "archive-key is missing for existing local archives. Restore the original key"
                );
                let key = random_secret()?;
                write_private(&key_path, key.as_bytes())?;
                key
            }
            Err(error) => return Err(error).context("inspect the local archive key"),
        };
        ensure!(valid_secret(&key), "the dev state contains an invalid archive key");
        Ok((directory, key))
    }
}

fn valid_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o777 == 0o700,
        "state directory {} must be a directory with mode 0700",
        path.display()
    );
    Ok(())
}

pub(crate) fn random_secret() -> Result<String> {
    let mut bytes = [0; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .context("generate local credentials")?;
    Ok(hex::encode(bytes))
}

pub(crate) struct PrivateFile {
    pub path: PathBuf,
}

impl PrivateFile {
    pub(crate) fn create(parent: &Path, bytes: &[u8]) -> Result<Self> {
        let path = parent.join(format!(".tmp-{}", random_secret()?));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let private = Self { path };
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(private)
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("write path must have a parent directory")?;
    let staged = PrivateFile::create(parent, bytes)?;
    fs::rename(&staged.path, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
