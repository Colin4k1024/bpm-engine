//! TaskHandler trait and TaskContext (design §1). User implements TaskHandler; runtime passes TaskContext.
//!
//! E3-2 additions on [`TaskContext`]: cooperative cancellation
//! (`cancellation` / `cancelled` / `is_cancelled`), automatic lease renewal
//! ([`TaskContext::spawn_auto_renewal`] / [`run_with_renewal`]), and the
//! timeout helper [`with_timeout`] (re-exported from `types`).

use async_trait::async_trait;
use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::client::EngineClient;
use crate::types::{ExternalTask, TaskResult};

/// Default lease duration assumed when a `TaskContext` is built without a
/// worker config (30 s, the `WorkerConfig` default).
const DEFAULT_LOCK_DURATION: Duration = Duration::from_secs(30);

/// Context passed to the handler. Provides access to task metadata and
/// the ability to extend the lock for long-running tasks.
///
/// E3-2: the context also carries the worker-run [`CancellationToken`].
/// `Worker::start`, `Worker::start_until_signal` and `WorkerHandle::shutdown`
/// cancel it on Ctrl-C / SIGTERM / shutdown: the poll loop stops AND every
/// in-flight handler is signalled — poll [`TaskContext::cancelled`] inside
/// long-running handlers to finish promptly and submit your result before
/// the lease is lost.
#[derive(Clone)]
pub struct TaskContext {
    pub(crate) worker_id: String,
    pub(crate) task_id: String,
    pub(crate) client: EngineClient,
    pub(crate) cancellation: CancellationToken,
    pub(crate) lock_duration: Duration,
}

impl TaskContext {
    pub fn new(worker_id: String, task_id: String, client: EngineClient) -> Self {
        Self {
            worker_id,
            task_id,
            client,
            cancellation: CancellationToken::new(),
            lock_duration: DEFAULT_LOCK_DURATION,
        }
    }

    /// Build a context bound to a worker-run cancellation token and the
    /// worker's lock duration (used to size [`run_with_renewal`]).
    pub(crate) fn with_runtime(
        worker_id: String,
        task_id: String,
        client: EngineClient,
        cancellation: CancellationToken,
        lock_duration: Duration,
    ) -> Self {
        Self {
            worker_id,
            task_id,
            client,
            cancellation,
            lock_duration,
        }
    }

    /// The worker ID that owns this task's lock.
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// The task ID.
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// The lease duration this task was fetched with. Renewal helpers use
    /// it to size their intervals and extensions.
    pub fn lock_duration(&self) -> Duration {
        self.lock_duration
    }

    /// The worker-run cancellation token for this task.
    ///
    /// It is a *child* of the worker's token: cancelling the worker (or
    /// its shutdown handle) cancels every in-flight task's token. Cloning
    /// the returned token gives a cheap way to `select!` against it.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// Resolves when the worker run is being shut down (Ctrl-C, SIGTERM or
    /// `WorkerHandle::shutdown`). Poll this (or `select!` it against your
    /// work) inside long-running handlers to finish promptly.
    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }

    /// Whether the worker run has already been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Extend the lock on this task to prevent timeout during long processing.
    ///
    /// Call this periodically for tasks that may exceed the initial lock duration.
    pub async fn extend_lock(&self, extension: Duration) -> Result<(), crate::client::ClientError> {
        self.client
            .extend_lock(&self.task_id, &self.worker_id, extension)
            .await
    }

    /// Recommended auto-renewal interval: `lock_duration / 3`.
    ///
    /// Renewing at a third of the lease means a single failed renewal
    /// attempt still leaves time for the next one before the lease
    /// actually expires.
    pub fn recommended_renewal_interval(&self) -> Duration {
        self.lock_duration / 3
    }

    /// Spawn a background task that extends this task's lock every
    /// `interval` until the returned guard is dropped (E3-2 auto lease
    /// renewal). Each renewal extends the lease by a full
    /// [`TaskContext::lock_duration`].
    ///
    /// Recommended interval: [`TaskContext::recommended_renewal_interval`]
    /// (`lock_duration / 3`). Renewal failures are logged as warnings and
    /// do NOT abort the renewal loop and do NOT fail your handler — the
    /// lease may have been reclaimed by another worker (at-least-once
    /// redelivery); finish your work and let the engine's complete call
    /// decide (a stale lease is rejected with 4xx).
    ///
    /// The renewal stops ONLY when the guard is dropped (the handler
    /// finished or panicked) or when the worker-run cancellation fires:
    /// an `extend_lock` error does NOT stop the loop — the lease may
    /// still be valid (transient error) or already released (task
    /// completed/failed/reclaimed by another worker); the server-side
    /// lease guard is the authority, and a stale complete is rejected
    /// with 4xx. Keeping the guard alive after the handler finished
    /// merely wastes one HTTP call per interval.
    ///
    /// Prefer [`run_with_renewal`] which scopes the guard to a future.
    pub fn spawn_auto_renewal(&self, interval: Duration) -> LeaseRenewalGuard {
        let stop = CancellationToken::new();
        let loop_stop = stop.clone();
        let token = self.cancellation.child_token();
        let ctx = self.clone();
        // Each renewal extends the lease by a full lock duration, so the
        // lease is restored to its original strength on every tick.
        let extend_by = self.lock_duration;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = loop_stop.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {}
                }
                match ctx
                    .client
                    .extend_lock(&ctx.task_id, &ctx.worker_id, extend_by)
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        // The lease may have expired (reclaimed by another
                        // worker) or the task may already be completed/failed.
                        // Either way the handler should still complete/fail;
                        // the server rejects stale completes with 4xx.
                        tracing::warn!(
                            task_id = %ctx.task_id,
                            worker_id = %ctx.worker_id,
                            error = %e,
                            "auto lease renewal failed; lease may have been reclaimed"
                        );
                    }
                }
            }
        });
        LeaseRenewalGuard { stop }
    }
}

/// Guard returned by [`TaskContext::spawn_auto_renewal`]: dropping it stops
/// the background renewal task (the loop wakes on the stop token and exits;
/// its `JoinHandle` is detached).
pub struct LeaseRenewalGuard {
    stop: CancellationToken,
}

impl LeaseRenewalGuard {
    /// Stop the renewal loop without waiting for it to finish its current
    /// sleep. It complements `Drop` (which does the same).
    pub fn stop(&self) {
        self.stop.cancel();
    }
}

impl Drop for LeaseRenewalGuard {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Run a handler future under automatic lease renewal (E3-2 convenience).
///
/// ```text
/// let result = run_with_renewal(ctx, |ctx| async move {
///     // long work; the lease is renewed every lock_duration/3
///     ctx.cancelled().await; // still responds to worker shutdown
///     TaskResult::Complete { variables }
/// }).await;
/// ```
///
/// The renewal is spawned at [`TaskContext::recommended_renewal_interval`]
/// (`lock_duration / 3`), each renewal extending the lease by a full
/// lock duration, and is guaranteed to stop when the future completes (or
/// panics — the guard drops on unwind). The closure receives the context
/// back so the future can poll `ctx.cancelled()`, call `ctx.extend_lock`
/// manually, etc.
pub async fn run_with_renewal<C, F, T>(ctx: TaskContext, f: C) -> T
where
    C: FnOnce(TaskContext) -> F,
    F: Future<Output = T>,
{
    let guard = ctx.spawn_auto_renewal(ctx.recommended_renewal_interval());
    let out = f(ctx).await;
    guard.stop();
    drop(guard);
    out
}

/// User implements this to handle tasks (design §1).
#[async_trait]
pub trait TaskHandler: Send + Sync {
    fn task_type(&self) -> &str;

    async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::EngineClient;
    use std::time::Duration;

    fn test_ctx() -> TaskContext {
        TaskContext::new(
            "worker-1".to_string(),
            "task-1".to_string(),
            EngineClient::new("http://127.0.0.1:1"), // dead endpoint: fine
        )
    }

    #[tokio::test]
    async fn fresh_context_is_not_cancelled() {
        let ctx = test_ctx();
        assert!(!ctx.is_cancelled());
        assert_eq!(ctx.worker_id(), "worker-1");
        assert_eq!(ctx.task_id(), "task-1");
        assert_eq!(ctx.lock_duration(), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn cancelling_the_token_resolves_cancelled() {
        let ctx = test_ctx();
        // Cloning the exposed token cancels the context the handler sees.
        ctx.cancellation().child_token().cancel();
        ctx.cancellation().cancel();
        assert!(ctx.is_cancelled());
        // The async `cancelled()` future resolves promptly (no hang).
        tokio::time::timeout(Duration::from_secs(1), ctx.cancelled())
            .await
            .expect("cancelled() must resolve once the token is cancelled");
    }

    #[tokio::test]
    async fn child_tokens_cancel_with_the_parent() {
        // The worker run cancels ONE token; every task context spawned as
        // its child observes the cancellation.
        let parent = CancellationToken::new();
        let ctx = TaskContext::with_runtime(
            "worker-1".to_string(),
            "task-1".to_string(),
            EngineClient::new("http://127.0.0.1:1"),
            parent.child_token(),
            Duration::from_secs(9),
        );
        assert_eq!(ctx.lock_duration(), Duration::from_secs(9));
        assert_eq!(ctx.recommended_renewal_interval(), Duration::from_secs(3));
        parent.cancel();
        assert!(
            ctx.is_cancelled(),
            "child token must cancel with the parent"
        );
    }

    #[tokio::test]
    async fn run_with_renewal_returns_output_and_stops() {
        // Renewal against a dead endpoint just logs warnings; the wrapped
        // future's output is returned unchanged and the renewal stops when
        // the future completes (guard dropped).
        let ctx = TaskContext::with_runtime(
            "worker-1".to_string(),
            "task-1".to_string(),
            EngineClient::new("http://127.0.0.1:1"),
            CancellationToken::new(),
            Duration::from_secs(3),
        );
        let out = run_with_renewal(ctx, |ctx| async move {
            // The closure receives the context back.
            assert_eq!(ctx.task_id(), "task-1");
            tokio::time::sleep(Duration::from_millis(50)).await;
            "done".to_string()
        })
        .await;
        assert_eq!(out, "done");
    }

    #[tokio::test]
    async fn spawn_auto_renewal_guard_stops_on_drop() {
        let ctx = TaskContext::with_runtime(
            "worker-1".to_string(),
            "task-1".to_string(),
            EngineClient::new("http://127.0.0.1:1"),
            CancellationToken::new(),
            Duration::from_secs(30),
        );
        // Short interval: the loop ticks once against the dead endpoint
        // (warning only), then the guard stops it.
        let guard = ctx.spawn_auto_renewal(Duration::from_millis(20));
        tokio::time::sleep(Duration::from_millis(60)).await;
        guard.stop();
        drop(guard);
        // Nothing to observe directly (the loop is detached); this asserts
        // drop/stop never hang and the context stays usable.
        assert!(!ctx.is_cancelled());
    }
}
