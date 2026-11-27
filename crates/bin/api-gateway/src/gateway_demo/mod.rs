use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use axum::{Extension, Router};

use crate::state::AppState;

mod access;
mod adaptive;
mod archive_blobs;
mod archive_bucket;
mod archive_events;
mod archive_retirement;
mod archives;
mod artifact_files;
mod artifact_storage;
pub(crate) mod artifacts;
mod availability;
mod catalog;
pub(crate) mod catalog_import;
mod catalog_observations;
mod catalog_refresh;
pub(crate) mod chat;
pub(crate) mod content_history;
mod chat_input;
mod client_budgets;
mod compute_evidence;
mod compute_providers;
mod data_handling;
mod debug_trace;
mod dispatch_control;
mod exports;
mod handlers;
mod local_files;
mod mcp;
mod operations;
pub(crate) mod media;
mod project_keys;
mod projects;
mod polling;
mod process;
mod provider_access;
mod provider_onboarding;
mod provider_credentials;
mod provider_recheck;
mod providers;
pub(crate) use providers::completion as connector_completion;
mod quote;
mod quote_seals;
mod readiness;
mod capacity;
mod request_capacity;
mod submission_limits;
mod request_location;
mod response_bytes;
pub(crate) mod request_content;
mod requests;
mod route_performance;
mod route_preview;
mod routing_policy;
mod routing_settings;
mod sealed;
mod settlements;
mod usage;
mod webhooks;
mod workflows;

use catalog::Catalog;
use catalog_refresh::CatalogStore;
pub(crate) use routing_policy::PROVIDER_PREFERENCE_HEADER;
pub(crate) use chat::{
    CLIENT_REF, MAX_COST_NANOS, PROVIDER_HEADER, WEBHOOK_SUBSCRIPTION_ID, X_REQUEST_ID, X_SHOULD_RETRY,
};
pub(crate) use exports::{
    EXPORT_CLIENT_HEADER, EXPORT_PROJECT_HEADER, EXPORT_VERSION_HEADER, FEED_AS_OF_HEADER,
    HAS_MORE_HEADER, NEXT_AFTER_HEADER, PUBLICATION_PENDING_HEADER, SHA256_HEADER,
};
pub(crate) use handlers::REQUEST_ID_HEADER;
pub(crate) use requests::APP_REF;
pub(crate) use process::ProcessRole;

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        readiness::ready,
        submission_limits::get,
        submission_limits::increase,
        dispatch_control::get,
        dispatch_control::set,
        provider_recheck::schedule,
        handlers::catalog,
        handlers::models,
        handlers::scoped_models,
        handlers::model_quote,
        routing_settings::scoped_model_quote,
        handlers::quote,
        routing_settings::scoped_quote,
        client_budgets::list,
        client_budgets::get,
        client_budgets::update,
        handlers::submit,
        handlers::list,
        handlers::detail,
        handlers::events,
        debug_trace::get,
        debug_trace::baseline::get,
        debug_trace::otlp::download,
        route_preview::preview,
        provider_credentials::list,
        provider_credentials::save,
        provider_credentials::revoke,
        handlers::result,
        artifacts::download,
        handlers::cancel,
        usage::report,
        exports::feed,
        webhooks::handlers::destination_ids,
        webhooks::handlers::list_subscriptions,
        webhooks::handlers::get_subscription,
        webhooks::handlers::create,
        webhooks::handlers::rotate,
        webhooks::handlers::revoke,
        webhooks::handlers::list_deliveries,
        webhooks::handlers::detail,
        webhooks::handlers::redrive,
    ),
    components(schemas(crate::error::GatewayError), responses(RequestErrorResponse)),
    tags(
        (name = "native", description = "Quote, execute, and inspect requests with the configured local provider catalog."),
        (name = "webhooks", description = "Manage project webhooks with an organization administrator session. Available when the operator has configured webhook destinations."),
    ),
)]
pub(crate) struct NativeApiDoc;

#[derive(utoipa::ToResponse)]
#[response(description = "Gateway rejection or HTTP request extraction error.")]
#[allow(dead_code)]
enum RequestErrorResponse {
    Json(#[content("application/json")] crate::error::GatewayError),
    Text(#[content("text/plain")] String),
}

pub(crate) struct DemoConfig {
    role: ProcessRole,
    quote_seals: Arc<quote_seals::QuoteSeals>,
    readiness: Arc<readiness::Readiness>,
    capacity_access: Option<Arc<capacity::Access>>,
    legacy_operations: bool,
    catalog: Arc<CatalogStore>,
    webhooks: Option<Arc<webhooks::WebhookConfig>>,
    providers: Option<Arc<providers::ProviderRuntime>>,
    archives: Option<Arc<archive_blobs::ArchiveStorage>>,
    mcp: Option<mcp::Settings>,
    workflows: Option<Arc<workflows::Security>>,
}

#[derive(Clone)]
pub(crate) struct ScopedModels(Arc<CatalogStore>);

impl ScopedModels {
    pub(crate) fn contains_public(&self, slug: &str) -> bool {
        let catalog = self.0.current();
        catalog.offers.iter().any(|offer| {
            catalog::public_model_id(&offer.model) == slug && catalog.visible_to(offer, None)
        })
    }
}

impl DemoConfig {
    pub(crate) fn scoped_models(&self) -> ScopedModels {
        ScopedModels(Arc::clone(&self.catalog))
    }
}

pub(crate) fn media_router(
    state: &AppState,
    config: Option<&DemoConfig>,
) -> Router<AppState> {

    let gateway = config.map(|config| media::MediaGateway {
        app: state.clone(),
        catalog: Arc::clone(&config.catalog),
        quote_seals: Arc::clone(&config.quote_seals),
        providers: config.providers.clone(),
        webhooks: config.webhooks.clone(),
    });
        // Project-key middleware handles admission. The route never falls back to a hosted job.
        async fn project_key_required() -> axum::http::StatusCode {
            axum::http::StatusCode::UNAUTHORIZED
        }
        Router::new()
            .route("/v1/image", post(project_key_required))
            .route("/v1/video", post(project_key_required))
            .route("/v1/audio", post(project_key_required))
            .route("/v1/3d", post(project_key_required))
            .route_layer(axum::middleware::from_fn_with_state(gateway, media::project_media))
}

pub(crate) fn startup_config(
    environment: &config::Environment,
    database_url: &str,
    session_secret: &str,
    role: ProcessRole,
) -> anyhow::Result<Option<DemoConfig>> {
    let Some(path) = std::env::var_os("TOKENCRAFT_CORE_CATALOG") else {
        anyhow::ensure!(role == ProcessRole::Combined, "separate process roles require the scoped gateway profile");
        anyhow::ensure!(
            std::env::var_os("TOKENCRAFT_MCP_ORIGIN").is_none(),
            "TokenCraft MCP requires the scoped gateway profile"
        );
        return Ok(None);
    };
    let hosted = environment.is_hosted();
    if !hosted {
        anyhow::ensure!(environment.is_dev(), "set FOUNDATION_ENV to development, staging, or production");
        let db: sqlx::postgres::PgConnectOptions = database_url.parse()
            .map_err(|_| anyhow::anyhow!("invalid development database connection settings"))?;
        let host = db.get_host();
        anyhow::ensure!(
            host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()),
            "the development gateway requires a loopback database"
        );
    }
    let mut catalog = Catalog::load(Path::new(&path))?;
    anyhow::ensure!(
        !hosted || catalog.offers.iter().all(|offer| offer.adapter != catalog::Adapter::Mock),
        "the hosted catalog requires connector-backed offers"
    );
    let providers = providers::ProviderRuntime::load(&catalog)?.map(Arc::new);
    if let Some(runtime) = &providers {
        runtime.pin_catalog(&mut catalog)?;
    }
    let readiness = Arc::new(readiness::Readiness::new(
        role,
        providers.is_some(),
        providers.as_ref().and_then(|runtime| runtime.artifact_storage.as_deref())
            .map_or("postgresql", artifact_storage::ArtifactStorage::backend),
    ));
    Ok(Some(DemoConfig {
        role,
        quote_seals: Arc::new(quote_seals::QuoteSeals::new(session_secret)),
        readiness,
        capacity_access: capacity::Access::load()?,
        legacy_operations: matches!(std::env::var("TOKENCRAFT_ENABLE_LEGACY_OPERATIONS").as_deref(), Ok("true")),
        providers,
        catalog: Arc::new(CatalogStore::load(Arc::new(catalog))?),
        webhooks: webhooks::WebhookConfig::load(hosted)?.map(Arc::new),
        archives: archive_blobs::ArchiveStorage::load()?.map(Arc::new),
        mcp: if role.serves_api() { mcp::Settings::from_env(session_secret)? } else { None },
        workflows: workflows::Security::load(!hosted)?,
    }))
}

fn router(config: DemoConfig) -> Router<AppState> {
    let router = Router::new()
        .route(
            "/v1/projects/:project/providers",
            get(provider_onboarding::inventory)
                .post(provider_onboarding::preview)
                .layer(DefaultBodyLimit::max(32 * 1024)),
        )
        .route(
            "/v1/providers",
            get(provider_onboarding::organization_inventory)
                .post(provider_onboarding::organization_preview)
                .layer(DefaultBodyLimit::max(32 * 1024)),
        )
        .route("/v1/provider-access", put(provider_access::update))
        .route("/v1/compute-providers", get(compute_providers::inventory).put(compute_providers::update))
        .route("/v1/compute-providers/preview", post(compute_providers::preview).layer(DefaultBodyLimit::max(8 * 1024)))
        .route("/v1/routing-policy/history", get(provider_access::history))
        .route("/v1/rate-limits", get(submission_limits::get))
        .route("/v1/rate-limits/increase", post(submission_limits::increase).layer(DefaultBodyLimit::max(1024)))
        .route("/v1/provider-credentials/:binding", get(provider_credentials::list).put(provider_credentials::save))
        .route("/v1/provider-credentials/:binding/:version", delete(provider_credentials::revoke))
        .route("/v1/operations/dispatch", get(dispatch_control::get).put(dispatch_control::set))
        .route("/v1/projects/:project/requests/:request/recheck", post(provider_recheck::schedule))
        .route("/v1/catalog/offers", get(handlers::catalog))
        .route("/v1/catalog/models", get(handlers::models))
        .route("/v1/catalog/models/scoped", get(handlers::scoped_models))
        .route("/v1/catalog/quotes", post(handlers::model_quote))
        .route("/v1/catalog/quotes/scoped", post(routing_settings::scoped_model_quote))
        .route("/v1/quotes", post(handlers::quote))
        .route("/v1/quotes/scoped", post(routing_settings::scoped_quote))
        .route(
            "/v1/routing-policy",
            get(routing_settings::get).put(routing_settings::update),
        )
        .route(
            "/v1/routing-policy/preview",
            post(routing_settings::preview),
        )
        .route("/v1/chat/completions", post(chat::complete))
        .route("/v1/responses", post(chat::responses::create))
        .route("/v1/requests", post(handlers::submit).get(handlers::list))
        .route("/v1/model-requests", post(handlers::submit).get(handlers::model_list))
        .route("/v1/model-requests/:id", get(handlers::model_detail))
        .route("/v1/model-requests/:id/cancel", post(handlers::model_cancel))
        .route("/v1/model-requests/:id/result", get(handlers::result))
        .route("/v1/model-requests/:id/artifacts/:slot", get(artifacts::download))
        .route("/v1/requests/:id", get(handlers::detail))
        .route("/v1/requests/:id/location", get(request_location::get))
        .route("/v1/requests/:id/events", get(handlers::events))
        .route("/v1/requests/:id/trace", get(debug_trace::get))
        .route("/v1/requests/:id/trace/baseline", get(debug_trace::baseline::get))
        .route("/v1/requests/:id/trace/otlp", get(debug_trace::otlp::download))
        .route("/v1/connector-jobs/:id/completion", post(connector_completion::receive))
        .route("/v1/requests/:id/route-preview", post(route_preview::preview))
        .route("/v1/requests/:id/result", get(handlers::result))
        .route("/v1/requests/:id/content", get(content_history::get))
        .route("/v1/requests/:id/content/restore", post(content_history::restore_job::enqueue))
        .route("/v1/requests/:id/content/retrieve", post(content_history::retrieve))
        .route("/v1/requests/:id/content/artifacts/:slot", get(artifacts::download_archived))
        .route("/v1/requests/:id/artifacts/:slot", get(artifacts::download))
        .route("/v1/requests/:id/cancel", post(handlers::cancel))
        .route("/v1/usage", get(usage::report))
        .route("/v1/exports", get(exports::list).post(exports::create))
        .route("/v1/exports/events", get(exports::feed))
        .route("/v1/exports/:id", get(exports::detail))
        .route("/v1/exports/:id/data", get(exports::data))
        .route("/v1/projects", get(projects::list).post(projects::create))
        .route(
            "/v1/projects/:project/requests/:request/settlements",
            get(settlements::list).post(settlements::record),
        )
        .route(
            "/v1/projects/:project/requests/:request/content",
            delete(request_content::delete),
        )
        .route("/v1/projects/:project/client-budgets", get(client_budgets::list))
        .route(
            "/v1/projects/:project/client-budgets/:client_ref",
            get(client_budgets::get).put(client_budgets::update),
        )
        .route("/v1/projects/:project/grants", get(projects::grants))
        .route(
            "/v1/projects/:project/grants/:user",
            put(projects::set_grant).delete(projects::revoke_grant),
        )
        .route("/v1/projects/:project/credentials", get(project_keys::list))
        .route("/v1/projects/:project/credentials/attached", put(project_keys::select_attached))
        .route(
            "/v1/projects/:project/service-principals",
            post(project_keys::create_principal),
        )
        .route(
            "/v1/projects/:project/service-principals/:principal",
            delete(project_keys::revoke_principal),
        )
        .route(
            "/v1/projects/:project/service-principals/:principal/keys",
            post(project_keys::rotate_key),
        )
        .route(
            "/v1/projects/:project/keys/:key",
            delete(project_keys::revoke_key),
        );
    let router = if config.legacy_operations {
        router
            .route("/v1/setup/status", get(readiness::setup))
            .route("/v1/operations/queues", get(operations::queues))
            .route("/v1/operations/artifact-publication", get(operations::publication))
    } else {
        router
    };
    let router = match config.mcp {
        Some(settings) => router.merge(mcp::router(settings)),
        None => router,
    };
    let router = if let Some(security) = config.workflows {
        router.merge(workflows::router(security))
    } else {
        router
    };
    let router = router
        .layer(Extension(config.readiness))
        .layer(Extension(config.catalog))
        .layer(Extension(config.quote_seals))
        .layer(Extension(config.providers))
        .layer(Extension(config.webhooks.clone()));
    let router = match config.webhooks {
        Some(config) => router.merge(webhooks::router(config)),
        None => router,
    };
    let archive_storage = config.archives.clone();
    let router = match config.archives {
        Some(storage) => router.merge(archives::router(storage)),
        None => router,
    };
    router
        .layer(Extension(archive_storage))
        .layer(Extension(exports::Capacity::new(exports::MAX_EXPORTS_IN_FLIGHT)))
        .layer(Extension(artifacts::DownloadCapacity::new()))
        .layer(DefaultBodyLimit::max(8192))
}

pub(crate) fn probe_router<S: Clone + Send + Sync + 'static>(config: &DemoConfig) -> Router<S> {
    let router = Router::new().route("/ready", get(readiness::ready));
    let router = if let Some(access) = &config.capacity_access {
        router.route("/internal/capacity", get(capacity::inspect).layer(Extension(Arc::clone(access))))
    } else {
        router
    };
    router
        .layer(Extension(Arc::clone(&config.readiness)))
        .layer(Extension(config.providers.clone()))
        .layer(axum::middleware::map_response(crate::middleware::cache_control::apply))
}

pub(crate) fn mount_api_routes(base: Router<AppState>, config: DemoConfig) -> Router<AppState> {
    let probes = probe_router(&config);
    let readiness = Arc::clone(&config.readiness);
    base.merge(router(config))
        .route_layer(axum::middleware::from_fn_with_state(
            readiness, readiness::require_database_schema,
        ))
        .merge(probes)
}

pub(crate) fn spawn_background_tasks(pool: db::PgPool, config: &DemoConfig) {
    Arc::clone(&config.readiness).observe_database(pool.clone());
    // Workflow admission and dispatch fallback also evaluate rates on worker replicas.
    config.catalog.spawn();
    if let Some(storage) = config.providers.as_ref().and_then(|runtime| runtime.artifact_storage.as_ref()) {
        Arc::clone(storage).spawn(pool.clone(), Arc::clone(&config.readiness), config.role.runs_workers());
    }

    if !config.role.runs_workers() {
        return;
    }
    workflows::spawn(pool.clone(), config);
    if let Some(webhooks) = &config.webhooks {
        webhooks::spawn(pool.clone(), Arc::clone(webhooks), Arc::clone(&config.readiness));
    }
    if let Some(providers) = &config.providers {
        if let Some(store) = &providers.content_archive {
            Arc::clone(store).spawn(pool.clone(), Arc::clone(providers), Arc::clone(&config.readiness));
        }
        providers::spawn(
            pool.clone(), Arc::clone(providers), Arc::clone(&config.catalog), Arc::clone(&config.readiness),
        );
    }
    if let Some(storage) = &config.archives {
        archives::spawn(pool.clone(), Arc::clone(storage), Arc::clone(&config.readiness));
    }
    let readiness = Arc::clone(&config.readiness);
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        let mut maintenance = tokio::time::interval(Duration::from_secs(2));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let result = tokio::select! {
                _ = readiness.work_tick(&mut timer) => {
                    let result = advance(&pool).await;
                    readiness.queue_observed(result.is_ok());
                    result
                },
                _ = readiness.work_tick(&mut maintenance) => maintain(&pool).await,
            };
            if let Err(err) = result {
                // The worker reclaims the same durable state on the next tick.
                tracing::warn!(sqlstate = ?err.as_database_error().and_then(|e| e.code()), "gateway request worker tick failed");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    });
}

async fn maintain(pool: &db::PgPool) -> sqlx::Result<()> {
    db_queries::gateway_requests::expire_provider_work(pool).await?;
    db_queries::gateway_requests::expire_results(pool).await?;
    db_queries::gateway_requests::content_history::expire(pool).await?;
    db_queries::gateway_requests::queue_cooldowns::purge_expired(pool).await?;
    db_queries::gateway_requests::artifacts::purge_expired(pool).await?;
    db_queries::gateway_requests::chat_streams::purge_expired(pool).await?;
    db_queries::gateway_exports::publish(pool, None).await?;
    Ok(())
}

async fn advance(pool: &db::PgPool) -> sqlx::Result<()> {
    db_queries::gateway_requests::dispatch_mock_requests(pool).await?;
    db_queries::gateway_requests::complete_mock_attempts(pool).await?;
    Ok(())
}
