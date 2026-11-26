//! The gateway's OpenAPI contract, combining the base API and the optional native provider profile.
//! The HTTP endpoint and `export-openapi` binary serialize the same document.

use std::sync::LazyLock;

use axum::body::Bytes;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use axum::Extension;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::gateway_demo::{artifacts, chat, connector_completion, content_history, request_content, NativeApiDoc};
use crate::handlers::{
  account, api_keys, audit_logs, auth, avatar, catalog, files, invitations, login_methods, org, root,
  sessions,
};
use crate::middleware::cache_control::PublicSchema;

#[derive(OpenApi)]
#[openapi(
  info(
    title = "TokenCraft API",
    description = "Discover model offers, quote and submit work, and inspect request and usage evidence. \
                   Interactive account operations use the `foundation_session` HttpOnly cookie. \
                   Project bearer keys authorize native and compatible request operations.",
    version = env!("CARGO_PKG_VERSION"),
  ),
  paths(
    document,
    primitive_schemas,
    root::index_handler,
    root::api_status_handler,
    catalog::health_handler,
    crate::model_reviews::list,
    auth::signup_handler,
    auth::login_handler,
    auth::logout_handler,
    auth::me_handler,
    auth::oauth_callback_handler,
    account::change_password_handler,
    account::change_email_handler,
    account::update_profile_handler,
    account::request_email_verification_handler,
    account::confirm_email_verification_handler,
    account::request_password_reset_handler,
    account::confirm_password_reset_handler,
    login_methods::list_login_methods_handler,
    login_methods::link_sso_handler,
    login_methods::unlink_sso_handler,
    sessions::list_sessions_handler,
    sessions::revoke_other_sessions_handler,
    sessions::revoke_session_handler,
    avatar::upload_my_avatar_handler,
    avatar::set_my_avatar_preference_handler,
    avatar::set_monogram_gradient_handler,
    avatar::remove_my_avatar_handler,
    avatar::remove_my_gravatar_handler,
    avatar::refresh_my_gravatar_handler,
    org::list_my_accounts_handler,
    org::list_account_members_handler,
    org::list_account_users_handler,
    org::remove_account_member_handler,
    org::update_business_name_handler,
    api_keys::list_api_keys_handler,
    api_keys::create_api_key_handler,
    api_keys::delete_api_key_handler,
    audit_logs::list_my_audit_logs_handler,
    audit_logs::list_account_audit_logs_handler,
    audit_logs::list_member_audit_logs_handler,
    invitations::create_invitation_handler,
    invitations::list_account_invitations_handler,
    invitations::cancel_invitation_handler,
    invitations::resolve_invitation_handler,
    invitations::accept_invitation_handler,
    files::upload_file_handler,
    submit_image,
    submit_video,
    submit_audio,
    submit_mesh,
    chat::complete,
    chat::responses::create,
    connector_completion::receive,
    content_history::get,
    content_history::retrieve,
    content_history::restore_job::enqueue,
    artifacts::download_archived,
    request_content::delete,
  ),
  components(schemas(crate::gateway_demo::media::MediaRequest, request_content::RemovedContent)),
  modifiers(&NativeContract, &SessionCookieSecurity),
  tags(
    (name = "meta", description = "Landing page and service health"),
    (name = "catalog", description = "The configured model catalog"),
    (name = "auth", description = "Signup, login, logout, identity"),
    (name = "account", description = "Password, email, profile, and recovery"),
    (name = "sessions", description = "The devices list and session revocation"),
    (name = "avatar", description = "Avatar upload/crop, preference, and gravatar mirroring"),
    (name = "org", description = "Organizations: switcher list, roster, membership"),
    (name = "api-keys", description = "API keys: list, mint (secret shown once), revoke"),
    (name = "invitations", description = "Inviting people into an organization"),
    (name = "audit-logs", description = "Keyset-paginated audit trails"),
    (name = "files", description = "Input uploads: `file_…` ids + CDN URLs for generation inputs"),
    (name = "admin", description = "Staff-only record browsing"),
    (name = "chat", description = "OpenAI-compatible text chat for the local provider profile"),
  )
)]
pub struct ApiDoc;

static DOCUMENT: LazyLock<Bytes> = LazyLock::new(|| {
  Bytes::from(ApiDoc::openapi().to_pretty_json().expect("OpenAPI document serializes"))
});

#[utoipa::path(
  get, path = "/openapi.json", operation_id = "getOpenApiDocument", tag = "meta",
  responses((status = 200, description = "OpenAPI document for the gateway and its optional provider profile.", body = Object))
)]
pub(crate) async fn document() -> impl IntoResponse {
  (
    Extension(PublicSchema),
    [(CONTENT_TYPE, "application/json")],
    (*DOCUMENT).clone(),
  )
}

#[utoipa::path(
  get, path = "/v1/schemas/primitives", tag = "catalog",
  responses((status = 200, description = "Versioned connector primitive input and result definitions.",
             body = Object, content_type = "application/schema+json"))
)]
pub(crate) async fn primitive_schemas() -> impl IntoResponse {
  (
    Extension(PublicSchema),
    [(CONTENT_TYPE, "application/schema+json")],
    Bytes::from_static(provider_mapping::Registry::builtin_json()),
  )
}

struct NativeContract;

impl Modify for NativeContract {
  fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
    openapi.merge(NativeApiDoc::openapi());
  }
}

/// Registers the `session_cookie` scheme referenced by `security(("session_cookie" = []))`
/// and the `api_key` scheme (`Authorization: Bearer foundation_api_…`, api_overview.md §auth).
struct SessionCookieSecurity;

impl Modify for SessionCookieSecurity {
  fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
    let components = openapi.components.get_or_insert_with(Default::default);
    components.add_security_scheme(
      "session_cookie",
      SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
        crate::auth::cookie::COOKIE_NAME,
        "HttpOnly session cookie set by signup/login",
      ))),
    );
    components.add_security_scheme(
      "api_key",
      SecurityScheme::Http(
        utoipa::openapi::security::HttpBuilder::new()
          .scheme(utoipa::openapi::security::HttpAuthScheme::Bearer)
          .description(Some(
            "Account-scoped API key (`foundation_api_…`) — canonical form `Authorization: Bearer`; \
             `X-API-Key`, `Authorization: Key`, bare `Authorization`, and Basic (key as username) \
             are also accepted.",
          ))
          .build(),
      ),
    );
    components.add_security_scheme(
      "connector_completion_token",
      SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer)
        .description(Some("Request-bound completion token supplied to an opted-in connector."))
        .build()),
    );
    components.add_security_scheme(
      "project_api_key",
      SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer)
        .description(Some("Project-scoped TokenCraft bearer key (`tc_project_…`)."))
        .build()),
    );
  }
}

macro_rules! document_media {
  ($operation:ident, $path:literal) => {
    #[utoipa::path(
      post, path = $path, tag = "media",
      request_body = crate::gateway_demo::media::MediaRequest,
      params(("Idempotency-Key" = Option<String>, Header, description = "Required to submit work.")),
      responses(
        (status = 200, description = "Quote preview or idempotent request replay"),
        (status = 202, description = "Request accepted"),
      ),
      security(("project_api_key" = []))
    )]
    #[allow(dead_code)]
    fn $operation() {}
  };
}

document_media!(submit_image, "/v1/image");
document_media!(submit_video, "/v1/video");
document_media!(submit_audio, "/v1/audio");
document_media!(submit_mesh, "/v1/3d");

#[cfg(test)]
mod tests {
  use super::*;

  /// Catch accidental omission of the documented gateway and provider paths.
  #[test]
  fn openapi_covers_every_route() {
    let doc = ApiDoc::openapi();
    // axum `:param` ↔ OpenAPI `{param}`
    let documented: std::collections::BTreeSet<String> = doc.paths.paths.keys().cloned().collect();
    assert!(documented.contains("/v1/auth/me"));
    assert!(documented.contains("/v1/chat/completions"));
    assert!(documented.contains("/v1/responses"));
    assert!(documented.contains("/ready"));
    assert!(!documented.contains("/v1/setup/status"));
    assert!(documented.contains("/v1/projects/{project}/requests/{request}/content"));
    for route in ["/v1/image", "/v1/video", "/v1/audio", "/v1/3d"] {
      assert!(documented.contains(route), "missing generation path {route}");
    }
    assert!(!documented.contains("/v1/operations/queues"));
    assert!(documented.contains("/v1/operations/dispatch"));
    assert!(documented.contains("/v1/projects/{project}/requests/{request}/recheck"));
    assert!(!documented.contains("/v1/operations/artifact-publication"));
    assert!(documented.contains("/v1/projects/{project}/client-budgets"));
    assert!(documented.contains("/v1/projects/{project}/client-budgets/{client_ref}"));
  }

  #[test]
  fn openapi_describes_project_webhook_administration() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    for (method, path, status) in [
      ("get", "/v1/projects/{project}/webhook-destinations", "200"),
      ("get", "/v1/projects/{project}/webhooks", "200"),
      ("post", "/v1/projects/{project}/webhooks", "201"),
      ("delete", "/v1/projects/{project}/webhooks/{subscription}", "204"),
      ("post", "/v1/projects/{project}/webhooks/{subscription}/rotate", "200"),
      ("get", "/v1/projects/{project}/webhook-deliveries", "200"),
      ("get", "/v1/projects/{project}/webhook-deliveries/{delivery}", "200"),
      ("post", "/v1/projects/{project}/webhook-deliveries/{delivery}/redrive", "204"),
    ] {
      let operation = &doc["paths"][path][method];
      assert!(operation["responses"][status].is_object(), "missing {status} for {method} {path}");
      assert_eq!(operation["security"], serde_json::json!([{"session_cookie": []}]));
      assert!(operation["parameters"].as_array().unwrap().iter().any(|parameter| {
        parameter["name"] == "account_id" && parameter["in"] == "query" && parameter["required"] == true
      }));
    }
    let schemas = &doc["components"]["schemas"];
    let subscription = &schemas["WebhookSubscription"];
    assert!(subscription["properties"]["signing_secret"].is_null());
    assert!(schemas["IssuedWebhookSubscription"].to_string().contains("signing_secret"));
    assert!(schemas["WebhookDeliveryDetail"]["properties"]["attempts"].is_object());
  }

  #[test]
  fn openapi_describes_native_request_workflow() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let paths = &doc["paths"];
    let schemas = &doc["components"]["schemas"];
    for (path, method) in [
      ("/openapi.json", "get"),
      ("/v1/catalog/models", "get"),
      ("/v1/quotes", "post"),
      ("/v1/quotes/scoped", "post"),
      ("/v1/requests", "get"),
      ("/v1/requests", "post"),
      ("/v1/requests/{id}", "get"),
      ("/v1/requests/{id}/events", "get"),
      ("/v1/requests/{id}/trace", "get"),
      ("/v1/requests/{id}/trace/baseline", "get"),
      ("/v1/requests/{id}/trace/otlp", "get"),
      ("/v1/requests/{id}/route-preview", "post"),
      ("/v1/requests/{id}/result", "get"),
      ("/v1/requests/{id}/artifacts/{slot}", "get"),
      ("/v1/requests/{id}/cancel", "post"),
      ("/v1/usage", "get"),
    ] {
      assert!(paths[path][method].is_object(), "missing operation {method} {path}");
    }

    let submit = &paths["/v1/requests"]["post"];
    let key = &submit["parameters"][0];
    assert_eq!(key["name"], "Idempotency-Key");
    assert_eq!(key["required"], true);
    assert_eq!(schemas["QuotedSubmission"]["required"], serde_json::json!(["quote_id", "input"]));
    for status in ["200", "202"] {
      let receipt = &submit["responses"][status];
      assert!(receipt["headers"]["Location"].is_object());
      assert!(receipt["headers"]["TokenCraft-Request-Id"].is_object());
    }

    let trace = &paths["/v1/requests/{id}/trace"]["get"];
    assert_eq!(trace["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/RequestTraceSnapshot");
    assert_eq!(schemas["RequestTraceSnapshot"]["properties"]["provider_path"]["$ref"], "#/components/schemas/ProviderPath");
    assert_eq!(schemas["ProviderPath"]["properties"]["attempts"]["items"]["$ref"], "#/components/schemas/ProviderAttempt");

    let baseline = &paths["/v1/requests/{id}/trace/baseline"]["get"];
    assert_eq!(baseline["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/RequestTraceBaseline");
    assert!(schemas["RequestTraceEvidence"]["properties"]["summary"].is_object());

    let preview = &paths["/v1/requests/{id}/route-preview"]["post"];
    assert_eq!(preview["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/RoutePreviewOptions");
    assert_eq!(preview["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/RoutePreview");
    assert_eq!(schemas["RoutePreviewOptions"]["additionalProperties"], false);
    assert!(schemas["RoutePreview"]["properties"]["recorded"].is_object());

    let feed = &paths["/v1/exports/events"]["get"]["responses"]["200"];
    assert!(feed["content"]["application/x-ndjson"].is_object());
    for header in ["X-TokenCraft-Next-After", "X-TokenCraft-Has-More", "X-TokenCraft-Publication-Pending", "X-TokenCraft-Client-Ref"] {
      assert!(feed["headers"][header].is_object(), "{header} is required in the feed schema");
    }

    let history = paths["/v1/requests"]["get"]["parameters"].as_array().unwrap();
    let query = |name: &str| history.iter().find(|entry| entry["name"] == name).unwrap();
    assert_eq!(query("app_ref")["required"], false);
    assert_eq!(query("status")["schema"], schemas["NativeRequest"]["properties"]["status"]);
    assert_eq!(query("cost_state")["schema"], schemas["NativeRequest"]["properties"]["cost_state"]);
    assert_eq!(query("to")["schema"]["format"], "date");
    let reasons = schemas["QuoteCandidateReason"]["enum"].as_array().unwrap();
    for reason in ["queue_cooling_down", "provider_credential_unavailable"] {
      assert!(reasons.iter().any(|entry| entry == reason));
    }
    assert_eq!(schemas["QuoteCandidate"]["properties"]["available_after"]["format"], "date-time");
    assert!(schemas["GroupBy"]["enum"].as_array().unwrap().iter().any(|group| group == "app"));
    let artifact = &paths["/v1/requests/{id}/artifacts/{slot}"]["get"]["responses"]["200"]["content"];
    assert_eq!(artifact["model/gltf-binary"]["schema"]["$ref"], "#/components/schemas/ArtifactBytes");
    assert_eq!(schemas["ArtifactBytes"]["format"], "binary");
  }

  #[test]
  fn openapi_describes_public_and_project_catalog_contracts() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let catalog = &doc["paths"]["/v1/catalog/offers"]["get"];
    assert_eq!(catalog["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
      "#/components/schemas/CatalogOffers");
    let public = &doc["paths"]["/v1/catalog/quotes"]["post"];
    let scoped = &doc["paths"]["/v1/catalog/quotes/scoped"]["post"];
    assert!(public.get("security").is_none());
    assert!(scoped["security"].as_array().unwrap().iter().any(|scheme| scheme.get("project_api_key").is_some()));
    assert!(scoped["parameters"].as_array().unwrap().iter().all(|param| param["name"] != "pinned_offer_id"));
    let schema = &public["responses"]["200"]["content"]["application/json"]["schema"];
    assert_eq!(schema["$ref"], "#/components/schemas/ModelQuote");
    assert_eq!(schema, &scoped["responses"]["200"]["content"]["application/json"]["schema"]);
    let candidate = &doc["components"]["schemas"]["ModelCandidate"]["properties"];
    assert!(candidate.get("provider").is_none() && candidate.get("offer_id").is_none());
  }

  #[test]
  fn openapi_describes_scoped_media_contract() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    assert!(doc["components"]["schemas"]["ScopedMediaRequest"].is_object());
    for path in ["/v1/image", "/v1/video", "/v1/audio", "/v1/3d"] {
      let operation = &doc["paths"][path]["post"];
      assert_eq!(operation["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/ScopedMediaRequest");
      assert!(operation["responses"]["202"].is_object());
      assert!(operation["security"].as_array().unwrap().iter().any(|scheme| scheme.get("project_api_key").is_some()));
    }
  }
}
