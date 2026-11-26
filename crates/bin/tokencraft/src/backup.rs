mod removals;

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection};
use tokio::signal::unix::{signal, SignalKind};

use crate::{
    postgres::{Mode, Postgres, DATABASE},
    state::{private_dir, random_secret, write_private, State},
};

const HELP: &str = "Usage:
  tokencraft backup  DEV_STATE NEW_BACKUP_DIRECTORY
  tokencraft checkpoint DEV_STATE NEW_CHECKPOINT_DIRECTORY
  tokencraft restore BACKUP_DIRECTORY NEW_DEV_STATE [--content-removals CHECKPOINT]

The backup includes a logical PostgreSQL dump and private local state.
Finish queued or running requests and active workflows, then stop tokencraft dev before backing up.
";
const DIRECTORIES: [&str; 3] = ["artifacts", "archives", "connector-jobs"];
const KEY_FILES: [&str; 2] = ["dev-keys.json", "archive-key"];
const DUMP: &str = "database.dump";
const MANIFEST: &str = "manifest.json";
const FORMAT: &str = "tokencraft-native-dev-v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    sha256: BTreeMap<String, String>,
}

#[derive(Clone, Copy)]
pub(crate) enum Action {
    Backup,
    Restore,
    Checkpoint,
}

pub(crate) fn run(args: impl Iterator<Item = OsString>, action: Action) -> Result<()> {
    let paths: Vec<_> = args.collect();
    if paths.first().is_some_and(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let (source, destination, checkpoint) = match paths.as_slice() {
        [source, destination] => (source, destination, None),
        [source, destination, flag, checkpoint]
            if matches!(action, Action::Restore) && flag == "--content-removals" =>
        {
            (source, destination, Some(Path::new(checkpoint)))
        }
        _ => bail!("supply source and new destination directories. Use tokencraft backup --help"),
    };
    let source = Path::new(source).canonicalize().context("find source directory")?;
    let destination = Path::new(destination);
    let name = destination.file_name().context("choose a destination name")?;
    let parent = destination.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let destination = parent.canonicalize()?.join(name);
    ensure!(!destination.starts_with(&source), "place the destination outside the source directory");
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("choose a new destination directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect the backup destination"),
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            let operation = async {
                match action {
                    Action::Backup => create(&source, &destination).await,
                    Action::Restore => recover(&source, &destination, checkpoint).await,
                    Action::Checkpoint => removals::capture(&source, &destination).await,
                }
            };
            tokio::select! {
                biased;
                _ = interrupt.recv() => bail!("TokenCraft backup operation interrupted"),
                _ = terminate.recv() => bail!("TokenCraft backup operation interrupted"),
                result = operation => result,
            }
        })?;
    println!("Created {}", destination.display());
    Ok(())
}

async fn create(source: &Path, destination: &Path) -> Result<()> {
    let state = State::open_existing(source)?;
    let stage = Stage::new(destination)?;
    let port = local_port()?;
    let (postgres, url) = Postgres::start(&state, port, Mode::Backup).await?;
    ensure_quiescent(&url).await?;

    let dump = stage.path.join(DUMP);
    // Reserve the destination with private permissions before pg_dump opens it.
    new_file(&dump)?;
    dump_database(&postgres, &state, &dump).await?;
    postgres.finish().await?;
    require_stopped(&state)?;
    File::open(&dump)?.sync_all()?;

    let mut checksums = BTreeMap::from([(DUMP.into(), hash_file(&dump)?)]);
    copy_state(source, &stage.path, &mut checksums)?;
    let manifest = Manifest { format: FORMAT.into(), sha256: checksums };
    write_private(&stage.path.join(MANIFEST), &serde_json::to_vec_pretty(&manifest)?)?;
    stage.publish(destination)
}

async fn ensure_quiescent(url: &str) -> Result<()> {
    let mut db = PgConnection::connect(url).await.context("open the dev database for backup")?;
    let requests_pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM public.gateway_requests
         WHERE num_nonnulls(succeeded_at, failed_at, canceled_at, uncertain_at) = 0)",
    )
    .fetch_one(&mut db)
    .await
    .context("inspect outstanding requests")?;
    ensure!(!requests_pending, "finish queued or running requests before backing up");

    let has_workflows: bool = sqlx::query_scalar("SELECT to_regclass('public.workflow_runs') IS NOT NULL")
        .fetch_one(&mut db)
        .await
        .context("inspect the workflow schema")?;
    if has_workflows {
        let workflows_pending: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM public.workflow_runs
             WHERE status IN ('queued', 'running', 'reconciling'))",
        )
        .fetch_one(&mut db)
        .await
        .context("inspect active workflows")?;
        ensure!(!workflows_pending, "finish queued, running, or reconciling workflows before backing up");
    }
    Ok(())
}

async fn recover(source: &Path, destination: &Path, checkpoint: Option<&Path>) -> Result<()> {
    ensure!(private_entry(source)?.is_dir(), "choose a backup directory");
    let mut names: BTreeSet<_> = fs::read_dir(source)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    let required = ["dev-keys.json", "artifacts", "connector-jobs", DUMP, MANIFEST];
    ensure!(required.into_iter().all(|name| names.remove(std::ffi::OsStr::new(name))),
        "backup layout is missing a required entry");
    ensure!(names.iter().all(|name| name == "archives" || name == "archive-key"),
        "backup directory contains unexpected entries");
    let manifest_path = source.join(MANIFEST);
    ensure!(private_entry(&manifest_path)?.is_file(), "backup manifest must be a file");
    let manifest: Manifest = serde_json::from_reader(std::io::BufReader::new(File::open(manifest_path)?))
        .context("read backup manifest")?;
    ensure!(manifest.format == FORMAT, "unsupported backup format");

    let stage = Stage::new(destination)?;
    let mut checksums = BTreeMap::new();
    copy_state(source, &stage.path, &mut checksums)?;
    ensure!(private_entry(&source.join(DUMP))?.is_file(), "database dump must be a file");
    copy_entry(source, &stage.path, Path::new(DUMP), &mut checksums)?;
    ensure!(checksums == manifest.sha256, "backup contents do not match the manifest");

    let state = State::open_existing(&stage.path)?;
    state.local_archives()?;
    let dump = stage.path.join(DUMP);
    let (postgres, url) = Postgres::start(&state, local_port()?, Mode::Restore).await?;
    load_database(&postgres, &state, &dump).await?;
    let postgres = if let Some(checkpoint) = checkpoint {
        removals::apply(checkpoint, &state, &postgres, &url).await?;
        dump_database(&postgres, &state, &dump).await?;
        postgres.finish().await?;

        // Build the published state from the scrubbed logical dump, without the
        // earlier cluster's heap pages, recovery logs, or superseded dump.
        require_stopped(&state)?;
        fs::remove_dir_all(state.root.join("postgres"))?;
        fs::remove_file(state.root.join("postgres.log"))?;
        let (postgres, _) = Postgres::start(&state, local_port()?, Mode::Restore).await?;
        load_database(&postgres, &state, &dump).await?;
        postgres
    } else {
        postgres
    };
    postgres.finish().await?;
    require_stopped(&state)?;
    drop(state);
    fs::remove_file(stage.path.join(DUMP))?;
    stage.publish(destination)
}

async fn dump_database(postgres: &Postgres, state: &State, dump: &Path) -> Result<()> {
    let status = postgres.database_tool("pg_dump", state)?
        .args(["-w", "--format=custom", "--dbname", DATABASE, "--file"])
        .arg(dump).status().await.context("run pg_dump")?;
    ensure!(status.success(), "pg_dump failed");
    Ok(())
}

async fn load_database(postgres: &Postgres, state: &State, dump: &Path) -> Result<()> {
    let status = postgres.database_tool("pg_restore", state)?
        .args(["-w", "--single-transaction", "--no-owner", "--dbname", DATABASE])
        .arg(dump).status().await.context("run pg_restore")?;
    ensure!(status.success(), "pg_restore failed");
    Ok(())
}

fn require_stopped(state: &State) -> Result<()> {
    ensure!(!state.root.join("postgres/postmaster.pid").exists(), "dev PostgreSQL is still running");
    Ok(())
}

fn local_port() -> Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port())
}

fn copy_state(source: &Path, dest: &Path, checksums: &mut BTreeMap<String, String>) -> Result<()> {
    let archive = fs::symlink_metadata(source.join("archives"));
    let key = fs::symlink_metadata(source.join("archive-key"));
    // Older native state can predate filesystem event archives.
    let has_archive = match (archive, key) {
        (Ok(_), Ok(_)) => true,
        (Err(dir), Err(key)) if dir.kind() == std::io::ErrorKind::NotFound
            && key.kind() == std::io::ErrorKind::NotFound => false,
        _ => bail!("filesystem archives and their key must be backed up together"),
    };
    for name in DIRECTORIES {
        if name == "archives" && !has_archive {
            continue;
        }
        ensure!(private_entry(&source.join(name))?.is_dir(), "{name} must be a directory");
        copy_entry(source, dest, Path::new(name), checksums)?;
    }
    for name in KEY_FILES {
        if name == "archive-key" && !has_archive {
            continue;
        }
        ensure!(private_entry(&source.join(name))?.is_file(), "{name} must be a file");
        copy_entry(source, dest, Path::new(name), checksums)?;
    }
    Ok(())
}

fn copy_entry(source: &Path, dest: &Path, relative: &Path, checksums: &mut BTreeMap<String, String>) -> Result<()> {
    let from = source.join(relative);
    let to = dest.join(relative);
    let metadata = private_entry(&from)?;
    if metadata.is_dir() {
        private_dir(&to)?;
        for entry in fs::read_dir(&from)? {
            copy_entry(source, dest, &relative.join(entry?.file_name()), checksums)?;
        }
        File::open(to)?.sync_all()?;
        return Ok(());
    }

    let mut input = File::open(from)?;
    let mut output = new_file(&to)?;
    let digest = digest_stream(&mut input, &mut output)?;
    output.sync_all()?;
    let name = relative.to_str().context("backup paths must use UTF-8")?;
    checksums.insert(name.into(), digest);
    Ok(())
}

fn hash_file(path: &Path) -> Result<String> {
    digest_stream(&mut File::open(path)?, &mut std::io::sink())
}

fn digest_stream(input: &mut impl Read, output: &mut impl Write) -> Result<String> {
    let mut bytes = [0; 64 * 1024];
    let mut hash = Sha256::new();
    loop {
        let size = input.read(&mut bytes)?;
        if size == 0 {
            return Ok(hex::encode(hash.finalize()));
        }
        hash.update(&bytes[..size]);
        output.write_all(&bytes[..size])?;
    }
}

fn private_entry(path: &Path) -> Result<fs::Metadata> {
    let entry = fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    ensure!(
        entry.permissions().mode() & 0o077 == 0 && (entry.is_file() || entry.is_dir()),
        "backup inputs must be private regular files or directories"
    );
    Ok(entry)
}

fn new_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?)
}

struct Stage {
    path: PathBuf,
}

impl Stage {
    fn new(destination: &Path) -> Result<Self> {
        let parent = destination.parent().context("choose a destination parent")?;
        let path = parent.join(format!(".tokencraft-backup-{}", random_secret()?));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }

    fn publish(&self, destination: &Path) -> Result<()> {
        File::open(&self.path)?.sync_all()?;
        // Reserve a new directory before renaming so an existing destination is untouched.
        fs::DirBuilder::new().mode(0o700).create(destination)?;
        if let Err(error) = fs::rename(&self.path, destination) {
            let _ = fs::remove_dir(destination);
            return Err(error).context("publish prepared state");
        }
        File::open(destination.parent().context("find destination parent")?)?.sync_all()?;
        Ok(())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn copying_state_rejects_links_and_keeps_existing_destinations() -> Result<()> {
        let target = std::env::temp_dir().join(format!("tokencraft-test-{}", random_secret()?));
        let fixture = Stage::new(&target)?;
        let copy = Stage::new(&target)?;
        write_private(&fixture.path.join("regular"), b"fixture bytes")?;
        symlink(fixture.path.join("regular"), fixture.path.join("link"))?;

        let mut checksums = BTreeMap::new();
        copy_entry(&fixture.path, &copy.path, Path::new("regular"), &mut checksums)?;
        assert_eq!(checksums["regular"], hex::encode(Sha256::digest(b"fixture bytes")));
        assert!(copy_entry(&fixture.path, &copy.path, Path::new("link"), &mut checksums).is_err());
        assert!(!copy.path.join("link").exists());

        let existing = fixture.path.join("existing");
        private_dir(&existing)?;
        write_private(&existing.join("sentinel"), b"keep")?;
        assert!(copy.publish(&existing).is_err());
        assert_eq!(fs::read(existing.join("sentinel"))?, b"keep");
        Ok(())
    }
}
