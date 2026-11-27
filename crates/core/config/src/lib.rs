//! Environment / config loading (environments.md).
//!
//! Every service reads its environment from **convention-named env files** loaded at startup by
//! [`load_env_files`] — `cargo run -p {service}` needs no hand-supplied env vars. Per service
//! (`{app}` = the binary name, files live in `crates/bin/{app}/config/`):
//!
//!   - `{app}.common.env`              — all environments (committed)
//!   - `{app}.development.env`         — dev defaults (committed; NO real secrets)
//!   - `{app}.development-secrets.env` — dev secrets (**gitignored**; see the `.example` template)
//!   - `{app}.production.env`          — prod config (committed; real secrets injected at deploy)
//!
//! `FOUNDATION_ENV` (read from the process environment BEFORE any file) picks the set; default is
//! dev. Real process env vars always win over file values, and more-specific files win over
//! `common` — so a deploy platform or a one-off `DATABASE_URL=… cargo run` override just works.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub mod workflow_tools;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Environment {
    Dev,
    Stage,
    Prod,
    Test,
}

impl Environment {
    fn parse(s: &str) -> Environment {
        match s {
            "prod" | "production" => Environment::Prod,
            "stage" | "staging" => Environment::Stage,
            "test" => Environment::Test,
            _ => Environment::Dev,
        }
    }
    pub fn is_dev(&self) -> bool {
        matches!(self, Environment::Dev)
    }

    pub fn is_hosted(&self) -> bool {
        matches!(self, Environment::Stage | Environment::Prod)
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub environment: Environment,
    pub database_url: String,
    pub api_gateway_port: u16,
    pub cdn_base_url: String,
    /// HMAC secret for signing session JWTs (auth.md §1). Rotate in prod.
    pub session_secret: String,
    /// Cookie `Domain` attribute — `.example.com` in prod so it spans the apex (auth.md §1).
    pub cookie_domain: String,
    /// Whether to set the `Secure` cookie attribute (off for plain-http local dev).
    pub cookie_secure: bool,
    /// Public base URL of the frontend app, used to build emailed links (e.g. the password-reset
    /// link `{app_base_url}/reset-password?token=…`).
    pub app_base_url: String,
    pub email: EmailConfig,
    /// Operator-set status/downtime message, surfaced verbatim by `GET /v1/api_status` for
    /// frontend banners (`API_STATUS_MESSAGE` — set at deploy, unset/blank = all quiet).
    pub status_message: Option<String>,
    /// Google OAuth client id (`GOOGLE_CLIENT_ID`) for "Sign in with Google" — a PUBLIC
    /// identifier (the GIS ID-token flow needs no client secret). Unset = Google SSO off.
    pub google_client_id: Option<String>,
    /// GitHub App credentials (`GITHUB_CLIENT_ID` + `GITHUB_CLIENT_SECRET`) for "Sign in with
    /// GitHub" (web application flow — the secret is used server-side only, for the code
    /// exchange). Requires BOTH; unset = GitHub SSO off.
    pub github_oauth: Option<GithubOauthConfig>,
    /// Object storage (R2 in prod, minio locally). `None` when unconfigured — avatar mirroring
    /// and uploads are disabled, not a boot failure.
    pub storage: Option<StorageConfig>,
}

/// S3-compatible object storage settings. For R2: endpoint
/// `https://{account_id}.r2.cloudflarestorage.com`, region `auto`.
#[derive(Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket_name: String,
    pub region: String,
}

// Debug redacts the secret so it can't leak through `{:?}` on Config.
impl std::fmt::Debug for StorageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageConfig")
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("bucket_name", &self.bucket_name)
            .field("region", &self.region)
            .finish()
    }
}

/// GitHub App OAuth credentials. The client id is public; the secret is not.
#[derive(Clone)]
pub struct GithubOauthConfig {
    pub client_id: String,
    pub client_secret: String,
}

// Debug redacts the secret so it can't leak through `{:?}` on Config.
impl std::fmt::Debug for GithubOauthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubOauthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

/// Email-sending config (email.md §1). A missing `resend_api_key` means email is unconfigured —
/// flows that would send just log and no-op rather than failing the request.
#[derive(Clone, Debug, Default)]
pub struct EmailConfig {
    pub resend_api_key: Option<String>,
    /// Sender in display form, e.g. `"Foundation <noreply@example.com>"`. Must be a Resend-verified
    /// sender domain.
    pub from_address: String,
}

fn opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Normalize a base URL so derived asset/link URLs are ALWAYS absolute:
/// blank-safe via [`opt`], trailing slash trimmed, and a scheme guaranteed (a scheme-less value
/// like `pub-xxx.r2.dev` would otherwise reach browsers as a *relative* path). Localhost gets
/// `http://`; anything else `https://`.
pub fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        trimmed.to_string()
    } else if trimmed.starts_with("localhost") || trimmed.starts_with("127.") {
        format!("http://{trimmed}")
    } else {
        format!("https://{trimmed}")
    }
}

/// Load the convention-named env files for `app_name` (see the crate docs for the file set).
/// Call this FIRST in every binary's `main`, before initializing logging (so `RUST_LOG` from a
/// file takes effect) and before [`Config::load`].
///
/// Semantics:
///   - `FOUNDATION_ENV` is read from the process environment only (a file can't switch it —
///     chicken-and-egg), defaulting to dev.
///   - Files load most-specific-first (`…-secrets` → `…{env}` → `common`); dotenvy never
///     overrides an already-set var, so earlier-loaded files and the real process env win.
///   - Each file is searched across the service dir (`crates/bin/{app}/config`), `./config`,
///     and `.` — so running from the repo root or the crate dir both work.
///   - Missing files are fine (the secrets file won't exist until someone creates it).
///
/// Returns the paths actually loaded so the caller can log them once tracing is up.
pub fn load_env_files(app_name: &str) -> Vec<PathBuf> {
    let environment = Environment::parse(&std::env::var("FOUNDATION_ENV").unwrap_or_default());

    let file_names: Vec<String> = match environment {
        Environment::Dev => vec![
            format!("{app_name}.development-secrets.env"), // gitignored
            format!("{app_name}.development.env"),
            format!("{app_name}.common.env"),
        ],
        Environment::Test => vec![format!("{app_name}.test.env"), format!("{app_name}.common.env")],
        Environment::Stage => vec![format!("{app_name}.stage.env"), format!("{app_name}.common.env")],
        Environment::Prod => vec![format!("{app_name}.production.env"), format!("{app_name}.common.env")],
    };

    let service_config_dir = format!("crates/bin/{app_name}/config");
    let search_dirs = [service_config_dir.as_str(), "config", "."];

    let mut loaded = Vec::new();
    for file_name in &file_names {
        for dir in &search_dirs {
            let path = Path::new(dir).join(file_name);
            if path.is_file() {
                // from_path never overrides vars that are already set.
                if dotenvy::from_path(&path).is_ok() {
                    loaded.push(path);
                }
                break; // first hit wins; don't load the same file name from a later dir
            }
        }
    }
    loaded
}

impl Config {
    /// Parse config from the process environment. Env files are NOT read here — call
    /// [`load_env_files`] first (every binary's `main` does).
    pub fn load() -> Result<Config> {
        Ok(Config {
            environment: Environment::parse(&std::env::var("FOUNDATION_ENV").unwrap_or_default()),
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is required")?,
            api_gateway_port: opt("API_GATEWAY_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(4000),
            cdn_base_url: normalize_base_url(
                &opt("CDN_BASE_URL").unwrap_or_else(|| "http://localhost:9000/foundation".into()),
            ),
            session_secret: std::env::var("SESSION_SECRET")
                .unwrap_or_else(|_| "dev-insecure-session-secret-change-me".into()),
            cookie_domain: std::env::var("COOKIE_DOMAIN").unwrap_or_else(|_| "localhost".into()),
            cookie_secure: opt("COOKIE_SECURE").map(|v| v == "true").unwrap_or(false),
            app_base_url: normalize_base_url(
                &opt("APP_BASE_URL").unwrap_or_else(|| "http://localhost:3000".into()),
            ),
            email: EmailConfig {
                resend_api_key: opt("RESEND_API_KEY"),
                from_address: std::env::var("EMAIL_FROM_ADDRESS")
                    .unwrap_or_else(|_| "Foundation <noreply@localhost>".into()),
            },
            status_message: opt("API_STATUS_MESSAGE"),
            google_client_id: opt("GOOGLE_CLIENT_ID"),
            github_oauth: match (opt("GITHUB_CLIENT_ID"), opt("GITHUB_CLIENT_SECRET")) {
                (Some(client_id), Some(client_secret)) => Some(GithubOauthConfig { client_id, client_secret }),
                _ => None,
            },
            storage: match (opt("R2_ENDPOINT"), opt("R2_ACCESS_KEY_ID"), opt("R2_SECRET_ACCESS_KEY"), opt("R2_BUCKET_NAME")) {
                (Some(endpoint), Some(access_key_id), Some(secret_access_key), Some(bucket_name)) => {
                    Some(StorageConfig {
                        endpoint,
                        access_key_id,
                        secret_access_key,
                        bucket_name,
                        region: opt("R2_REGION").unwrap_or_else(|| "auto".into()),
                    })
                }
                _ => None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_keeps_absolute_urls() {
        assert_eq!(normalize_base_url("https://pub-abc.r2.dev"), "https://pub-abc.r2.dev");
        assert_eq!(normalize_base_url("http://localhost:9000/foundation"), "http://localhost:9000/foundation");
    }

    #[test]
    fn base_url_trims_trailing_slash() {
        assert_eq!(normalize_base_url("https://cdn.foundation.ai/"), "https://cdn.foundation.ai");
    }

    #[test]
    fn base_url_adds_missing_scheme() {
        // A scheme-less host would reach browsers as a RELATIVE path — never allow it.
        assert_eq!(normalize_base_url("pub-abc.r2.dev"), "https://pub-abc.r2.dev");
        assert_eq!(normalize_base_url("localhost:9000/foundation"), "http://localhost:9000/foundation");
        assert_eq!(normalize_base_url("127.0.0.1:9000/x"), "http://127.0.0.1:9000/x");
    }
}
