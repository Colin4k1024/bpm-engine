//! SDK-side types: ExternalTask (user view), TaskResult, TaskContext.
//! Timeout helper: `with_timeout` (E3-2).

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use bpm_engine_core::RetryCategory;

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
        /// Structured retry category (E3-2): `Some(Permanent)` tells the
        /// engine to skip the retry loop — the task goes straight to
        /// `Failed` (DLQ + `TokenFailed`), like retry exhaustion.
        /// `None` (or `Some(Transient)`) keeps the legacy
        /// decrement-and-requeue behavior. On the wire this maps to the
        /// optional `category` field of the fail request (omitted when
        /// `None`), so legacy workers are byte-identical.
        category: Option<RetryCategory>,
    },
}

/// Stable error code prefix used by [`with_timeout`] failures.
pub const TASK_TIMEOUT_CODE: &str = "task_timeout";

/// Run `fut` under a deadline (E3-2 timeout helper).
///
/// Returns `Ok(output)` when the future completes within `duration`.
/// On elapse returns `Err(TaskResult::Fail)` with the stable
/// `"<code>: <message>"` convention shared with the E2-4 REST error
/// contract and the E3-1 worker tool boundary — the message is
/// `task_timeout: task timed out after <duration>` (readable in the
/// engine's `error_message`, the `ExternalTaskFailed` history payload and
/// the DLQ). The category is left `None`: a timeout is a *transient*
/// failure by default (the engine decrements retries and requeues, exactly
/// like a legacy `TaskResult::Fail`).
///
/// A timeout is a *policy* boundary, not a lease boundary: the worker's
/// lock may still be valid afterwards, so the returned `Fail` is still
/// submitted through the normal fail path. If the deadline you want is
/// "the lease expired", rely on the engine's lease redelivery instead.
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use bpm_engine_worker_sdk::types::with_timeout;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let fast = with_timeout(Duration::from_secs(5), async { 42 }).await;
/// assert_eq!(fast.expect("fast future completes"), 42);
///
/// let slow = with_timeout(Duration::from_millis(50), async {
///     tokio::time::sleep(Duration::from_secs(60)).await;
///     42
/// })
/// .await;
/// let result = slow.expect_err("slow future must time out");
/// match result {
///     bpm_engine_worker_sdk::TaskResult::Fail { error, .. } => {
///         assert!(error.starts_with("task_timeout: "), "{error}");
///     }
///     _ => panic!("expected a timeout failure"),
/// }
/// # }
/// ```
pub async fn with_timeout<F>(duration: Duration, fut: F) -> Result<F::Output, TaskResult>
where
    F: Future,
{
    match tokio::time::timeout(duration, fut).await {
        Ok(output) => Ok(output),
        Err(_) => Err(TaskResult::Fail {
            error: format!("{}: task timed out after {:?}", TASK_TIMEOUT_CODE, duration),
            retry_after: None,
            category: None,
        }),
    }
}
