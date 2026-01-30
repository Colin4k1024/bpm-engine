//! External Task domain: state and DTO for Worker protocol (fetch-and-lock / complete / fail).

use std::collections::HashMap;

/// External task lifecycle (plan §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExternalTaskState {
    Ready,
    Locked,
    Completed,
    Failed,
}

impl ExternalTaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExternalTaskState::Ready => "READY",
            ExternalTaskState::Locked => "LOCKED",
            ExternalTaskState::Completed => "COMPLETED",
            ExternalTaskState::Failed => "FAILED",
        }
    }
}

/// External task DTO for API and store (plan §5).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExternalTask {
    pub task_id: String,
    pub token_id: String,
    pub process_instance_id: String,
    pub task_type: String,
    pub state: ExternalTaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_expire_at: Option<String>,
    pub retries: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}
