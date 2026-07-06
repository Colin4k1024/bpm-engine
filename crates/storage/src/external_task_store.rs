//! External Task store trait (plan §12): fetch-and-lock, complete, fail, reclaim, create.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::collections::HashMap;
//! use std::time::Duration;
//! use bpm_engine_core::ExternalTaskState;
//! use bpm_engine_storage::ExternalTaskStore;
//!
//! # async fn example(repo: Arc<impl ExternalTaskStore>) -> anyhow::Result<()> {
//! // Create an external task
//! let task_id = repo
//!     .create("token-1", "instance-1", "payment", 3, 60, HashMap::new())
//!     .await?;
//!
//! // Worker fetches and locks the task
//! let tasks = repo
//!     .fetch_and_lock("worker-1", &["payment".into()], 10, Duration::from_secs(30))
//!     .await?;
//! assert_eq!(tasks.len(), 1);
//! assert_eq!(tasks[0].state, ExternalTaskState::Locked);
//!
//! // Worker completes the task
//! repo.complete(&task_id, "worker-1", HashMap::new()).await?;
//! let task = repo.get(&task_id).await?.unwrap();
//! assert_eq!(task.state, ExternalTaskState::Completed);
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use bpm_engine_core::ExternalTask;
use std::collections::HashMap;
use std::time::Duration;

/// Typed errors for external task operations.
///
/// These errors are safe to expose to API clients without leaking internal details.
#[derive(Debug, thiserror::Error)]
pub enum ExternalTaskError {
    /// The specified task was not found.
    #[error("task not found: {task_id}")]
    TaskNotFound {
        /// The task ID that was not found.
        task_id: String,
    },
    /// The task is not in the required state for the operation.
    #[error("task not in locked state: {task_id}")]
    TaskNotLocked {
        /// The task ID with invalid state.
        task_id: String,
    },
    /// The lock on the task has expired.
    #[error("lock expired for task: {task_id}")]
    LockExpired {
        /// The task ID whose lock expired.
        task_id: String,
    },
    /// An invariant violation occurred (e.g., lease conflict).
    #[error("invariant violation: {0}")]
    InvariantViolation(#[from] crate::InvariantViolation),
    /// An unexpected internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}

/// External task store: manages the lifecycle of long-running work delegated to external workers.
///
/// External tasks allow the BPM engine to delegate work to external systems while
/// maintaining lease-based exclusivity (only one worker owns a task at a time).
///
/// # State machine
///
/// ```text
///  READY ──fetch_and_lock──▶ LOCKED ──complete──▶ COMPLETED
///    ▲                        │
///    │                        │
///    └──fail (retries > 0)────┘
///    │
///    └──fail (retries = 0)────▶ FAILED
/// ```
///
/// # Concurrency model
///
/// [`fetch_and_lock`] is atomic: it reclaims expired locks and selects READY tasks
/// in a single write transaction, eliminating TOCTOU races between workers.
#[async_trait]
pub trait ExternalTaskStore: Send + Sync {
    /// Create a READY external task when token arrives at ExternalTask node.
    async fn create(
        &self,
        token_id: &str,
        process_instance_id: &str,
        task_type: &str,
        retries: i32,
        timeout_secs: u64,
        variables: HashMap<String, String>,
    ) -> anyhow::Result<String>;

    /// Fetch READY tasks matching task_types, lock to worker, return locked tasks.
    ///
    /// Atomically: (1) reclaims any expired locks, (2) selects oldest READY tasks,
    /// (3) locks them to the requesting worker. Multiple concurrent callers each
    /// see a consistent snapshot with no race on task assignment.
    async fn fetch_and_lock(
        &self,
        worker_id: &str,
        task_types: &[String],
        max_tasks: usize,
        lock_duration: Duration,
    ) -> anyhow::Result<Vec<ExternalTask>>;

    /// Complete: LOCKED + lock_owner + not expired -> COMPLETED; merge variables.
    async fn complete(
        &self,
        task_id: &str,
        worker_id: &str,
        variables: HashMap<String, String>,
    ) -> Result<(), ExternalTaskError>;

    /// Fail: retries -= 1; if retries > 0 -> READY else -> FAILED.
    async fn fail(
        &self,
        task_id: &str,
        worker_id: &str,
        error: String,
        retry_after: Option<Duration>,
    ) -> Result<(), ExternalTaskError>;

    /// Reclaim LOCKED tasks whose lock_expire_at < now to READY (plan §9).
    async fn reclaim_expired_locks(&self) -> anyhow::Result<usize>;

    /// Extend the lock on a LOCKED task. Returns false if task not found, not locked,
    /// or owned by a different worker.
    async fn extend_lock(
        &self,
        task_id: &str,
        worker_id: &str,
        extension: Duration,
    ) -> anyhow::Result<bool>;

    /// Load task by id (for REST layer after complete to get token_id/instance_id for transition).
    async fn get(&self, task_id: &str) -> anyhow::Result<Option<ExternalTask>>;
}
