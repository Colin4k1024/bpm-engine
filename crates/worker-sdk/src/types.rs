//! SDK-side types: ExternalTask (user view), TaskResult, TaskContext.

use std::collections::HashMap;
use std::time::Duration;

/// External task as seen by the worker (design §2).
/// Does not expose token_id / process_instance_id unless debug.
#[derive(Debug, Clone)]
pub struct ExternalTask {
    pub task_id: String,
    pub task_type: String,
    pub variables: HashMap<String, String>,
    pub lock_expire_at: Option<String>,
    pub retries: i32,
    #[cfg(debug_assertions)]
    pub token_id: Option<String>,
    #[cfg(debug_assertions)]
    pub process_instance_id: Option<String>,
}

/// Result of handling a task (design §2). Retries are managed by the Engine.
#[derive(Debug, Clone)]
pub enum TaskResult {
    Complete {
        variables: HashMap<String, String>,
    },
    Fail {
        error: String,
        retry_after: Option<Duration>,
    },
}
