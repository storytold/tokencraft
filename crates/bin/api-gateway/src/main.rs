//! api-gateway binary — thin: load env files (by convention), init tracing, hand off to the
//! library. `cargo run -p api_gateway` needs no hand-supplied env vars.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let mut args = std::env::args_os().skip(1);
  match args.next() {
    Some(command) if command == "catalog" => return api_gateway::catalog_command(args).await,
    Some(command) if command == "--version" => {
      println!(
        "api-gateway {} ({})",
        env!("CARGO_PKG_VERSION"),
        option_env!("TOKENCRAFT_SOURCE_REVISION").unwrap_or("unstamped")
      );
      return Ok(());
    }
    _ => {}
  }

  // Env files first, so RUST_LOG from a file shapes the tracing filter below.
  let loaded = if std::env::var_os("TOKENCRAFT_CORE_CATALOG").is_some()
  {
    Vec::new()
  } else {
    config::load_env_files("api-gateway")
  };

  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,api_gateway=debug".into()),
    )
    .init();

  for path in &loaded {
    tracing::info!("loaded env file: {}", path.display());
  }

  let cfg = config::Config::load()?;
  api_gateway::run(cfg).await
}
