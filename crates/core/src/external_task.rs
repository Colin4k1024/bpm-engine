//! External Task domain: state and DTO for Worker protocol (fetch-and-lock / complete / fail).

use std::collections::HashMap;

/// Lifecycle state of an external task. See [`ExternalTaskStore`](bpm_engine_storage::ExternalTaskStore) for transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ExternalTaskState {
    /// Available for workers to fetch and lock.
    Ready,
    /// Held by a worker under a time-limited lease.
    Locked,
    /// Worker reported successful completion.
    Completed,
    /// All retries exhausted — requires manual intervention.
    Failed,
}

impl ExternalTaskState {
    /// Returns the state as a string constant for storage serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            ExternalTaskState::Ready => "READY",
            ExternalTaskState::Locked => "LOCKED",
            ExternalTaskState::Completed => "COMPLETED",
            ExternalTaskState::Failed => "FAILED",
        }
    }
}

/// Data transfer object for an external task (used by REST API and storage).
///
/// External tasks delegate work to stateless workers via a fetch-and-lock protocol.
/// The engine guarantees exactly one owner at a time through lease-based exclusivity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExternalTask {
    /// Unique task identifier.
    pub task_id: String,
    /// The token this task is associated with.
    pub token_id: String,
    /// The process instance containing this task.
    pub process_instance_id: String,
    /// Task type (topic) that workers subscribe to.
    pub task_type: String,
    /// Current lifecycle state.
    pub state: ExternalTaskState,
    /// Worker ID that currently holds the lock (if locked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_owner: Option<String>,
    /// ISO 8601 timestamp when the lock expires (if locked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_expire_at: Option<String>,
    /// Remaining retry count. Decremented on each failure.
    pub retries: i32,
    /// Error message from the last failed attempt (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Process variables passed to the worker.
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// ISO 8601 creation timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 last update timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}
