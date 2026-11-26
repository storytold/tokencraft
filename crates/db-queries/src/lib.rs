//! The ONLY SQL in the codebase (codebase_layout_backend.md §4). Queries are grouped by table,
//! **one query per file**; each table module also declares its row struct.
//!
//! RULES (see `CLAUDE.md`): always use the compile-checked `sqlx::query!` / `query_as!` /
//! `query_scalar!` macros (never runtime string queries), validated against the `.sqlx/` offline
//! cache. Ids are strongly typed (`ids::UserId`, …) — columns decode via `AS "col: Type"`
//! overrides, params bind `.as_str()`. Every fn is executor-generic so it composes with a pool
//! or a `&mut Transaction`.

pub mod account_audit_logs;
pub mod accounts;
pub mod api_keys;
pub mod email_changes;
pub mod email_sends;
pub mod email_verifications;
pub mod files;
pub mod gateway_mcp;
pub mod gateway_operations;
pub mod gateway_dispatch_controls;
pub mod gateway_provider_credentials;
pub mod gateway_requests;
pub mod gateway_supplier_settlements;
pub mod gateway_projects;
pub mod gateway_routing_policies;
pub mod gateway_exports;
pub mod gateway_archive_retirement;
pub mod gateway_archive_schedule;
pub mod gateway_archives;
pub mod gateway_webhooks;
pub mod inbound_webhooks;
pub mod invitations;
pub mod job_events;
pub mod job_slices;
pub mod jobs;
pub mod members;
pub mod outputs;
pub mod password_resets;
pub mod provider_calls;
pub mod sessions;
pub mod sso;
pub mod user_audit_logs;
pub mod user_avatar_files;
pub mod user_avatars;
pub mod users;
pub mod workflows;

// Row structs are re-exported at the crate root for ergonomic imports by consumers.
pub use account_audit_logs::AccountAuditLog;
pub use accounts::Account;
pub use api_keys::ApiKey;
pub use email_changes::EmailChange;
pub use email_sends::EmailSend;
pub use email_verifications::EmailVerification;
pub use files::FileRow;
pub use inbound_webhooks::InboundWebhook;
pub use invitations::Invitation;
pub use job_slices::JobSlice;
pub use jobs::Job;
pub use members::AccountMember;
pub use outputs::Output;
pub use password_resets::PasswordReset;
pub use provider_calls::ProviderCall;
pub use sessions::Session;
pub use sso::SsoConnection;
pub use user_audit_logs::UserAuditLog;
pub use user_avatar_files::UserAvatarFile;
pub use user_avatars::UserAvatar;
pub use users::User;

pub mod model_review_briefs;
pub mod model_reviews;
