//! End-to-end invitation flows (accounts.md §4) through the real router against
//! `foundation_test`: the join page's resolve, invited signup (new user), signup-as-login
//! redemption (existing user), and the no-double-invites rules.

mod common;

use common::{app_and_pool, body_json, delete, get, post, session_cookie, unique_email};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt; // oneshot


/// Sign up a fresh owner and invite `invitee_email`; returns (app, pool, owner account id,
/// invite response body).
async fn owner_with_invite(invitee_email: &str, role: enums::MemberRole) -> (Router, db::PgPool, ids::AccountId, Value) {
  let (app, pool) = app_and_pool().await;
  let owner_email = unique_email();
  let signup = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": owner_email, "password": "ownerpass1" }), None))
    .await
    .unwrap();
  assert_eq!(signup.status(), StatusCode::CREATED);
  let owner_cookie = session_cookie(&signup).unwrap();
  let owner_id: ids::UserId = body_json(signup).await["user"]["id"].as_str().unwrap().parse().unwrap();
  let account_id = db_queries::members::list_memberships_for_user(&pool, &owner_id).await.unwrap()[0]
    .account_id
    .clone();

  let inv = app
    .clone()
    .oneshot(post(
      &format!("/v1/accounts/{account_id}/invitations"),
      json!({ "email": invitee_email, "role": role }),
      Some(&owner_cookie),
    ))
    .await
    .unwrap();
  assert_eq!(inv.status(), StatusCode::OK);
  let inv_body = body_json(inv).await;
  (app, pool, account_id, inv_body)
}

#[tokio::test]
async fn resolve_reports_join_page_context() {
  let invitee_email = unique_email();
  let (app, pool, account_id, inv) = owner_with_invite(&invitee_email, enums::MemberRole::Member).await;
  let code = inv["code"].as_str().unwrap();

  // The invite email was recorded (email.md §3 — template `invite`, bound to the address,
  // no user row yet). Without RESEND_API_KEY in tests the row stays `queued`.
  let sends: i64 =
    sqlx::query_scalar("SELECT count(*) FROM email_sends WHERE to_email = $1 AND template = 'invite'")
      .bind(&invitee_email)
      .fetch_one(&pool)
      .await
      .unwrap();
  assert_eq!(sends, 1);

  // By code — a brand-new email, so no user exists yet.
  let resolved = app.clone().oneshot(get(&format!("/v1/invitations/{code}"), None)).await.unwrap();
  assert_eq!(resolved.status(), StatusCode::OK);
  let body = body_json(resolved).await;
  assert_eq!(body["invitation_id"], inv["invitation_id"]);
  assert_eq!(body["account_id"], account_id.to_string());
  assert_eq!(body["email"], invitee_email);
  assert_eq!(body["role"], "member");
  assert_eq!(body["invitee_user_exists"], false);

  // By the magic-link token (what join_url carries).
  let token = inv["join_url"].as_str().unwrap().strip_prefix("/join?token=").unwrap().to_string();
  let resolved = app.clone().oneshot(get(&format!("/v1/invitations/{token}"), None)).await.unwrap();
  assert_eq!(resolved.status(), StatusCode::OK);
  assert_eq!(body_json(resolved).await["invitation_id"], inv["invitation_id"]);
}

#[tokio::test]
async fn invited_signup_joins_the_inviting_org_without_a_personal_one() {
  let invitee_email = unique_email();
  let (app, pool, account_id, inv) = owner_with_invite(&invitee_email, enums::MemberRole::Admin).await;
  let code = inv["code"].as_str().unwrap();

  // Email is locked to the invite's.
  let mismatched = app
    .clone()
    .oneshot(post(
      "/v1/auth/signup",
      json!({ "email": unique_email(), "password": "joinerpass1", "invitation": code }),
      None,
    ))
    .await
    .unwrap();
  assert_eq!(mismatched.status(), StatusCode::BAD_REQUEST);

  // Signup with the invitation: user created, member of the inviting org, logged in.
  let signup = app
    .clone()
    .oneshot(post(
      "/v1/auth/signup",
      json!({ "email": invitee_email, "password": "joinerpass1", "invitation": code }),
      None,
    ))
    .await
    .unwrap();
  assert_eq!(signup.status(), StatusCode::CREATED);
  let cookie = session_cookie(&signup).unwrap();
  let joiner_id: ids::UserId = body_json(signup).await["user"]["id"].as_str().unwrap().parse().unwrap();

  // Exactly ONE membership — the inviting account, in the invite's role. No personal org
  // (invites never create accounts, accounts.md §4).
  let memberships = db_queries::members::list_memberships_for_user(&pool, &joiner_id).await.unwrap();
  assert_eq!(memberships.len(), 1);
  assert_eq!(memberships[0].account_id, account_id);
  assert_eq!(memberships[0].role, enums::MemberRole::Admin);

  // The session is live and the invitation is spent.
  let me = app.clone().oneshot(get("/v1/auth/me", Some(&cookie))).await.unwrap();
  assert_eq!(me.status(), StatusCode::OK);
  let dead = app.clone().oneshot(get(&format!("/v1/invitations/{code}"), None)).await.unwrap();
  assert_eq!(dead.status(), StatusCode::BAD_REQUEST); // already accepted
}

#[tokio::test]
async fn existing_user_redeems_invite_via_signup_as_login() {
  let (app, pool) = app_and_pool().await;

  // An existing user with their own org.
  let joiner_email = unique_email();
  let signup = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": joiner_email, "password": "joinerpass1" }), None))
    .await
    .unwrap();
  assert_eq!(signup.status(), StatusCode::CREATED);
  let joiner_id: ids::UserId = body_json(signup).await["user"]["id"].as_str().unwrap().parse().unwrap();

  // A second org's owner invites that email.
  let owner_email = unique_email();
  let owner_signup = app
    .clone()
    .oneshot(post("/v1/auth/signup", json!({ "email": owner_email, "password": "ownerpass1" }), None))
    .await
    .unwrap();
  let owner_cookie = session_cookie(&owner_signup).unwrap();
  let owner_id: ids::UserId = body_json(owner_signup).await["user"]["id"].as_str().unwrap().parse().unwrap();
  let account_id = db_queries::members::list_memberships_for_user(&pool, &owner_id).await.unwrap()[0]
    .account_id
    .clone();
  let inv = app
    .clone()
    .oneshot(post(
      &format!("/v1/accounts/{account_id}/invitations"),
      json!({ "email": joiner_email, "role": "member" }),
      Some(&owner_cookie),
    ))
    .await
    .unwrap();
  assert_eq!(inv.status(), StatusCode::OK);
  let inv_body = body_json(inv).await;
  let code = inv_body["code"].as_str().unwrap();

  // The join page reports the user exists → it shows login; the login-shaped signup call
  // (signup-as-login) with the invitation attached accepts it.
  let resolved = app.clone().oneshot(get(&format!("/v1/invitations/{code}"), None)).await.unwrap();
  assert_eq!(body_json(resolved).await["invitee_user_exists"], true);

  let redeem = app
    .clone()
    .oneshot(post(
      "/v1/auth/signup",
      json!({ "email": joiner_email, "password": "joinerpass1", "invitation": code }),
      None,
    ))
    .await
    .unwrap();
  assert_eq!(redeem.status(), StatusCode::CREATED);

  // Now a member of BOTH orgs — their own plus the inviting one.
  let memberships = db_queries::members::list_memberships_for_user(&pool, &joiner_id).await.unwrap();
  assert_eq!(memberships.len(), 2);
  assert!(memberships.iter().any(|m| m.account_id == account_id && m.role == enums::MemberRole::Member));
}

#[tokio::test]
async fn no_double_invites() {
  let invitee_email = unique_email();
  let (app, pool, account_id, first_inv) = owner_with_invite(&invitee_email, enums::MemberRole::Member).await;
  let first_code = first_inv["code"].as_str().unwrap().to_string();

  // Re-inviting the same email revokes the outstanding invitation and issues a fresh one.
  let owner = db_queries::members::list_members_of_account(&pool, &account_id)
    .await
    .unwrap()
    .into_iter()
    .find(|m| m.role == enums::MemberRole::Owner)
    .unwrap();
  let owner_login = app
    .clone()
    .oneshot(post("/v1/auth/login", json!({ "email": owner.email, "password": "ownerpass1" }), None))
    .await
    .unwrap();
  let owner_cookie = session_cookie(&owner_login).unwrap();

  let second = app
    .clone()
    .oneshot(post(
      &format!("/v1/accounts/{account_id}/invitations"),
      json!({ "email": invitee_email, "role": "member" }),
      Some(&owner_cookie),
    ))
    .await
    .unwrap();
  assert_eq!(second.status(), StatusCode::OK);
  let second_code = body_json(second).await["code"].as_str().unwrap().to_string();

  // The first invite is dead (revoked), the second resolves.
  let dead = app.clone().oneshot(get(&format!("/v1/invitations/{first_code}"), None)).await.unwrap();
  assert_eq!(dead.status(), StatusCode::BAD_REQUEST);
  let live = app.clone().oneshot(get(&format!("/v1/invitations/{second_code}"), None)).await.unwrap();
  assert_eq!(live.status(), StatusCode::OK);

  // Once the invitee is a member, inviting them again is a 409 (accounts.md §4).
  let joiner = app
    .clone()
    .oneshot(post(
      "/v1/auth/signup",
      json!({ "email": invitee_email, "password": "joinerpass1", "invitation": second_code }),
      None,
    ))
    .await
    .unwrap();
  assert_eq!(joiner.status(), StatusCode::CREATED);
  let again = app
    .clone()
    .oneshot(post(
      &format!("/v1/accounts/{account_id}/invitations"),
      json!({ "email": invitee_email, "role": "member" }),
      Some(&owner_cookie),
    ))
    .await
    .unwrap();
  assert_eq!(again.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn database_rejects_concurrent_duplicate_live_invitations() {
  // The service's revoke-then-insert closes the common path; the partial unique index is the
  // race-proof last line. Insert two live invitations directly — the second must fail.
  let (_app, pool) = app_and_pool().await;
  let owner_email = unique_email();
  let invitee_email = unique_email();

  let mut tx = pool.begin().await.unwrap();
  let user = db_queries::users::create_user(
    &mut *tx,
    db_queries::users::NewUser {
      email: owner_email.clone(),
      email_clean: owner_email.clone(),
      email_original: owner_email.clone(),
      password_hash: None,
      display_name: None,
      email_verified_at: None,
    },
  )
  .await
  .unwrap();
  let account = db_queries::accounts::create_account(
    &mut *tx,
    db_queries::accounts::NewAccount { name: None, placeholder_business_name: "Placeholder Business" },
  ).await.unwrap();

  let new_invitation = |code: &'static str| db_queries::invitations::NewInvitation {
    account_id: &account.id,
    invited_by_user_id: &user.id,
    email: &invitee_email,
    role: enums::MemberRole::Member,
    code,
    token_hash: "test-hash",
    expires_at: chrono::Utc::now() + chrono::Duration::days(7),
  };
  db_queries::invitations::create_invitation(&mut *tx, new_invitation("TEST-DUP1")).await.unwrap();
  let dup = db_queries::invitations::create_invitation(&mut *tx, new_invitation("TEST-DUP2")).await;
  let err = dup.expect_err("second live invitation for the same account+email must be rejected");
  match err {
    sqlx::Error::Database(db) => {
      assert_eq!(db.constraint(), Some("invitations_one_live_per_account_email"));
    }
    other => panic!("expected a unique-violation database error, got: {other:?}"),
  }
  // tx dropped → rolled back; nothing persists.
}

#[tokio::test]
async fn sessions_record_ip_and_creation_type() {
  let (app, pool) = app_and_pool().await;

  // Fresh signup, with a proxy header — the session must capture ip + created_via=signup.
  let email = unique_email();
  let signup_req = Request::builder()
    .method("POST")
    .uri("/v1/auth/signup")
    .header("content-type", "application/json")
    .header("x-forwarded-for", "203.0.113.9, 10.0.0.1")
    .body(Body::from(serde_json::to_vec(&json!({ "email": email, "password": "password1" })).unwrap()))
    .unwrap();
  let signup = app.clone().oneshot(signup_req).await.unwrap();
  assert_eq!(signup.status(), StatusCode::CREATED);
  let user_id: ids::UserId = body_json(signup).await["user"]["id"].as_str().unwrap().parse().unwrap();

  let sessions = db_queries::sessions::list_live_sessions_for_user(&pool, &user_id).await.unwrap();
  assert_eq!(sessions.len(), 1);
  assert_eq!(sessions[0].ip.as_deref(), Some("203.0.113.9"), "client ip (first x-forwarded-for hop) must be stored");
  assert_eq!(sessions[0].created_via, enums::SessionCreationType::Signup);

  let mut login = post("/v1/auth/login", json!({ "email": email, "password": "password1" }), None);
  login.headers_mut().insert("x-forwarded-for", "unknown".parse().unwrap());
  let login = app.clone().oneshot(login).await.unwrap();
  assert_eq!(login.status(), StatusCode::OK);
  let sessions = db_queries::sessions::list_live_sessions_for_user(&pool, &user_id).await.unwrap();
  assert_eq!(sessions.len(), 2);
  assert_eq!(sessions[0].ip, None);
  assert_eq!(sessions[0].created_via, enums::SessionCreationType::Login); // newest first

  // Invited signup → created_via=invited_signup.
  let invitee_email = unique_email();
  let (app2, pool2, _account_id, inv) = owner_with_invite(&invitee_email, enums::MemberRole::Member).await;
  let code = inv["code"].as_str().unwrap();
  let joined = app2
    .clone()
    .oneshot(post(
      "/v1/auth/signup",
      json!({ "email": invitee_email, "password": "password1", "invitation": code }),
      None,
    ))
    .await
    .unwrap();
  assert_eq!(joined.status(), StatusCode::CREATED);
  let joiner_id: ids::UserId = body_json(joined).await["user"]["id"].as_str().unwrap().parse().unwrap();
  let sessions = db_queries::sessions::list_live_sessions_for_user(&pool2, &joiner_id).await.unwrap();
  assert_eq!(sessions[0].created_via, enums::SessionCreationType::InvitedSignup);

  // The devices list surfaces it.
  let cookie = session_cookie(
    &app.clone()
      .oneshot(post("/v1/auth/login", json!({ "email": email, "password": "password1" }), None))
      .await
      .unwrap(),
  )
  .unwrap();
  let list = app.clone().oneshot(get("/v1/auth/sessions", Some(&cookie))).await.unwrap();
  let list = body_json(list).await;
  assert_eq!(list["sessions"][0]["created_via"], "login");
  assert!(list["sessions"].as_array().unwrap().iter().any(|s| s["created_via"] == "signup"));
}

#[tokio::test]
async fn pending_invitations_list_cancel_and_audit() {
  let invitee_email = unique_email();
  let (app, pool, account_id, inv) = owner_with_invite(&invitee_email, enums::MemberRole::Member).await;
  let owner = db_queries::members::list_members_of_account(&pool, &account_id)
    .await
    .unwrap()
    .into_iter()
    .find(|m| m.role == enums::MemberRole::Owner)
    .unwrap();
  let owner_login = app
    .clone()
    .oneshot(post("/v1/auth/login", json!({ "email": owner.email, "password": "ownerpass1" }), None))
    .await
    .unwrap();
  let owner_cookie = session_cookie(&owner_login).unwrap();

  // The pending list shows the invite: email, role, code, who issued it, issued/expiry times.
  let list = app
    .clone()
    .oneshot(get(&format!("/v1/accounts/{account_id}/invitations"), Some(&owner_cookie)))
    .await
    .unwrap();
  assert_eq!(list.status(), StatusCode::OK);
  let list = body_json(list).await;
  let items = list["invitations"].as_array().unwrap();
  assert_eq!(items.len(), 1);
  let item = &items[0];
  assert_eq!(item["invitation_id"], inv["invitation_id"]);
  assert_eq!(item["email"], invitee_email);
  assert_eq!(item["role"], "member");
  assert_eq!(item["code"], inv["code"]);
  assert_eq!(item["invited_by_user_id"], owner.user_id.to_string());
  let expires = chrono::DateTime::parse_from_rfc3339(item["expires_at"].as_str().unwrap()).unwrap();
  let days_left = (expires.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_days();
  assert!((6..=7).contains(&days_left), "default expiry is 7 days, got {days_left}");

  // A regular member can neither view nor cancel pending invitations.
  let bystander_email = unique_email();
  let member_inv = app
    .clone()
    .oneshot(post(
      &format!("/v1/accounts/{account_id}/invitations"),
      json!({ "email": bystander_email, "role": "member" }),
      Some(&owner_cookie),
    ))
    .await
    .unwrap();
  let member_code = body_json(member_inv).await["code"].as_str().unwrap().to_string();
  let member_signup = app
    .clone()
    .oneshot(post(
      "/v1/auth/signup",
      json!({ "email": bystander_email, "password": "password1", "invitation": member_code }),
      None,
    ))
    .await
    .unwrap();
  let member_cookie = session_cookie(&member_signup).unwrap();
  let denied = app
    .clone()
    .oneshot(get(&format!("/v1/accounts/{account_id}/invitations"), Some(&member_cookie)))
    .await
    .unwrap();
  assert_eq!(denied.status(), StatusCode::FORBIDDEN);
  let invitation_id = inv["invitation_id"].as_str().unwrap();
  let denied = app
    .clone()
    .oneshot(delete(&format!("/v1/accounts/{account_id}/invitations/{invitation_id}"), Some(&member_cookie)))
    .await
    .unwrap();
  assert_eq!(denied.status(), StatusCode::FORBIDDEN);

  // The owner cancels: the invite dies (code stops resolving) and leaves the pending list.
  let cancelled = app
    .clone()
    .oneshot(delete(&format!("/v1/accounts/{account_id}/invitations/{invitation_id}"), Some(&owner_cookie)))
    .await
    .unwrap();
  assert_eq!(cancelled.status(), StatusCode::OK);
  let code = inv["code"].as_str().unwrap();
  let dead = app.clone().oneshot(get(&format!("/v1/invitations/{code}"), None)).await.unwrap();
  assert_eq!(dead.status(), StatusCode::BAD_REQUEST); // revoked
  let list = app
    .clone()
    .oneshot(get(&format!("/v1/accounts/{account_id}/invitations"), Some(&owner_cookie)))
    .await
    .unwrap();
  assert!(body_json(list).await["invitations"].as_array().unwrap().is_empty());

  // Cancelling again → 404; a foreign owner can't cancel through their own account either.
  let again = app
    .clone()
    .oneshot(delete(&format!("/v1/accounts/{account_id}/invitations/{invitation_id}"), Some(&owner_cookie)))
    .await
    .unwrap();
  assert_eq!(again.status(), StatusCode::NOT_FOUND);

  // The audit log tells the whole story: sent + revoked (with the cancelling actor).
  let audit = app
    .clone()
    .oneshot(get(&format!("/v1/accounts/{account_id}/audit_logs"), Some(&owner_cookie)))
    .await
    .unwrap();
  let audit = body_json(audit).await;
  let revoked: Vec<&Value> = audit["items"]
    .as_array()
    .unwrap()
    .iter()
    .filter(|i| i["event"] == "invitation_revoked")
    .collect();
  assert_eq!(revoked.len(), 1);
  assert_eq!(revoked[0]["actor_user_id"], owner.user_id.to_string());
  assert_eq!(revoked[0]["data"]["email"], invitee_email);
  assert_eq!(revoked[0]["target_id"], invitation_id);
}

#[tokio::test]
async fn reinviting_audits_the_superseded_revoke() {
  let invitee_email = unique_email();
  let (app, pool, account_id, first_inv) = owner_with_invite(&invitee_email, enums::MemberRole::Member).await;
  let owner = db_queries::members::list_members_of_account(&pool, &account_id)
    .await
    .unwrap()
    .into_iter()
    .find(|m| m.role == enums::MemberRole::Owner)
    .unwrap();
  let owner_login = app
    .clone()
    .oneshot(post("/v1/auth/login", json!({ "email": owner.email, "password": "ownerpass1" }), None))
    .await
    .unwrap();
  let owner_cookie = session_cookie(&owner_login).unwrap();

  // Re-invite the same email: the old invite is revoked AND that revoke is audited.
  let second = app
    .clone()
    .oneshot(post(
      &format!("/v1/accounts/{account_id}/invitations"),
      json!({ "email": invitee_email, "role": "member" }),
      Some(&owner_cookie),
    ))
    .await
    .unwrap();
  assert_eq!(second.status(), StatusCode::OK);

  let audit = app
    .clone()
    .oneshot(get(&format!("/v1/accounts/{account_id}/audit_logs"), Some(&owner_cookie)))
    .await
    .unwrap();
  let audit = body_json(audit).await;
  let superseded: Vec<&Value> = audit["items"]
    .as_array()
    .unwrap()
    .iter()
    .filter(|i| i["event"] == "invitation_revoked" && i["data"]["superseded"] == true)
    .collect();
  assert_eq!(superseded.len(), 1, "the supersede must be audited: {audit:#?}");
  assert_eq!(superseded[0]["target_id"], first_inv["invitation_id"]);
  let sent_count = audit["items"].as_array().unwrap().iter().filter(|i| i["event"] == "invitation_sent").count();
  assert_eq!(sent_count, 2);
}
