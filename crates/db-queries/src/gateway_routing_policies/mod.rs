mod current;
mod insert;
mod lock_for_admission;
mod lock_for_update;

pub use current::{current, current_for_projects, ScopedPolicyRow};
pub use insert::insert;
pub use lock_for_admission::lock_for_admission;
pub use lock_for_update::lock_for_update;

#[derive(sqlx::FromRow)]
pub struct PolicyRow {
    pub revision: i32,
    pub policy: serde_json::Value,
    pub project_id: Option<String>,
}
