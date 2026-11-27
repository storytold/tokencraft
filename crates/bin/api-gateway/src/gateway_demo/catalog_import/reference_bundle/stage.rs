use std::{
    collections::BTreeMap,
    fs,
    io,
    path::Path,
};

use anyhow::{ensure, Context, Result};
use serde::Serialize;

use super::super::{ReferenceCatalog, MAX_REFERENCE_BYTES};
use super::{digest, verify_bundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_MIB};
use crate::gateway_demo::{catalog_import::reference_signature::VerifiedReference, local_files};

const CATALOG_FILENAME: &str = "catalog.json";
const INDEX_READY: &str = ".indexed-v1";

#[derive(Serialize)]
pub(super) struct StagedRelease<'a> {
    #[serde(flatten)]
    reference: &'a VerifiedReference,
    catalog_path: String,
    release_path: String,
    current_path: String,
}

pub(super) fn write<'a>(
    directory: &Path,
    reference: &'a VerifiedReference,
    public_key: &[u8],
    catalog: &[u8],
    bundle: &[u8],
) -> Result<StagedRelease<'a>> {
    create_directory(directory)?;
    let revisions = directory.join("revisions");
    create_directory(&revisions)?;
    local_files::sync_directory(directory)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(revisions.join(".lock"))
        .context("open the reference revision lock")?;
    lock.lock().context("lock the reference revision index")?;
    if !revisions.join(INDEX_READY).try_exists()? {
        index_existing_releases(directory, &revisions)?;
        publish_file(&revisions, INDEX_READY, b"1\n", false).context("finish indexing reference releases")?;
    }
    let revision_digest = digest(reference.reference_revision.as_bytes());
    record_revision(&revisions, &revision_digest, &reference.reference_sha256)?;

    let release = directory.join(&reference.reference_sha256);
    let current = directory.join("current");
    create_directory(&release)?;
    create_directory(&current)?;
    local_files::sync_directory(directory)?;

    write_catalog(&release, catalog)?;

    let filename = format!("{}.json", reference.public_key_sha256);
    let retained;
    let published = match publish_file(&release, &filename, bundle, false) {
        Ok(()) => bundle,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            retained = local_files::read_bounded(&release.join(&filename), MAX_BUNDLE_BYTES)
                .context("read the immutable reference bundle")?;
            ensure!(retained.len() <= MAX_BUNDLE_BYTES, "immutable reference bundle exceeds {MAX_BUNDLE_MIB} MiB");
            let existing = verify_bundle(&retained, public_key)
                .context("verify the immutable reference bundle")?;
            ensure!(
                existing.verified.is_some_and(|release| release.reference_sha256 == reference.reference_sha256),
                "immutable release path contains a different reference snapshot"
            );
            &retained
        }
        Err(error) => return Err(error).context("write the immutable reference bundle"),
    };
    publish_file(&current, &filename, published, true).context("update the current reference bundle")?;
    Ok(StagedRelease {
        reference,
        catalog_path: format!("{}/{CATALOG_FILENAME}", reference.reference_sha256),
        release_path: format!("{}/{filename}", reference.reference_sha256),
        current_path: format!("current/{filename}"),
    })
}

fn index_existing_releases(directory: &Path, revisions: &Path) -> Result<()> {
    let mut existing = BTreeMap::new();
    for entry in fs::read_dir(directory).context("list existing reference releases")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(checksum) = name.to_str().filter(|name| {
            name.len() == 64 && name.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        }) else {
            continue;
        };
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join(CATALOG_FILENAME);
        let bytes = match local_files::read_bounded(&path, MAX_REFERENCE_BYTES as usize) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("read a previously staged reference catalog"),
        };
        let catalog = ReferenceCatalog::parse(&bytes).context("parse a previously staged reference catalog")?;
        ensure!(digest(&bytes) == checksum, "existing catalog does not match its immutable release path");
        let previous = existing.insert(digest(catalog.revision.as_bytes()), checksum.to_owned());
        ensure!(
            previous.is_none_or(|previous| previous == checksum),
            "the reference revision already names a different snapshot"
        );
    }
    for (revision_digest, checksum) in existing {
        record_revision(revisions, &revision_digest, &checksum)?;
    }
    Ok(())
}

fn record_revision(directory: &Path, revision_digest: &str, checksum: &str) -> Result<()> {
    let filename = format!("{revision_digest}.sha256");
    let contents = format!("{checksum}\n");
    match publish_file(directory, &filename, contents.as_bytes(), false) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = local_files::read_bounded(&directory.join(&filename), contents.len())
                .context("read the recorded reference revision")?;
            ensure!(existing == contents.as_bytes(), "the reference revision already names a different snapshot");
            Ok(())
        }
        Err(error) => Err(error).context("record the reference revision"),
    }
}

fn write_catalog(directory: &Path, catalog: &[u8]) -> Result<()> {
    match publish_file(directory, CATALOG_FILENAME, catalog, false) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = local_files::read_bounded(&directory.join(CATALOG_FILENAME), catalog.len())
                .context("read the immutable public catalog")?;
            ensure!(existing == catalog, "immutable catalog path contains different bytes");
            Ok(())
        }
        Err(error) => Err(error).context("write the immutable public catalog"),
    }
}

fn create_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).context("create the reference registry directory")?;
    ensure!(fs::symlink_metadata(path)?.is_dir(), "reference registry path must be a directory");
    Ok(())
}

fn publish_file(directory: &Path, name: &str, bytes: &[u8], replace: bool) -> io::Result<()> {
    let temporary_name = format!(".{name}-{:016x}", rand::random::<u64>());
    local_files::publish(directory, &temporary_name, bytes)?;
    let temporary = directory.join(temporary_name);
    let destination = directory.join(name);
    let result = if replace {
        fs::rename(&temporary, destination)
    } else {
        fs::hard_link(&temporary, destination)
    };
    let cleanup = fs::remove_file(temporary);
    result?;
    if !replace {
        cleanup?;
    }
    local_files::sync_directory(directory)
}
