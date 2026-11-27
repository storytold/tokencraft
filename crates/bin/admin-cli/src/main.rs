//! admin-cli — migrations and ops one-offs (codebase_layout_backend.md §2). Migrations
//! are applied here. Hosted demo startup can initialize an empty database by explicit opt-in.

use anyhow::{bail, ensure, Result};
use db::SchemaState;

#[tokio::main]
async fn main() -> Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    if cmd == "--version" {
        println!(
            "admin-cli {} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("TOKENCRAFT_SOURCE_REVISION").unwrap_or("unstamped")
        );
        return Ok(());
    }

    // Env files by convention (crates/bin/admin-cli/config/…) — no hand-supplied vars needed.
    let _ = config::load_env_files("admin-cli");
    let cfg = config::Config::load()?;

    match cmd.as_str() {
        "migrate" => {
            let pool = connect(&cfg).await?;
            db::migrate(&pool).await?;
            let n = db::table_count(&pool).await?;
            println!("migrations applied · {n} application tables in {}", db_name(&cfg.database_url));
        }
        "initialize-demo" => {
            let pool = connect(&cfg).await?;
            if db::initialize_demo(&pool).await? {
                println!("initialized empty demo schema");
            } else {
                println!("existing demo schema is compatible");
            }
        }
        "schema-status" => {
            let pool = connect(&cfg).await?;
            let report = db::schema_status(&pool).await?;
            let state = match report.state {
                SchemaState::Empty => "empty",
                SchemaState::Unmanaged => "unmanaged",
                SchemaState::UpgradeRequired => "upgrade_required",
                SchemaState::Current => "current",
                SchemaState::ForwardCompatible => "forward_compatible",
                SchemaState::Diverged => "diverged",
                SchemaState::IncompatibleNewer => "incompatible_newer",
            };
            println!("schema status: {state}");
            println!("binary schema: {}", report.binary_version);
            if let Some(version) = report.database_version {
                println!("database schema: {version}");
            }
            if !report.pending_versions.is_empty() {
                let versions = report.pending_versions.iter().map(i64::to_string).collect::<Vec<_>>();
                println!("pending migrations: {}", versions.join(","));
            }
            ensure!(report.state.is_compatible(), "database schema requires operator review");
        }
        "db-info" => {
            let pool = connect(&cfg).await?;
            let up = db::ping(&pool).await;
            let n = db::table_count(&pool).await?;
            println!("database: {}", db_name(&cfg.database_url));
            println!("reachable: {up}");
            println!("application tables: {n}");
        }
        "" | "help" | "-h" | "--help" => {
            eprintln!(
                "admin-cli <command>\n  --version        show the version and source revision\n  migrate          apply pending migrations\n  schema-status    inspect the database migration ledger without changing it\n  initialize-demo  initialize an empty demo or verify its existing schema\n  db-info          show db name, reachability, table count"
            );
        }
        other => bail!("unknown command: {other} (try `help`)"),
    }
    Ok(())
}

async fn connect(cfg: &config::Config) -> Result<db::PgPool> {
    if cfg.environment.is_hosted() {
        db::verify_hosted_url(&cfg.database_url)?;
    }
    db::connect(&cfg.database_url).await
}

fn db_name(url: &str) -> String {
    url.split('?')
        .next()
        .and_then(|u| u.rsplit('/').next())
        .unwrap_or("?")
        .to_string()
}
