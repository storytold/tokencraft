use std::{
    env,
    fs::{self, OpenOptions},
    net::Ipv4Addr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command as SyncCommand, ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{bail, ensure, Context, Result};
use sqlx::postgres::PgPoolOptions;
use tokio::{
    process::{Child, Command},
    time::{sleep, Instant},
};

use crate::state::{PrivateFile, State};

const HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;
const USER: &str = "tokencraft";
pub(crate) const DATABASE: &str = "tokencraft_dev";
const SERVER_TOOLS: &[&str] = &["postgres", "initdb", "pg_ctl", "pg_isready", "createdb"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Dev,
    Backup,
    Restore,
}

pub(crate) struct Postgres {
    child: Child,
    tools: PathBuf,
    port: u16,
    data: PathBuf,
    log: PathBuf,
}

impl Postgres {
    pub(crate) async fn start(state: &State, port: u16, mode: Mode) -> Result<(Self, String)> {
        let tools = postgres_tools().await?;
        let data = state.root.join("postgres");
        let log = state.root.join("postgres.log");
        if let Ok(metadata) = fs::symlink_metadata(&data) {
            ensure!(metadata.is_dir(), "dev PostgreSQL data must be a directory");
        }
        if !data.join("PG_VERSION").is_file() {
            ensure!(mode != Mode::Backup, "dev PostgreSQL cluster is missing");
            let password = format!("{}\n", state.secrets.database);
            let secret = PrivateFile::create(&state.root, password.as_bytes())?;
            let status = tool(tools.join("initdb"))
                .arg("-D")
                .arg(&data)
                .args([
                    "--encoding=UTF8",
                    "--no-locale",
                    "--auth=scram-sha-256",
                    "--username",
                    USER,
                    "--pwfile",
                ])
                .arg(&secret.path)
                .status()
                .await
                .context("start initdb")?;
            ensure!(status.success(), "initdb failed");
        }

        let ctl = tools.join("pg_ctl");
        let stopped = tool(&ctl)
            .arg("status")
            .arg("-D")
            .arg(&data)
            .stdout(Stdio::null())
            .status()
            .await
            .context("check local PostgreSQL state")?;
        ensure!(
            stopped.code() == Some(3),
            "the dev PostgreSQL cluster is already running or its state could not be checked"
        );

        let output = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log)?;
        let child = tool(tools.join("postgres"))
            .arg("-D")
            .arg(&data)
            .args([
                "-h",
                &HOST.to_string(),
                "-p",
                &port.to_string(),
                "-c",
                "unix_socket_directories=",
            ])
            .stdout(output.try_clone()?)
            .stderr(output)
            .spawn()
            .context("start local PostgreSQL")?;
        let mut pg = Self {
            child,
            tools,
            port,
            data,
            log,
        };
        pg.wait_until_ready(port, &pg.tools.join("pg_isready")).await?;
        let database_url = prepare(state, port, &pg.tools, mode).await?;
        Ok((pg, database_url))
    }

    async fn wait_until_ready(&mut self, port: u16, isready: &Path) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!(
                    "PostgreSQL stopped during startup ({status}). See {}",
                    self.log.display()
                );
            }
            let ready = tool(isready)
                .args([
                    "-q",
                    "-t",
                    "1",
                    "-h",
                    &HOST.to_string(),
                    "-p",
                    &port.to_string(),
                    "-U",
                    USER,
                    "-d",
                    "postgres",
                ])
                .status()
                .await
                .context("check PostgreSQL readiness")?;
            if ready.success() {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "PostgreSQL did not become ready. See {}",
                self.log.display()
            );
            sleep(Duration::from_millis(200)).await;
        }
    }

    pub(crate) fn database_tool(&self, name: &str, state: &State) -> Result<Command> {
        let program = self.tools.join(name);
        ensure!(program.is_file(), "{} is missing from {}", name, self.tools.display());
        let mut command = tool(program);
        command
            .env("PGHOST", HOST.to_string())
            .env("PGPORT", self.port.to_string())
            .env("PGUSER", USER)
            .env("PGPASSWORD", &state.secrets.database);
        Ok(command)
    }

    pub(crate) async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait().await
    }

    pub(crate) async fn finish(mut self) -> Result<()> {
        self.stop_server()?;
        let control = tool(self.tools.join("pg_controldata"))
            .arg(&self.data)
            .output()
            .await
            .context("inspect PostgreSQL shutdown state with pg_controldata")?;
        ensure!(control.status.success(), "could not read the PostgreSQL control file");
        let output = String::from_utf8_lossy(&control.stdout);
        let state = output.lines()
            .find_map(|line| line.strip_prefix("Database cluster state:"))
            .map(str::trim);
        ensure!(
            state == Some("shut down"),
            "PostgreSQL did not record a clean shutdown (last state: {})",
            state.unwrap_or("unknown")
        );
        Ok(())
    }

    fn stop_server(&mut self) -> Result<()> {
        if self.child.try_wait().is_ok_and(|status| status.is_some())
            || !self.data.join("postmaster.pid").is_file()
        {
            return Ok(());
        }
        let status = SyncCommand::new(self.tools.join("pg_ctl"))
            .env_clear()
            .env("LC_ALL", "C")
            .arg("stop")
            .arg("-D")
            .arg(&self.data)
            .args(["-w", "-t", "15", "-m", "fast"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("stop the dev PostgreSQL cluster")?;
        ensure!(status.success(), "could not stop the dev PostgreSQL cluster");
        Ok(())
    }
}

impl Drop for Postgres {
    fn drop(&mut self) {
        if self.stop_server().is_err() {
            eprintln!(
                "Could not stop the dev PostgreSQL cluster cleanly. See {}",
                self.log.display()
            );
        }
    }
}

async fn prepare(state: &State, port: u16, tools: &Path, mode: Mode) -> Result<String> {
    let password = &state.secrets.database;
    let origin = format!("postgresql://{USER}:{password}@{HOST}:{port}");
    let maintenance = connect(&format!("{origin}/postgres"))
        .await
        .context("open the dev PostgreSQL server")?;
    // Verify the live listener before creating or migrating a database.
    let data = db::data_directory(&maintenance)
        .await
        .context("read the dev PostgreSQL data directory")?;
    ensure!(
        Path::new(&data).canonicalize()? == state.root.join("postgres"),
        "the PostgreSQL listener is not the cluster owned by this dev state"
    );
    maintenance.close().await;

    let url = format!("{origin}/{DATABASE}");
    let pool = match connect(&url).await {
        Ok(pool) => pool,
        Err(sqlx::Error::Database(error))
            if mode != Mode::Backup && error.code().as_deref() == Some("3D000") =>
        {
            let createdb = tools.join("createdb");
            let status = tool(createdb)
                .env("PGHOST", HOST.to_string())
                .env("PGPORT", port.to_string())
                .env("PGUSER", USER)
                .env("PGPASSWORD", password)
                .args(["--maintenance-db=postgres", "-w", DATABASE])
                .status()
                .await
                .context("start createdb")?;
            ensure!(status.success(), "could not create the dev database");
            connect(&url)
                .await
                .context("connect to the created dev database")?
        }
        Err(error) => return Err(error).context("connect to the dev database"),
    };
    if mode == Mode::Dev {
        db::migrate(&pool)
            .await
            .context("apply TokenCraft database migrations")?;
    }
    pool.close().await;
    Ok(url)
}

async fn connect(url: &str) -> sqlx::Result<db::PgPool> {
    PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await
}

async fn postgres_tools() -> Result<PathBuf> {
    let search_path = env::var_os("PATH").unwrap_or_default();
    if let Some(tools) = env::split_paths(&search_path)
        .filter_map(|directory| directory.canonicalize().ok())
        .find(|directory| SERVER_TOOLS.iter().all(|name| executable(&directory.join(name))))
    {
        return Ok(tools);
    }

    let output = Command::new("pg_config")
        .arg("--bindir")
        .kill_on_drop(true)
        .output()
        .await
        .context("put the PostgreSQL server tools or their pg_config on PATH")?;
    ensure!(output.status.success(), "pg_config --bindir failed");
    let tools = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    for name in SERVER_TOOLS {
        ensure!(
            executable(&tools.join(name)),
            "{} is missing or cannot run from {}",
            name,
            tools.display()
        );
    }
    Ok(tools)
}

fn executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn tool(program: impl AsRef<Path>) -> Command {
    let mut command = Command::new(program.as_ref());
    command
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}
