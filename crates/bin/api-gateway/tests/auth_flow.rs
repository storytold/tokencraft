//! End-to-end auth flow tests through the real router against `foundation_test`.
//!
//! Uses unique emails per run so rows never collide; the test database accumulates harmlessly
//! in the disposable database selected by `FOUNDATION_TEST_DATABASE_URL`.

mod common;

use common::{app_and_pool, body_json, get, post, put, session_cookie, test_state, unique_email, JWT_SECRET};

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt; // oneshot

use api_gateway::auth::token::JwtKeys;
use api_gateway::AppState;

#[tokio::test]
async fn signup_sets_cookie_and_me_returns_user() {
  let (app, _pool) = app_and_pool().await;
  let email = unique_email();

  let resp = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": email, "password": "hunter2hunter2", "display_name": "Ada" }), None))
    .await
    .unwrap();
  assert_eq!(resp.status(), StatusCode::CREATED);
  assert_eq!(resp.headers()[header::CACHE_CONTROL], "private, no-store");
  let cookie = session_cookie(&resp).expect("signup should set a session cookie");
  let body = body_json(resp).await;
  assert_eq!(body["user"]["email"], email);

  // /me with the cookie → the same user.
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  assert_eq!(me.status(), StatusCode::OK);
  assert_eq!(me.headers()[header::CACHE_CONTROL], "private, no-store");
  assert_eq!(body_json(me).await["user"]["email"], email);

  // /me without a cookie → 401.
  let anon = app.oneshot(get("/v1/auth/me", None)).await.unwrap();
  assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
  assert_eq!(anon.headers()[header::CACHE_CONTROL], "private, no-store");
}

#[tokio::test]
async fn short_password_is_rejected() {
  let (app, _pool) = app_and_pool().await;
  let resp = app
    .oneshot(post("/v1/auth/signup", json!({ "email": unique_email(), "password": "short" }), None))
    .await
    .unwrap();
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_succeeds_with_correct_password_and_fails_otherwise() {
  let (app, _pool) = app_and_pool().await;
  let email = unique_email();
  let pw = "correct horse staple";

  app.clone().oneshot(post("/v1/auth/signup", json!({ "email": email, "password": pw }), None)).await.unwrap();

  let ok = app.clone().oneshot(post("/v1/auth/login", json!({ "email": email, "password": pw }), None)).await.unwrap();
  assert_eq!(ok.status(), StatusCode::OK);
  assert!(session_cookie(&ok).is_some());

  let bad = app.clone().oneshot(post("/v1/auth/login", json!({ "email": email, "password": "wrong" }), None)).await.unwrap();
  assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

  // Unknown email → same generic 401 (no enumeration).
  let unknown = app.oneshot(post("/v1/auth/login", json!({ "email": "nobody@example.com", "password": "x" }), None)).await.unwrap();
  assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn duplicate_signup_with_wrong_password_conflicts() {
  let (app, _pool) = app_and_pool().await;
  let email = unique_email();
  app.clone().oneshot(post("/v1/auth/signup", json!({ "email": email, "password": "firstpass1" }), None)).await.unwrap();

  let dup = app.oneshot(post("/v1/auth/signup", json!({ "email": email, "password": "different!" }), None)).await.unwrap();
  assert_eq!(dup.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn logout_revokes_the_session() {
  let (app, _pool) = app_and_pool().await;
  let email = unique_email();
  let signup = app.clone().oneshot(post("/v1/auth/signup", json!({ "email": email, "password": "logmeout12" }), None)).await.unwrap();
  let cookie = session_cookie(&signup).unwrap();

  // logout
  let out = app.clone().oneshot(post("/v1/auth/logout", json!({}), Some(&cookie))).await.unwrap();
  assert_eq!(out.status(), StatusCode::OK);

  // the same cookie is now rejected (session revoked → online check fails)
  let me = app.oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
}

/* ── Google SSO (GIS credential flow — tokens signed with a throwaway TEST key) ── */

const GOOGLE_TEST_KEY_PEM: &str = include_str!("fixtures/google_test_key.pem");
const GOOGLE_TEST_JWKS: &str = include_str!("fixtures/google_test_jwks.json");
const GOOGLE_TEST_CLIENT_ID: &str = "test-client-id.apps.googleusercontent.com";

/// An app whose Google verifier trusts the TEST keypair — full flow, no network, no secrets.
async fn app_with_google() -> (Router, db::PgPool) {
  let pool = db::test_fixtures::create_test_pool().await;
  let state = AppState {
    google: Some(Arc::new(api_gateway::auth::google::GoogleAuth::with_static_jwks(
      GOOGLE_TEST_CLIENT_ID.to_string(),
      GOOGLE_TEST_JWKS,
    ))),
    ..test_state(pool.clone())
  };
  (api_gateway::build_router(state), pool)
}

fn google_credential(sub: &str, email: &str, name: &str, email_verified: bool) -> String {
  use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
  let now = chrono::Utc::now().timestamp();
  let claims = json!({
    "iss": "https://accounts.google.com",
    "aud": GOOGLE_TEST_CLIENT_ID,
    "sub": sub,
    "email": email,
    "email_verified": email_verified,
    "name": name,
    "iat": now,
    "exp": now + 300,
  });
  let mut header = Header::new(Algorithm::RS256);
  header.kid = Some("foundation-test-key".to_string());
  encode(&header, &claims, &EncodingKey::from_rsa_pem(GOOGLE_TEST_KEY_PEM.as_bytes()).unwrap()).unwrap()
}

#[tokio::test]
async fn google_sso_signs_up_then_logs_in_on_repeat() {
  let (app, pool) = app_with_google().await;
  let email = unique_email();
  let sub = format!("10881{}", id_core::mint("g"));

  // First visit: verified credential → user + own org created, session cookie set.
  let credential = google_credential(&sub, &email, "SSO User", true);
  let first = app
    .clone()
    .oneshot(post("/v1/auth/oauth/google/callback", json!({ "credential": credential }), None))
    .await
    .unwrap();
  assert_eq!(first.status(), StatusCode::OK);
  let cookie = session_cookie(&first).unwrap();
  let body = body_json(first).await;
  assert_eq!(body["user"]["email"], email);
  assert_eq!(body["user"]["display_name"], "SSO User");
  let user_id: ids::UserId = body["user"]["id"].as_str().unwrap().parse().unwrap();

  // SSO emails count as verified, the session works, and created_via says sso_signup.
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  let me = body_json(me).await;
  assert_eq!(me["user"]["email_verified"], true);
  assert_eq!(me["user"]["has_password"], false);
  assert_eq!(me["sso_providers"], json!(["google"]));
  let sessions = db_queries::sessions::list_live_sessions_for_user(&pool, &user_id).await.unwrap();
  assert_eq!(sessions[0].created_via, enums::SessionCreationType::SsoSignup);

  // Same (provider, sub) again — even with a CHANGED email — logs into the same user.
  let second_credential = google_credential(&sub, &unique_email(), "SSO User", true);
  let second = app
    .clone()
    .oneshot(post("/v1/auth/oauth/google/callback", json!({ "credential": second_credential }), None))
    .await
    .unwrap();
  assert_eq!(second.status(), StatusCode::OK);
  assert_eq!(body_json(second).await["user"]["id"], user_id.to_string());
  let sessions = db_queries::sessions::list_live_sessions_for_user(&pool, &user_id).await.unwrap();
  assert_eq!(sessions[0].created_via, enums::SessionCreationType::SsoLogin);
}

#[tokio::test]
async fn google_sso_rejects_bad_credentials_and_collisions() {
  let (app, _pool) = app_with_google().await;

  // Garbage / unverifiable credential → 401.
  let bad = app
    .clone()
    .oneshot(post("/v1/auth/oauth/google/callback", json!({ "credential": "not-a-real-jwt" }), None))
    .await
    .unwrap();
  assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

  // Google-unverified email → 401 (we mark SSO emails verified, so we require Google's word).
  let email = unique_email();
  let unverified = google_credential(&format!("10882{}", id_core::mint("g")), &email, "X", false);
  let resp = app
    .clone()
    .oneshot(post("/v1/auth/oauth/google/callback", json!({ "credential": unverified }), None))
    .await
    .unwrap();
  assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

  // Email already registered with a password and NO Google link → 409, no silent merge.
  let taken = unique_email();
  let signup = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": taken, "password": "password1" }), None))
    .await
    .unwrap();
  assert_eq!(signup.status(), StatusCode::CREATED);
  let colliding = google_credential(&format!("10883{}", id_core::mint("g")), &taken, "X", true);
  let resp = app
    .clone()
    .oneshot(post("/v1/auth/oauth/google/callback", json!({ "credential": colliding }), None))
    .await
    .unwrap();
  assert_eq!(resp.status(), StatusCode::CONFLICT);

  // Unsupported provider → 400.
  let gh = app
    .clone()
    .oneshot(post("/v1/auth/oauth/github/callback", json!({ "credential": "x" }), None))
    .await
    .unwrap();
  assert_eq!(gh.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn google_sso_unconfigured_server_says_so() {
  let (app, _pool) = app_and_pool().await; // google: None
  let resp = app
    .oneshot(post("/v1/auth/oauth/google/callback", json!({ "credential": "anything" }), None))
    .await
    .unwrap();
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invite_create_resolve_and_accept() {
  let (app, pool) = app_and_pool().await;

  // Owner signs up.
  let owner_email = unique_email();
  let signup = app.clone().oneshot(post("/v1/auth/signup", json!({ "email": owner_email, "password": "ownerpass1" }), None)).await.unwrap();
  let owner_cookie = session_cookie(&signup).unwrap();
  let owner_id: ids::UserId = body_json(signup).await["user"]["id"].as_str().unwrap().parse().unwrap();

  // Find the owner's account (signup created it + an owner membership).
  let account_id = db_queries::members::list_memberships_for_user(&pool, &owner_id).await.unwrap()[0]
    .account_id
    .clone();

  // Owner creates an invitation.
  let invitee_email = unique_email();
  let inv = app
    .clone()
    .oneshot(post(&format!("/v1/accounts/{account_id}/invitations"), json!({ "email": invitee_email, "role": "member" }), Some(&owner_cookie)))
    .await
    .unwrap();
  assert_eq!(inv.status(), StatusCode::OK);
  let inv_body = body_json(inv).await;
  let code = inv_body["code"].as_str().unwrap().to_string();
  let invitation_id = inv_body["invitation_id"].as_str().unwrap().to_string();

  // Resolve via code (public).
  let resolved = app.clone().oneshot(get(&format!("/v1/invitations/{code}"), None)).await.unwrap();
  assert_eq!(resolved.status(), StatusCode::OK);
  assert_eq!(body_json(resolved).await["role"], "member");

  // Invitee signs up, then accepts.
  let joiner_signup = app.clone().oneshot(post("/v1/auth/signup", json!({ "email": invitee_email, "password": "joinerpass1" }), None)).await.unwrap();
  let joiner_cookie = session_cookie(&joiner_signup).unwrap();
  let joiner_id: ids::UserId = body_json(joiner_signup).await["user"]["id"].as_str().unwrap().parse().unwrap();

  let accept = app.clone().oneshot(post(&format!("/v1/invitations/{invitation_id}/accept"), json!({}), Some(&joiner_cookie))).await.unwrap();
  assert_eq!(accept.status(), StatusCode::OK);

  // The joiner is now a member of the owner's account.
  let mem = db_queries::members::find_membership(&pool, &account_id, &joiner_id).await.unwrap();
  assert_eq!(mem.unwrap().role, enums::MemberRole::Member);

  // A non-member cannot invite (403/404).
  let outsider_email = unique_email();
  let outsider_signup = app.clone().oneshot(post("/v1/auth/signup", json!({ "email": outsider_email, "password": "outsider12" }), None)).await.unwrap();
  let outsider_cookie = session_cookie(&outsider_signup).unwrap();
  let forbidden = app
    .oneshot(post(&format!("/v1/accounts/{account_id}/invitations"), json!({ "email": unique_email(), "role": "member" }), Some(&outsider_cookie)))
    .await
    .unwrap();
  assert!(forbidden.status() == StatusCode::NOT_FOUND || forbidden.status() == StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn api_status_reports_reachability_and_operator_message() {
  // Quiet by default: ok, no message.
  let (app, pool) = app_and_pool().await;
  let resp = app.clone().oneshot(get("/v1/api_status", None)).await.unwrap();
  assert_eq!(resp.status(), StatusCode::OK);
  let body = body_json(resp).await;
  assert_eq!(body["ok"], true);
  assert_eq!(body["message"], Value::Null);

  // With an operator message (API_STATUS_MESSAGE at deploy) it's surfaced verbatim.
  let state = AppState {
    status_message: Some("Scheduled maintenance at 03:00 UTC.".into()),
    ..test_state(pool)
  };
  let app = api_gateway::build_router(state);
  let resp = app.oneshot(get("/v1/api_status", None)).await.unwrap();
  assert_eq!(resp.status(), StatusCode::OK);
  let body = body_json(resp).await;
  assert_eq!(body["ok"], true);
  assert_eq!(body["message"], "Scheduled maintenance at 03:00 UTC.");
}

#[tokio::test]
async fn sso_only_user_sets_first_password_without_current_then_rules_apply() {
  let (app, _pool) = app_with_google().await;
  let email = unique_email();
  let sub = format!("10884{}", id_core::mint("g"));

  // SSO signup — /me reports has_password: false (drives the "Set password" form).
  let credential = google_credential(&sub, &email, "SSO User", true);
  let sso = app
    .clone()
    .oneshot(post("/v1/auth/oauth/google/callback", json!({ "credential": credential }), None))
    .await
    .unwrap();
  let cookie = session_cookie(&sso).unwrap();
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  assert_eq!(body_json(me).await["user"]["has_password"], false);

  // Setting the FIRST password needs no current_password.
  let set = app
    .clone()
    .oneshot(put("/v1/auth/password", json!({ "new_password": "brand-new-pass1" }), Some(&cookie)))
    .await
    .unwrap();
  assert_eq!(set.status(), StatusCode::OK);
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  assert_eq!(body_json(me).await["user"]["has_password"], true);

  // Email/password login now works alongside SSO.
  let login = app
    .clone()
    .oneshot(post("/v1/auth/login", json!({ "email": email, "password": "brand-new-pass1" }), None))
    .await
    .unwrap();
  assert_eq!(login.status(), StatusCode::OK);

  // From here the normal rules apply: current required (400 absent, 401 wrong).
  let missing = app
    .clone()
    .oneshot(put("/v1/auth/password", json!({ "new_password": "another-pass-1" }), Some(&cookie)))
    .await
    .unwrap();
  assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
  let wrong = app
    .clone()
    .oneshot(put(
      "/v1/auth/password",
      json!({ "current_password": "not-it", "new_password": "another-pass-1" }),
      Some(&cookie),
    ))
    .await
    .unwrap();
  assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
}

/// PUT with a JSON body + optional cookie (this file's `post` is POST-only).
#[tokio::test]
async fn settings_factoids_and_monogram_gradient() {
  let (app, _pool) = app_and_pool().await;
  let email = unique_email();
  let signup = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": email, "password": "password1" }), None))
    .await
    .unwrap();
  let cookie = session_cookie(&signup).unwrap();

  // Password signup: has_password true, no SSO connections, default (null) monogram gradient.
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  let me = body_json(me).await;
  assert_eq!(me["user"]["has_password"], true);
  assert_eq!(me["sso_providers"], json!([]));
  assert_eq!(me["user"]["avatar"]["monogram_gradient"], Value::Null);

  // Pick a gradient — stored, echoed, and visible on /me thereafter.
  let set = app
    .clone()
    .oneshot(put("/v1/auth/avatar/monogram", json!({ "gradient": "ocean" }), Some(&cookie)))
    .await
    .unwrap();
  assert_eq!(set.status(), StatusCode::OK);
  assert_eq!(body_json(set).await["avatar"]["monogram_gradient"], "ocean");
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  assert_eq!(body_json(me).await["user"]["avatar"]["monogram_gradient"], "ocean");

  // Unknown slugs are rejected at deserialization.
  let bad = app
    .clone()
    .oneshot(put("/v1/auth/avatar/monogram", json!({ "gradient": "neon-zebra" }), Some(&cookie)))
    .await
    .unwrap();
  assert!(bad.status().is_client_error(), "got {}", bad.status());
}

/* ── GitHub SSO (web application flow against a LOCAL mock github — no secrets) ── */

const GITHUB_TEST_CLIENT_ID: &str = "Iv23test";
const GITHUB_TEST_CLIENT_SECRET: &str = "test-secret-not-real";

/// A local stand-in for github.com: token exchange (validating our creds + the code),
/// /user, and /user/emails.
async fn mock_github(user_id: u64, email: &str) -> String {
  use axum::extract::Form;
  use axum::routing::{get as axget, post as axpost};
  use axum::Json as AxJson;

  let email = email.to_string();
  let token = axpost(|Form(form): Form<Vec<(String, String)>>| async move {
    let field = |k: &str| form.iter().find(|(fk, _)| fk == k).map(|(_, v)| v.as_str());
    if field("client_id") != Some(GITHUB_TEST_CLIENT_ID)
      || field("client_secret") != Some(GITHUB_TEST_CLIENT_SECRET)
      || field("code") != Some("good-code")
    {
      return AxJson(json!({ "error": "bad_verification_code" }));
    }
    AxJson(json!({ "access_token": "ghu_testtoken", "token_type": "bearer" }))
  });
  let user = axget(move || async move {
    AxJson(json!({ "id": user_id, "login": "octocat", "name": "Octo Cat", "email": null }))
  });
  let emails = axget(move || {
    let email = email.clone();
    async move { AxJson(json!([{ "email": email, "primary": true, "verified": true }])) }
  });
  let app = Router::new()
    .route("/login/oauth/access_token", token)
    .route("/user", user)
    .route("/user/emails", emails);
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
  format!("http://{addr}")
}

async fn app_with_github(mock_base: &str) -> (Router, db::PgPool) {
  let pool = db::test_fixtures::create_test_pool().await;
  let state = AppState {
    github: Some(Arc::new(api_gateway::auth::github::GithubAuth::with_endpoints(
      GITHUB_TEST_CLIENT_ID.into(),
      GITHUB_TEST_CLIENT_SECRET.into(),
      format!("{mock_base}/login/oauth/access_token"),
      mock_base.to_string(),
    ))),
    ..test_state(pool.clone())
  };
  (api_gateway::build_router(state), pool)
}

#[tokio::test]
async fn github_sso_signs_up_then_logs_in_on_repeat() {
  // Unique numeric id + email per run (the mock echoes them).
  let numeric_id = 100_000 + (chrono::Utc::now().timestamp_micros() % 1_000_000) as u64;
  let email = unique_email();
  let base = mock_github(numeric_id, &email).await;
  let (app, pool) = app_with_github(&base).await;

  // First visit: code exchanged, verified email fetched → user + org + session.
  let first = app
    .clone()
    .oneshot(post(
      "/v1/auth/oauth/github/callback",
      json!({ "code": "good-code", "code_verifier": "pkce-verifier-123" }),
      None,
    ))
    .await
    .unwrap();
  assert_eq!(first.status(), StatusCode::OK);
  let cookie = session_cookie(&first).unwrap();
  let body = body_json(first).await;
  assert_eq!(body["user"]["email"], email);
  assert_eq!(body["user"]["display_name"], "Octo Cat");
  let user_id: ids::UserId = body["user"]["id"].as_str().unwrap().parse().unwrap();

  // The factoids know, the email counts as verified, created_via says sso_signup.
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  let me = body_json(me).await;
  assert_eq!(me["sso_providers"], json!(["github"]));
  assert_eq!(me["user"]["email_verified"], true);
  assert_eq!(me["user"]["has_password"], false);
  let sessions = db_queries::sessions::list_live_sessions_for_user(&pool, &user_id).await.unwrap();
  assert_eq!(sessions[0].created_via, enums::SessionCreationType::SsoSignup);

  // Same numeric id again → logs into the same user (created_via sso_login).
  let second = app
    .clone()
    .oneshot(post("/v1/auth/oauth/github/callback", json!({ "code": "good-code" }), None))
    .await
    .unwrap();
  assert_eq!(second.status(), StatusCode::OK);
  assert_eq!(body_json(second).await["user"]["id"], user_id.to_string());
  let sessions = db_queries::sessions::list_live_sessions_for_user(&pool, &user_id).await.unwrap();
  assert_eq!(sessions[0].created_via, enums::SessionCreationType::SsoLogin);

  // A bad/stolen code → 401; a missing code → 400.
  let bad = app
    .clone()
    .oneshot(post("/v1/auth/oauth/github/callback", json!({ "code": "stolen" }), None))
    .await
    .unwrap();
  assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
  let missing = app
    .clone()
    .oneshot(post("/v1/auth/oauth/github/callback", json!({}), None))
    .await
    .unwrap();
  assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn github_sso_unconfigured_server_says_so() {
  let (app, _pool) = app_and_pool().await; // github: None
  let resp = app
    .oneshot(post("/v1/auth/oauth/github/callback", json!({ "code": "anything" }), None))
    .await
    .unwrap();
  assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/* ── Login methods: link / unlink SSO from settings ── */

#[tokio::test]
async fn login_methods_link_and_unlink_google() {
  let (app, _pool) = app_with_google().await;

  // A password user (email not yet verified — the password alone permits removal).
  let email = unique_email();
  let signup = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": email, "password": "password1" }), None))
    .await
    .unwrap();
  let cookie = session_cookie(&signup).unwrap();

  let list = app.clone().oneshot(get("/v1/auth/login_methods", Some(&cookie))).await.unwrap();
  let list = body_json(list).await;
  assert_eq!(list["password"]["is_enabled"], true);
  assert_eq!(list["sso_connections"], json!([]));

  // Link Google (provider email may differ from the account email — allowed by design).
  let sub = format!("10885{}", id_core::mint("g"));
  let credential = google_credential(&sub, &unique_email(), "Ada", true);
  let linked = app
    .clone()
    .oneshot(post("/v1/auth/login_methods/google", json!({ "credential": credential }), Some(&cookie)))
    .await
    .unwrap();
  assert_eq!(linked.status(), StatusCode::OK);
  let linked = body_json(linked).await;
  assert_eq!(linked["sso_connections"].as_array().unwrap().len(), 1);
  assert_eq!(linked["sso_connections"][0]["provider"], "google");
  let sso_id = linked["sso_connections"][0]["id"].as_str().unwrap().to_string();

  // Linking the same sub to a DIFFERENT user → 409, never a silent move.
  let other = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": unique_email(), "password": "password1" }), None))
    .await
    .unwrap();
  let other_cookie = session_cookie(&other).unwrap();
  let stolen = google_credential(&sub, &unique_email(), "Ada", true);
  let conflict = app
    .clone()
    .oneshot(post("/v1/auth/login_methods/google", json!({ "credential": stolen }), Some(&other_cookie)))
    .await
    .unwrap();
  assert_eq!(conflict.status(), StatusCode::CONFLICT);

  // Unlink (password set → allowed); the connect/disconnect pair is audited.
  let unlink = Request::builder()
    .method("DELETE")
    .uri(format!("/v1/auth/login_methods/{sso_id}"))
    .header(header::COOKIE, &cookie)
    .body(Body::empty())
    .unwrap();
  let resp = app.clone().oneshot(unlink).await.unwrap();
  assert_eq!(resp.status(), StatusCode::OK);
  let list = app.clone().oneshot(get("/v1/auth/login_methods", Some(&cookie))).await.unwrap();
  assert_eq!(body_json(list).await["sso_connections"], json!([]));

  let audit = app.clone().oneshot(get("/v1/user/audit_logs", Some(&cookie))).await.unwrap();
  let audit = body_json(audit).await;
  let events: Vec<&str> = audit["items"].as_array().unwrap().iter().map(|i| i["event"].as_str().unwrap()).collect();
  assert!(events.contains(&"sso_connected"), "events: {events:?}");
  assert!(events.contains(&"sso_disconnected"), "events: {events:?}");

  // Unknown id → 404.
  let ghost = ids::SsoConnectionId::generate();
  let resp = app
    .clone()
    .oneshot(
      Request::builder()
        .method("DELETE")
        .uri(format!("/v1/auth/login_methods/{ghost}"))
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unlink_is_refused_when_it_could_lock_the_user_out() {
  let (app, pool) = app_and_pool().await;

  // Hand-craft the dangerous state: NO password, UNVERIFIED email, one SSO connection.
  let email = unique_email();
  let user = db_queries::users::create_user(
    &pool,
    db_queries::users::NewUser {
      email: email.clone(),
      email_clean: email.clone(),
      email_original: email.clone(),
      password_hash: None,
      display_name: None,
      email_verified_at: None,
    },
  )
  .await
  .unwrap();
  let conn = db_queries::sso::create_sso_connection(
    &pool,
    db_queries::sso::NewSsoConnection {
      user_id: &user.id,
      provider: "google",
      provider_user_id: &format!("10886{}", id_core::mint("g")),
      provider_email: &email,
      provider_user_display_name: None,
    },
  )
  .await
  .unwrap();
  let session = db_queries::sessions::create_session(
    &pool,
    db_queries::sessions::NewSession {
      user_id: &user.id,
      expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
      ip: None,
      user_agent: None,
      device: None,
      impersonator_user_id: None,
      created_via: enums::SessionCreationType::SsoLogin,
    },
  )
  .await
  .unwrap();
  let token = JwtKeys::new(JWT_SECRET).mint(session.id.as_str(), user.id.as_str(), user.session_version).unwrap();
  let cookie = format!("foundation_session={token}");

  // Removal would strand them: no password to log in with, no verified email to reset with.
  let unlink = |cookie: String| {
    Request::builder()
      .method("DELETE")
      .uri(format!("/v1/auth/login_methods/{}", conn.id))
      .header(header::COOKIE, cookie)
      .body(Body::empty())
      .unwrap()
  };
  let refused = app.clone().oneshot(unlink(cookie.clone())).await.unwrap();
  assert_eq!(refused.status(), StatusCode::FORBIDDEN);

  // A password gives them a way back in — now removal is allowed.
  db_queries::users::set_password_hash(&pool, &user.id, "argon2$fakehash").await.unwrap();
  let allowed = app.clone().oneshot(unlink(cookie)).await.unwrap();
  assert_eq!(allowed.status(), StatusCode::OK);
}
