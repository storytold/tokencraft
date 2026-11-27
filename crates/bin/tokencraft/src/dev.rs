mod console;
mod profile;

use std::{
    env,
    ffi::OsString,
    net::{Ipv4Addr, TcpListener},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    signal::unix::{SignalKind, signal},
    time::{Instant, sleep},
};

use self::profile::Profile;
use crate::{
    connector,
    postgres::{Mode, Postgres},
    state::{State, private_dir, random_secret, write_private},
};

const HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;
const CATALOG: &[u8] = include_bytes!("../../../../examples/connector/catalog.json");
const BINDINGS: &[u8] = include_bytes!("../../../../examples/connector/bindings.json");
const HELP: &str = "Usage: tokencraft dev [OPTIONS]

Start a local TokenCraft gateway, PostgreSQL cluster, and bundled example connector.
Use tokencraft mapping --help to manage provider field mappings.
Use tokencraft backup --help to back up or restore stopped dev state.
Use tokencraft --version to inspect the launcher build.

Options:
  --state-dir PATH      Private state directory (default: .tokencraft-dev)
  --profile DIR         Use a reviewed catalog, bindings and rate feed from DIR
  --api-port PORT       Gateway API port (default: 4000)
  --connector-port PORT Example connector port (default: 4019)
  --db-port PORT        PostgreSQL port (default: 55439)
  --web-port PORT       Allowed localhost console port (default: 3000)
  --console-dir DIR     Serve a packaged frontend build on the web port
  --gateway-bin PATH    Use an existing api-gateway binary
  --offline             Build api-gateway using cached Cargo dependencies
  --help                Show this help
";

struct Options {
    state_dir: PathBuf,
    api_port: u16,
    connector_port: u16,
    db_port: u16,
    web_port: u16,
    gateway_bin: Option<PathBuf>,
    profile: Option<PathBuf>,
    console_dir: Option<PathBuf>,
    offline: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            state_dir: PathBuf::from(".tokencraft-dev"),
            api_port: 4000,
            connector_port: 4019,
            db_port: 55439,
            web_port: 3000,
            gateway_bin: None,
            profile: None,
            console_dir: None,
            offline: false,
        }
    }
}

pub(crate) fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    match args.next().as_deref().and_then(|arg| arg.to_str()) {
        None | Some("help" | "--help" | "-h") => print!("{HELP}"),
        Some("--version") => println!(
            "tokencraft {} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("TOKENCRAFT_SOURCE_REVISION").unwrap_or("unstamped")
        ),
        Some("mapping") => crate::mapping::run(args)?,
        Some("backup") => crate::backup::run(args, crate::backup::Action::Backup)?,
        Some("restore") => crate::backup::run(args, crate::backup::Action::Restore)?,
        Some("checkpoint") => crate::backup::run(args, crate::backup::Action::Checkpoint)?,
        Some("example-connector") => connector::serve_example(args)?,
        Some("dev") => {
            if let Some(options) = parse(args)? {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()?
                    .block_on(async {
                        let mut interrupt = signal(SignalKind::interrupt())?;
                        let mut terminate = signal(SignalKind::terminate())?;
                        tokio::select! {
                            biased;
                            _ = interrupt.recv() => { eprintln!("Stopping TokenCraft dev"); Ok(()) }
                            _ = terminate.recv() => { eprintln!("Stopping TokenCraft dev"); Ok(()) }
                            result = run(options) => result,
                        }
                    })?;
            }
        }
        _ => bail!("unknown command. Use tokencraft --help"),
    }
    Ok(())
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Options>> {
    let mut options = Options::default();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .with_context(|| format!("missing value for {}", arg.to_string_lossy()))
        };
        match arg.to_str() {
            Some("--help" | "-h") => {
                print!("{HELP}");
                return Ok(None);
            }
            Some("--state-dir") => options.state_dir = value()?.into(),
            Some("--api-port") => options.api_port = parse_port(value()?)?,
            Some("--connector-port") => options.connector_port = parse_port(value()?)?,
            Some("--db-port") => options.db_port = parse_port(value()?)?,
            Some("--web-port") => options.web_port = parse_port(value()?)?,
            Some("--gateway-bin") => options.gateway_bin = Some(value()?.into()),
            Some("--profile") => options.profile = Some(value()?.into()),
            Some("--console-dir") => options.console_dir = Some(value()?.into()),
            Some("--offline") => options.offline = true,
            _ => bail!(
                "unknown option {}. Use tokencraft --help",
                arg.to_string_lossy()
            ),
        }
    }
    let mut ports = vec![options.api_port, options.db_port, options.web_port];
    if options.profile.is_none() {
        ports.push(options.connector_port);
    }
    ports.sort_unstable();
    ensure!(
        ports.windows(2).all(|ports| ports[0] != ports[1]),
        "choose distinct ports for the API, database, connector, and console"
    );
    Ok(Some(options))
}

fn parse_port(value: OsString) -> Result<u16> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|port| *port >= 1024)
        .context("ports must be integers from 1024 through 65535")
}

async fn run(options: Options) -> Result<()> {
    let console = match options.console_dir.as_deref() {
        Some(directory) => Some(console::Console::prepare(directory, options.web_port, options.api_port).await?),
        None => None,
    };
    let state = State::open(&options.state_dir)?;
    for port in [options.api_port, options.db_port] {
        TcpListener::bind((HOST, port))
            .with_context(|| format!("loopback port {port} is unavailable"))?;
    }
    // Dev backups include this directory even when the connector runs separately.
    private_dir(&state.root.join("connector-jobs"))?;
    let providers = match options.profile.as_deref() {
        Some(directory) => Providers::Reviewed(Profile::load(directory)?),
        None => {
            Providers::Example(ExampleConnector::prepare(&state, options.connector_port).await?)
        }
    };
    let artifacts = state.root.join("artifacts");
    private_dir(&artifacts)?;
    let (mut postgres, database_url) = Postgres::start(&state, options.db_port, Mode::Dev).await?;
    tokio::select! {
        result = run_stack(&state, &options, providers, database_url, artifacts) => result,
        result = async {
            match console {
                Some(console) => console.serve().await,
                None => std::future::pending().await,
            }
        } => result,
        result = postgres.wait() => {
            let status = result.context("wait for dev PostgreSQL")?;
            bail!("dev PostgreSQL stopped ({status}). See postgres.log in the state directory");
        }
    }
}

enum Providers {
    Example(ExampleConnector),
    Reviewed(Profile),
}

struct ExampleConnector {
    listener: tokio::net::TcpListener,
    jobs: PathBuf,
    catalog: PathBuf,
    bindings: PathBuf,
    token: String,
}

impl ExampleConnector {
    async fn prepare(state: &State, port: u16) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((HOST, port))
            .await
            .context("bind the example connector")?;
        let jobs = state.root.join("connector-jobs");
        let catalog = state.root.join("catalog.json");
        let bindings = state.root.join("connector-bindings.json");
        write_private(&catalog, CATALOG)?;
        write_private(&bindings, &serde_json::to_vec(&fixture_binding(port)?)?)?;
        Ok(Self {
            listener,
            jobs,
            catalog,
            bindings,
            token: random_secret()?,
        })
    }

    async fn serve(self) -> Result<()> {
        axum::serve(self.listener, connector::router(self.jobs, &self.token)?).await?;
        bail!("the example connector stopped")
    }
}

async fn run_stack(
    state: &State,
    options: &Options,
    providers: Providers,
    database_url: String,
    artifacts: PathBuf,
) -> Result<()> {
    let (archives, archive_key) = state.local_archives()?;
    let profile = match &providers {
        Providers::Reviewed(profile) => Some(profile),
        Providers::Example(_) => None,
    };
    let binary = gateway_binary(options, &database_url, profile).await?;
    let mut gateway = Command::new(binary);
    gateway
        .current_dir(&state.root)
        .env_clear()
        .env("LC_ALL", "C")
        .env("DATABASE_URL", database_url)
        .env("FOUNDATION_ENV", "development")
        .env("API_GATEWAY_PORT", options.api_port.to_string())
        .env(
            "APP_BASE_URL",
            format!("http://localhost:{}", options.web_port),
        )
        .env("COOKIE_DOMAIN", "localhost")
        .env("COOKIE_SECURE", "false")
        .env("SESSION_SECRET", &state.secrets.session)
        .env("TOKENCRAFT_DEMO_PAYLOAD_KEY", &state.secrets.payload)
        .env("TOKENCRAFT_DEMO_ARTIFACT_DIR", artifacts)
        .env("TOKENCRAFT_DEMO_ARCHIVE_DIR", archives)
        .env("TOKENCRAFT_DEMO_ARCHIVE_KEY", archive_key)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let (example, _profile_files) = match providers {
        Providers::Example(example) => {
            gateway
                .env("TOKENCRAFT_CORE_CATALOG", &example.catalog)
                .env("TOKENCRAFT_CONNECTORS", &example.bindings)
                .env("TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE", &example.token);
            (Some(example), Vec::new())
        }
        Providers::Reviewed(profile) => (None, profile.configure(&mut gateway, state)?),
    };
    let mut gateway = gateway.spawn().context("start api-gateway")?;

    let api = format!("http://localhost:{}", options.api_port);
    let bundled_connector = example.is_some();
    let gateway = async {
        wait_for_gateway(&mut gateway, options.api_port).await?;
        println!("Gateway API  {api}");
        println!("Readiness    {api}/ready");
        if options.console_dir.is_some() {
            println!("Console      http://localhost:{}", options.web_port);
        }
        if bundled_connector {
            println!("Connector    http://{HOST}:{}", options.connector_port);
        }
        println!("State        {}", state.root.display());
        println!("Press Ctrl+C to stop.");
        bail!("api-gateway stopped ({})", gateway.wait().await?)
    };
    if let Some(example) = example {
        tokio::select! {
            result = example.serve() => result,
            result = gateway => result,
        }
    } else {
        gateway.await
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BindingFile {
    version: u8,
    bindings: [Binding; 1],
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    name: String,
    revision: String,
    origin: String,
    token_env: String,
}

fn fixture_binding(port: u16) -> Result<BindingFile> {
    let mut config: BindingFile =
        serde_json::from_slice(BINDINGS).context("load the example binding")?;
    config.bindings[0].origin = format!("http://{HOST}:{port}/");
    Ok(config)
}

#[derive(Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
enum CargoEvent {
    CompilerArtifact {
        executable: Option<PathBuf>,
        target: CargoTarget,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct CargoTarget {
    name: String,
}

async fn gateway_binary(
    options: &Options,
    database_url: &str,
    profile: Option<&Profile>,
) -> Result<PathBuf> {
    if let Some(path) = &options.gateway_bin {
        ensure!(
            path.is_file(),
            "--gateway-bin must point to an api-gateway executable"
        );
        return path
            .canonicalize()
            .context("resolve the specified gateway binary");
    }
    let cwd = env::current_dir()?;
    let Some(workspace) = cwd.ancestors().find(|directory| {
        directory.join("Cargo.toml").is_file()
            && directory
                .join("crates/bin/api-gateway/Cargo.toml")
                .is_file()
    }) else {
        let gateway = env::current_exe()?.with_file_name("api-gateway");
        ensure!(
            gateway.is_file(),
            "run from a TokenCraft source checkout or place api-gateway next to tokencraft"
        );
        return Ok(gateway);
    };

    eprintln!("Building the local gateway with the migrated database schema...");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command.args([
        "build",
        "--locked",
        "-p",
        "api_gateway",
        "--bin",
        "api-gateway",
        "--message-format=json-render-diagnostics",
    ]);
    if options.offline {
        command.arg("--offline");
    }
    if let Some(profile) = profile {
        profile.exclude_credentials(&mut command);
    }
    let mut build = command
        .current_dir(workspace)
        .env("DATABASE_URL", database_url)
        .env("SQLX_OFFLINE", "false")
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start Cargo to build api-gateway")?;
    let mut output = BufReader::new(build.stdout.take().context("capture Cargo messages")?).lines();
    let mut gateway = None;
    while let Some(line) = output.next_line().await? {
        if let Ok(CargoEvent::CompilerArtifact {
            executable: Some(path),
            target,
        }) = serde_json::from_str(&line)
        {
            if target.name == "api-gateway" {
                gateway = Some(path);
            }
        }
    }
    ensure!(build.wait().await?.success(), "api-gateway failed to build");
    gateway.context("Cargo did not report an api-gateway executable")
}

async fn wait_for_gateway(gateway: &mut Child, port: u16) -> Result<()> {
    let url = format!("http://{HOST}:{port}/ready");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(1))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = gateway.try_wait()? {
            bail!("api-gateway stopped before it was ready ({status})");
        }
        if client
            .get(&url)
            .send()
            .await
            .is_ok_and(|reply| reply.status() == reqwest::StatusCode::OK)
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "api-gateway did not become ready within 30 seconds"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_profiles_can_use_the_unused_example_port_for_the_api() {
        let options = parse(
            ["--profile", "./local-profile", "--api-port", "4019"]
                .into_iter()
                .map(OsString::from),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            options.profile.as_deref(),
            Some(std::path::Path::new("./local-profile"))
        );
        assert_eq!(options.api_port, 4019);
        assert!(parse(["--profile"].into_iter().map(OsString::from)).is_err());
    }

    #[test]
    fn parses_custom_ports_and_keeps_the_binding_in_sync() {
        let options = parse(
            [
                "--api-port",
                "4400",
                "--connector-port",
                "4419",
                "--offline",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .unwrap()
        .unwrap();
        assert_eq!(options.api_port, 4400);
        assert_eq!(
            fixture_binding(options.connector_port).unwrap().bindings[0].origin,
            "http://127.0.0.1:4419/"
        );
        assert!(options.offline);
        assert!(parse(["--api-port", "4019"].into_iter().map(OsString::from)).is_err());
        assert!(parse(["--db-port"].into_iter().map(OsString::from)).is_err());
    }
}
