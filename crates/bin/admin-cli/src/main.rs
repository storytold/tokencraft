//! admin-cli — migrations and ops one-offs (codebase_layout_backend.md §2). Migrations
//! are applied here. Hosted demo startup can initialize an empty database by explicit opt-in.

use anyhow::{bail, Result};

#[tokio::main]
async fn main() -> Result<()> {
    // Env files by convention (crates/bin/admin-cli/config/…) — no hand-supplied vars needed.
    let _ = config::load_env_files("admin-cli");
    let cfg = config::Config::load()?;
    let cmd = std::env::args().nth(1).unwrap_or_default();

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
                "admin-cli <command>\n  migrate          apply pending migrations\n  initialize-demo  initialize an empty demo or verify its existing schema\n  db-info          show db name, reachability, table count"
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
