//! Router assembly + server bootstrap. `main.rs` stays thin over [`run`].

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::MatchedPath;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, ETAG, IF_NONE_MATCH, LOCATION, RETRY_AFTER};
use axum::http::{HeaderName, HeaderValue, Method};
use axum::routing::{delete, get, post, put};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::{DefaultOnResponse, OnResponse, TraceLayer};
use tracing::Level;

use crate::auth::token::JwtKeys;
use crate::{gateway_demo, hosted};
use crate::handlers::{
  account, api_keys, audit_logs, auth, avatar, catalog, files, invitations, login_methods, org, root,
  sessions,
};
use crate::middleware::trace_context::{self, TraceContext, TRACE_ID_HEADER};
use crate::state::AppState;

/// Connect the DB, build state + router, and serve until shutdown.
pub async fn run(mut cfg: config::Config) -> anyhow::Result<()> {
  let role = gateway_demo::ProcessRole::load()?;
  let scoped = std::env::var_os("TOKENCRAFT_CORE_CATALOG").is_some();
  let is_hosted = scoped && cfg.environment.is_hosted();
  let listen_ip = if is_hosted {
    hosted::configure(&mut cfg)?
  } else {
    anyhow::ensure!(
      std::env::var_os("TOKENCRAFT_LISTEN_IP").is_none(),
      "TOKENCRAFT_LISTEN_IP requires the hosted gateway profile"
    );
    if scoped { Ipv4Addr::LOCALHOST.into() } else { Ipv4Addr::UNSPECIFIED.into() }
  };
  let demo = gateway_demo::startup_config(&cfg.environment, &cfg.database_url, &cfg.session_secret, role)?;
  if demo.is_some() && !is_hosted {
    if role != gateway_demo::ProcessRole::Worker {
      anyhow::ensure!(cfg.session_secret.len() >= 32 && cfg.session_secret != "dev-insecure-session-secret-change-me",
        "set a strong, local JWT session secret before starting the TokenCraft demo");
    }
    cfg.email.resend_api_key = None;
    cfg.storage = None;
    cfg.google_client_id = None;
    cfg.github_oauth = None;
  }
  let pool = db::connect(&cfg.database_url).await?;
  tracing::info!("connected to database");
  if is_hosted || role != gateway_demo::ProcessRole::Combined {
    let mut connection = pool.acquire().await?;
    anyhow::ensure!(
      db::migrations_current(&mut connection).await?,
      "apply the matching TokenCraft migrations before starting this gateway profile"
    );
  }

  let addr = SocketAddr::from((listen_ip, cfg.api_gateway_port));
  let listener = tokio::net::TcpListener::bind(&addr).await?;
  if let Some(demo) = &demo {
    gateway_demo::spawn_background_tasks(pool.clone(), demo);
    if role == gateway_demo::ProcessRole::Worker {
      return serve(listener, gateway_demo::probe_router(demo), role).await;
    }
  }

  let state = AppState {
    pool,
    jwt: Arc::new(JwtKeys::new(&cfg.session_secret)),
    cookie_domain: cfg.cookie_domain.clone(),
    cookie_secure: cfg.cookie_secure,
    app_base_url: cfg.app_base_url.clone(),
    email_from: cfg.email.from_address.clone(),
    resend_api_key: cfg
      .email
      .resend_api_key
      .clone()
      .map(|k| Arc::new(email_client::ResendApiKey::new(k))),
    bucket: match &cfg.storage {
      Some(s) => Some(
        bucket_client::BucketClientBuilder::new()
          .endpoint(&s.endpoint)
          .access_key(&s.access_key_id)
          .secret_key(&s.secret_access_key)
          .region_name(&s.region)
          .bucket_name(&s.bucket_name)
          .build()
          .map_err(|e| anyhow::anyhow!("bucket client: {e}"))?,
      ),
      None => None,
    },
    cdn_base_url: cfg.cdn_base_url.clone(),
    environment: cfg.environment.clone(),
    status_message: cfg.status_message.clone(),
    google: cfg
      .google_client_id
      .clone()
      .map(|id| Arc::new(crate::auth::google::GoogleAuth::new(id))),
    github: cfg
      .github_oauth
      .clone()
      .map(|g| Arc::new(crate::auth::github::GithubAuth::new(g.client_id, g.client_secret))),
  };
  if state.resend_api_key.is_none() {
    tracing::warn!("RESEND_API_KEY not set — outbound email will be logged and skipped");
  }
  if state.bucket.is_none() {
    tracing::warn!("R2 storage not configured — avatar uploads disabled");
  }
  if state.google.is_none() {
    tracing::warn!("GOOGLE_CLIENT_ID not set — Google sign-in disabled");
  }
  if state.github.is_none() {
    tracing::warn!("GITHUB_CLIENT_ID/GITHUB_CLIENT_SECRET not set — GitHub sign-in disabled");
  }
  // Asset URLs are derived from this base — surface what we resolved, and catch the classic
  // dev misconfig: R2 secrets set but CDN base still pointing at the (dead) local-minio default.
  tracing::info!(cdn_base_url = %state.cdn_base_url, "asset links will use this CDN base");
  if state.bucket.is_some()
    && state.environment == config::Environment::Dev
    && state.cdn_base_url.contains("localhost:9000")
  {
    tracing::warn!(
      "R2 is configured but CDN_BASE_URL is still the local-minio default — avatar links will 404. \
       Set CDN_BASE_URL to your bucket's public URL (e.g. https://pub-<hash>.r2.dev) in \
       api-gateway.development-secrets.env"
    );
  }

  let app = build_router_inner(state, demo);
  serve(listener, app, role).await
}

async fn serve(listener: tokio::net::TcpListener, app: Router, role: gateway_demo::ProcessRole) -> anyhow::Result<()> {
  let addr = listener.local_addr()?;
  tracing::info!(?role, "api-gateway listening on http://{addr}");
  // with_connect_info: gives the trace span a peer address even when no proxy header is set.
  axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
    .with_graceful_shutdown(shutdown_signal())
    .await?;
  Ok(())
}

async fn shutdown_signal() {
  #[cfg(unix)]
  if let Ok(mut terminate) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
    tokio::select! {
      _ = terminate.recv() => {},
      _ = tokio::signal::ctrl_c() => {},
    }
    return;
  }
  if tokio::signal::ctrl_c().await.is_err() {
    std::future::pending::<()>().await;
  }
}

/// The product router group (auth.md §5). Split-seam kept clean so console/admin/webhooks-in
/// groups can mount here later.
pub fn build_router(state: AppState) -> Router {
  build_router_inner(state, None)
}

fn build_router_inner(
  state: AppState, demo: Option<gateway_demo::DemoConfig>,
) -> Router {
  // Browser calls come from the frontend origin (credentials/cookies included), so CORS must name
  // origins explicitly — wildcard + credentials is forbidden by the spec. We allow the configured
  // app plus the local dev servers.
  let is_hosted = demo.is_some() && state.environment.is_hosted();
  let console_origin = state.app_base_url.trim_end_matches('/').parse::<HeaderValue>().ok();
  let mut origins: Vec<_> = console_origin.iter().cloned().collect();
  if !is_hosted {
    origins.extend([HeaderValue::from_static("http://localhost:3000"),
      HeaderValue::from_static("http://127.0.0.1:3000")]);
  }
  let mut allowed_headers = vec![CONTENT_TYPE];
  if demo.is_some() {
    allowed_headers.extend([
      AUTHORIZATION,
      IF_NONE_MATCH,
      HeaderName::from_static("idempotency-key"),
      HeaderName::from_static("x-tokencraft-content-sha256"),
      HeaderName::from_static("traceparent"),
      HeaderName::from_static("tracestate"),
      HeaderName::from_static("baggage"),
      HeaderName::from_static(gateway_demo::CLIENT_REF),
      HeaderName::from_static(gateway_demo::APP_REF),
      HeaderName::from_static(gateway_demo::MAX_COST_NANOS),
      HeaderName::from_static(gateway_demo::PROVIDER_HEADER),
      HeaderName::from_static(gateway_demo::PROVIDER_PREFERENCE_HEADER),
      HeaderName::from_static(gateway_demo::WEBHOOK_SUBSCRIPTION_ID),
    ]);
  }
  let mut cors = CorsLayer::new()
    .allow_origin(origins)
    .allow_credentials(true)
    .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
    .allow_headers(allowed_headers);
  if demo.is_some() {
    cors = cors.expose_headers([
      ETAG,
      LOCATION,
      RETRY_AFTER,
      HeaderName::from_static(gateway_demo::REQUEST_ID_HEADER),
      HeaderName::from_static(TRACE_ID_HEADER),
      HeaderName::from_static(gateway_demo::X_REQUEST_ID),
      HeaderName::from_static(gateway_demo::X_SHOULD_RETRY),
      HeaderName::from_static(gateway_demo::NEXT_AFTER_HEADER),
      HeaderName::from_static(gateway_demo::HAS_MORE_HEADER),
      HeaderName::from_static(gateway_demo::SHA256_HEADER),
      HeaderName::from_static(gateway_demo::FEED_AS_OF_HEADER),
      HeaderName::from_static(gateway_demo::PUBLICATION_PENDING_HEADER),
      HeaderName::from_static(gateway_demo::EXPORT_PROJECT_HEADER),
      HeaderName::from_static(gateway_demo::EXPORT_CLIENT_HEADER),
      HeaderName::from_static(gateway_demo::EXPORT_VERSION_HEADER),
    ]);
  }

  // Use route templates and parsed IP addresses so headers and path parameters stay out of access logs.
  let on_response = DefaultOnResponse::new().level(Level::INFO);
  let trace = TraceLayer::new_for_http()
    .make_span_with(|request: &axum::http::Request<axum::body::Body>| {
      let ip = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
        .or_else(|| {
          request
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip())
        });
      let route = request.extensions().get::<MatchedPath>().map_or("<unmatched>", MatchedPath::as_str);
      let context = TraceContext::admission();
      tracing::info_span!(
        "http", method = %request.method(), route, ip = ?ip,
        trace_id = %context.trace_id, span_id = %context.span_id,
        request_id = tracing::field::Empty, execution_trace_id = tracing::field::Empty,
      )
    })
    .on_response(move |response: &axum::http::Response<axum::body::Body>, latency, span: &tracing::Span| {
      for (header, field) in [(gateway_demo::REQUEST_ID_HEADER, "request_id"), (TRACE_ID_HEADER, "execution_trace_id")] {
        if let Some(value) = response.headers().get(header).and_then(|value| value.to_str().ok()) {
          span.record(field, value);
        }
      }
      on_response.clone().on_response(response, latency, span);
    });

  let diagnostics = Router::new()
    .route("/", get(root::index_handler))
    .route("/health", get(catalog::health_handler))
    .route("/openapi.json", get(crate::openapi::document))
    .route("/v1/schemas/primitives", get(crate::openapi::primitive_schemas))
    .route("/v1/api_status", get(root::api_status_handler));
  let router = Router::new()
    .merge(gateway_demo::media_router(&state, demo.as_ref()))
    // auth
    .route("/v1/auth/signup", post(auth::signup_handler))
    .route("/v1/auth/login", post(auth::login_handler))
    .route("/v1/auth/logout", post(auth::logout_handler))
    .route("/v1/auth/me", get(auth::me_handler))
    .route("/v1/auth/oauth/:provider/callback", post(auth::oauth_callback_handler))
    // account security: password, email, forgot/reset, sessions
    .route("/v1/auth/password", put(account::change_password_handler))
    // login methods: the password entry + SSO connections (POST {provider} links, DELETE
    // {sso_id} unlinks — one axum route, two param meanings)
    .route("/v1/auth/login_methods", get(login_methods::list_login_methods_handler))
    .route(
      "/v1/auth/login_methods/:provider_or_id",
      post(login_methods::link_sso_handler).delete(login_methods::unlink_sso_handler),
    )
    .route("/v1/auth/email", put(account::change_email_handler))
    .route("/v1/auth/profile", put(account::update_profile_handler))
    .route("/v1/auth/email/verification", post(account::request_email_verification_handler))
    .route("/v1/auth/email/verification/confirm", post(account::confirm_email_verification_handler))
    .route("/v1/auth/password-reset", post(account::request_password_reset_handler))
    .route("/v1/auth/password-reset/confirm", post(account::confirm_password_reset_handler))
    .route(
      "/v1/auth/sessions",
      get(sessions::list_sessions_handler).delete(sessions::revoke_other_sessions_handler),
    )
    .route("/v1/auth/sessions/:id", delete(sessions::revoke_session_handler))
    // avatar: upload+crop / remove / preference / gravatar remove+refresh. (No GET — clients
    // render the CDN links returned by /me and these mutations; the gateway never serves bytes.)
    .route(
      "/v1/auth/avatar",
      post(avatar::upload_my_avatar_handler).delete(avatar::remove_my_avatar_handler),
    )
    .route("/v1/auth/avatar/preference", put(avatar::set_my_avatar_preference_handler))
    .route("/v1/auth/avatar/monogram", put(avatar::set_monogram_gradient_handler))
    .route("/v1/auth/avatar/gravatar", delete(avatar::remove_my_gravatar_handler))
    .route("/v1/auth/avatar/gravatar/refresh", post(avatar::refresh_my_gravatar_handler))
    // organizations: switcher list, roster, member removal, audit trails
    .route("/v1/accounts", get(org::list_my_accounts_handler))
    .route("/v1/accounts/:account_id/business", put(org::update_business_name_handler))
    // api keys: any member may list/create/delete (auth.md §4, revised 2026-07-21)
    .route(
      "/v1/accounts/:account_id/api_keys",
      get(api_keys::list_api_keys_handler).post(api_keys::create_api_key_handler),
    )
    .route("/v1/accounts/:account_id/api_keys/:key_id", delete(api_keys::delete_api_key_handler))
    .route("/v1/accounts/:account_id/members", get(org::list_account_members_handler))
    .route("/v1/accounts/:account_id/users", get(org::list_account_users_handler))
    .route("/v1/accounts/:account_id/members/:user_id", delete(org::remove_account_member_handler))
    .route("/v1/user/audit_logs", get(audit_logs::list_my_audit_logs_handler))
    .route("/v1/accounts/:account_id/audit_logs", get(audit_logs::list_account_audit_logs_handler))
    .route(
      "/v1/accounts/:account_id/members/:user_id/audit_logs",
      get(audit_logs::list_member_audit_logs_handler),
    )
    // accounts / invitations
    .route(
      "/v1/accounts/:account_id/invitations",
      post(invitations::create_invitation_handler).get(invitations::list_account_invitations_handler),
    )
    .route(
      "/v1/accounts/:account_id/invitations/:invitation_id",
      delete(invitations::cancel_invitation_handler),
    )
    .route("/v1/invitations/:code_or_token", get(invitations::resolve_invitation_handler))
    .route("/v1/invitations/:id/accept", post(invitations::accept_invitation_handler));
  let router = router.route("/v1/files", post(files::upload_file_handler));
  let router = router.merge(crate::model_reviews::router(demo.as_ref().map(gateway_demo::DemoConfig::scoped_models)));
  let router = match demo {
    Some(demo) => gateway_demo::mount_api_routes(router, demo),
    None => router,
  }.merge(diagnostics);
  let router = match console_origin.filter(|_| is_hosted) {
    Some(origin) => router.layer(axum::middleware::from_fn_with_state(origin, hosted::protect_session_origin)),
    None => router,
  };
  router
    // Avatar photos and generation input images are whole files; the axum default (2 MB)
    // is far too small.
    .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024))
    .layer(cors)
    .layer(trace)
    .layer(axum::middleware::from_fn(trace_context::scope))
    .with_state(state)
}
