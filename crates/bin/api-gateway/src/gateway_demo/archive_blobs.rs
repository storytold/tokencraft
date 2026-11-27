mod keys;

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{ensure, Context};
use db_queries::{gateway_archives::Archive, gateway_exports::ExportEvent};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use ids::{AccountId, GatewayArchiveId};
use serde::Deserialize;
use tokio::sync::Semaphore;

use self::keys::READ_KEYS;
use super::{
    archive_bucket::{ArchiveBucket, RestoreDispatch},
    catalog::digest,
    exports::ndjson::MAX_BYTES as MAX_PLAINTEXT,
    local_files,
    sealed::SealedKeyRing,
};

const MAGIC: &[u8] = b"TCA1";
const MAX_OBJECT: usize = 9 * 1024 * 1024;
const FILES: &str = "files";

pub(super) struct ArchiveStorage {
    root: Option<PathBuf>,
    bucket: Option<ArchiveBucket>,
    keys: SealedKeyRing,
    io_slots: Arc<Semaphore>,
    pub(super) automatic_after_days: Option<i32>,
    pub(super) retire_after_days: Option<i32>,
}

enum Backend<'a> {
    Files(&'a Path),
    S3(&'a ArchiveBucket),
}

#[derive(Clone)]
pub(super) struct ObjectKey {
    pub id: GatewayArchiveId,
    pub account_id: String,
    pub project_id: String,
    pub client_ref: Option<String>,
    pub generation: i32,
    pub storage_target: String,
}

impl ObjectKey {
    fn name(&self) -> String {
        format!("{}-{}.tca", self.id, self.generation)
    }

    fn binding(&self) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            self.account_id,
            self.project_id,
            self.id,
            self.generation,
            self.client_ref.as_deref().unwrap_or("")
        )
    }
}

pub(super) struct Expected {
    pub key: ObjectKey,
    pub schema_version: i32,
    pub event_count: i32,
    pub first_event_id: String,
    pub last_event_id: String,
    pub content_sha256: String,
    pub object_sha256: String,
}

impl Expected {
    pub fn from_manifest(account: &AccountId, archive: &Archive) -> Option<Self> {
        Some(Self {
            key: ObjectKey {
                id: archive.id.parse().ok()?,
                account_id: account.as_str().to_owned(),
                project_id: archive.project_id.clone(),
                client_ref: archive.client_ref.clone(),
                generation: archive.storage_generation?,
                storage_target: archive.storage_target.clone(),
            },
            schema_version: archive.schema_version,
            event_count: archive.event_count?,
            first_event_id: archive.first_event_id.clone()?,
            last_event_id: archive.last_event_id.clone()?,
            content_sha256: archive.content_sha256.clone()?,
            object_sha256: archive.object_sha256.clone()?,
        })
    }
}

pub(super) const BAD_OBJECT: &str = "archive_integrity_failed";
pub(super) const IO_ERROR: &str = "archive_storage_unavailable";
pub(super) const COLD_OBJECT: &str = "archive_retrieval_required";

impl ArchiveStorage {
    pub fn load() -> anyhow::Result<Option<Self>> {
        let automatic_after_days = age_days("TOKENCRAFT_DEMO_ARCHIVE_AUTO_AFTER_DAYS")?;
        let retire_after_days = age_days("TOKENCRAFT_DEMO_ARCHIVE_RETIRE_AFTER_DAYS")?;
        let root = std::env::var_os("TOKENCRAFT_DEMO_ARCHIVE_DIR")
            .map(|root| {
                local_files::require_private_directory(Path::new(&root))
                    .context("open the configured archive directory")?;
                fs::canonicalize(root).context("resolve the archive directory")
            })
            .transpose()?;
        let bucket = ArchiveBucket::load(
            "TOKENCRAFT_DEMO_ARCHIVE_S3",
            "tokencraft-archives/v1",
            Some("TOKENCRAFT_DEMO_ARCHIVE_S3_AUTO_RESTORE"),
        )?;
        if root.is_none() && bucket.is_none() {
            ensure!(
                automatic_after_days.is_none()
                    && retire_after_days.is_none()
                    && std::env::var_os(READ_KEYS).is_none(),
                "archive scheduling, retirement, and read keys require archive storage"
            );
            return Ok(None);
        }
        Ok(Some(Self {
            root,
            bucket,
            keys: keys::load()?,
            io_slots: Arc::new(Semaphore::new(4)),
            automatic_after_days,
            retire_after_days,
        }))
    }

    pub fn write_target(&self) -> &str {
        self.bucket.as_ref().map_or(FILES, ArchiveBucket::target)
    }

    fn backend(&self, target: &str) -> Result<Backend<'_>, &'static str> {
        if target == FILES {
            return self.root.as_deref().map(Backend::Files).ok_or(IO_ERROR);
        }
        self.bucket
            .as_ref()
            .filter(|bucket| bucket.target() == target)
            .map(Backend::S3)
            .ok_or(IO_ERROR)
    }

    pub fn permits_retrieval(&self, target: &str) -> bool {
        matches!(self.backend(target), Ok(Backend::S3(bucket)) if bucket.permits_restore())
    }

    pub async fn restoration_exists(&self, key: &ObjectKey) -> Result<bool, &'static str> {
        match self.backend(&key.storage_target)? {
            Backend::S3(bucket) => bucket.restoration_exists(&key.name()).await,
            Backend::Files(_) => Err(COLD_OBJECT),
        }
    }

    pub async fn request_retrieval(&self, key: &ObjectKey) -> Result<RestoreDispatch, &'static str> {
        match self.backend(&key.storage_target)? {
            Backend::S3(bucket) => bucket.request_restore(&key.name()).await,
            Backend::Files(_) => Err(COLD_OBJECT),
        }
    }

    pub async fn write(
        self: &Arc<Self>,
        key: ObjectKey,
        plain: Vec<u8>,
    ) -> Result<String, &'static str> {
        let name = key.name();
        let backend = self.backend(&key.storage_target)?;
        let storage = self.clone();
        let slot = storage
            .io_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| IO_ERROR)?;
        let (object, checksum, slot) = tokio::task::spawn_blocking(move || {
            let object = storage.seal(&key, &plain)?;
            let checksum = digest(&object);
            Ok::<_, &'static str>((object, checksum, slot))
        })
        .await
        .map_err(|_| IO_ERROR)??;
        match backend {
            Backend::Files(root) => {
                let root = root.to_owned();
                tokio::task::spawn_blocking(move || {
                    let _slot = slot;
                    local_files::publish(&root, &name, &object).map_err(|_| IO_ERROR)
                })
                .await
                .map_err(|_| IO_ERROR)??;
            }
            Backend::S3(bucket) => {
                let _slot = slot;
                bucket.put(&name, object).await?;
            }
        }
        Ok(checksum)
    }

    pub async fn read(self: &Arc<Self>, expected: Expected) -> Result<Vec<u8>, &'static str> {
        self.read_with(expected, None, |plain, _| plain).await
    }

    pub async fn read_events(
        self: &Arc<Self>,
        expected: Expected,
        ids: HashSet<String>,
    ) -> Result<HashMap<String, ExportEvent>, &'static str> {
        self.read_with(expected, Some(ids), |_, events| events)
            .await
    }

    async fn read_with<T: Send + 'static>(
        self: &Arc<Self>,
        expected: Expected,
        ids: Option<HashSet<String>>,
        finish: impl FnOnce(Vec<u8>, HashMap<String, ExportEvent>) -> T + Send + 'static,
    ) -> Result<T, &'static str> {
        let name = expected.key.name();
        let backend = self.backend(&expected.key.storage_target)?;
        let storage = self.clone();
        let slot = storage
            .io_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| IO_ERROR)?;
        let (object, slot) = match backend {
            Backend::Files(root) => {
                let path = root.join(name);
                tokio::task::spawn_blocking(move || {
                    local_files::read_bounded(&path, MAX_OBJECT)
                        .map(|object| (object, slot))
                        .map_err(|_| IO_ERROR)
                })
                .await
                .map_err(|_| IO_ERROR)??
            }
            Backend::S3(bucket) => (bucket.get(&name, MAX_OBJECT).await?, slot),
        };
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let plain = storage.open(&object, &expected)?;
            let events = validate(&plain, &expected, ids.as_ref())?;
            Ok(finish(plain, events))
        })
        .await
        .map_err(|_| IO_ERROR)?
    }

    pub async fn discard(&self, key: &ObjectKey) {
        match self.backend(&key.storage_target) {
            Ok(Backend::Files(root)) => {
                let _ = tokio::fs::remove_file(root.join(key.name())).await;
            }
            Ok(Backend::S3(bucket)) => {
                let _ = bucket.delete(&key.name()).await;
            }
            Err(_) => {}
        }
    }

    fn seal(&self, key: &ObjectKey, plain: &[u8]) -> Result<Vec<u8>, &'static str> {
        if plain.len() > MAX_PLAINTEXT {
            return Err("archive_partition_too_large");
        }
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(plain).map_err(|_| IO_ERROR)?;
        let compressed = gzip.finish().map_err(|_| IO_ERROR)?;
        let sealed = self
            .keys
            .seal(&key.binding(), compressed)
            .ok_or(IO_ERROR)?;
        let mut object = MAGIC.to_vec();
        object.extend_from_slice(&sealed);
        if object.len() > MAX_OBJECT {
            return Err("archive_partition_too_large");
        }
        Ok(object)
    }

    fn open(&self, object: &[u8], expected: &Expected) -> Result<Vec<u8>, &'static str> {
        if object.len() > MAX_OBJECT || digest(object) != expected.object_sha256 {
            return Err(BAD_OBJECT);
        }
        let sealed = object.strip_prefix(MAGIC).ok_or(BAD_OBJECT)?;
        let compressed = self
            .keys
            .open(&expected.key.binding(), sealed)
            .ok_or(BAD_OBJECT)?;
        let mut plain = Vec::new();
        GzDecoder::new(compressed.as_slice())
            .take((MAX_PLAINTEXT + 1) as u64)
            .read_to_end(&mut plain)
            .map_err(|_| BAD_OBJECT)?;
        if plain.len() > MAX_PLAINTEXT || digest(&plain) != expected.content_sha256 {
            return Err(BAD_OBJECT);
        }
        Ok(plain)
    }
}

fn age_days(setting: &str) -> anyhow::Result<Option<i32>> {
    std::env::var_os(setting)
        .map(|raw| {
            raw.to_str()
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|days| (1..=3650).contains(days))
                .with_context(|| format!("{setting} must be 1 to 3650"))
        })
        .transpose()
}

#[derive(Deserialize)]
struct EventIdentity<'a> {
    id: &'a str,
    project_id: &'a str,
    client_ref: Option<&'a str>,
    schema_version: i32,
}

fn validate(
    plain: &[u8],
    expected: &Expected,
    ids: Option<&HashSet<String>>,
) -> Result<HashMap<String, ExportEvent>, &'static str> {
    let body = plain.strip_suffix(b"\n").ok_or(BAD_OBJECT)?;
    let mut events = HashMap::with_capacity(ids.map_or(0, HashSet::len));
    let mut count = 0;
    let mut last = "";
    for line in body.split(|byte| *byte == b'\n') {
        let row: EventIdentity<'_> = serde_json::from_slice(line).map_err(|_| BAD_OBJECT)?;
        if row.project_id != expected.key.project_id
            || row.schema_version != expected.schema_version
            || expected
                .key
                .client_ref
                .as_deref()
                .is_some_and(|client| row.client_ref != Some(client))
            || (count == 0 && row.id != expected.first_event_id)
        {
            return Err(BAD_OBJECT);
        }
        // Verify every row against the manifest and decode only requested payloads.
        if ids.is_some_and(|ids| ids.contains(row.id)) {
            let event = serde_json::from_slice(line).map_err(|_| BAD_OBJECT)?;
            if events.insert(row.id.to_owned(), event).is_some() {
                return Err(BAD_OBJECT);
            }
        }
        last = row.id;
        count += 1;
    }
    if count != expected.event_count
        || last != expected.last_event_id
        || ids.is_some_and(|ids| ids.len() != events.len())
    {
        return Err(BAD_OBJECT);
    }
    Ok(events)
}
