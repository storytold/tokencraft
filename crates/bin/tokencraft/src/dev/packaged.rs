use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use tokio::{process::Command, time::timeout};

use super::profile::read_config;

#[derive(Deserialize)]
struct Manifest {
    version: String,
    source_revision: Option<String>,
}

pub(super) async fn gateway(launcher: &Path) -> Result<Option<PathBuf>> {
    let launcher = launcher
        .canonicalize()
        .context("resolve the TokenCraft launcher")?;
    let Some(bin) = launcher
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "bin"))
    else {
        return Ok(None);
    };
    let Some(root) = bin.parent() else {
        return Ok(None);
    };
    let Some(bytes) = read_config(&root.join("VERSION.json"), 8 * 1024)? else {
        return Ok(None);
    };
    let manifest: Manifest =
        serde_json::from_slice(&bytes).context("read the native package VERSION.json")?;
    ensure!(
        manifest.version == env!("CARGO_PKG_VERSION")
            && manifest.source_revision.as_deref() == option_env!("TOKENCRAFT_SOURCE_REVISION"),
        "the packaged launcher version or source revision differs from VERSION.json"
    );

    let executable = launcher.with_file_name("api-gateway");
    ensure!(executable.is_file(), "the native package is missing bin/api-gateway");
    let mut probe = Command::new(&executable);
    probe.arg("--version")
        .current_dir(bin)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(10), probe.output())
        .await
        .context("time out checking the packaged api-gateway version")?
        .context("run api-gateway --version from the native package")?;
    let revision = manifest.source_revision.as_deref().unwrap_or("unstamped");
    let expected = format!("api-gateway {} ({revision})\n", manifest.version);
    ensure!(
        output.status.success() && output.stdout == expected.as_bytes(),
        "the packaged api-gateway version or source revision differs from VERSION.json"
    );
    Ok(Some(executable))
}
