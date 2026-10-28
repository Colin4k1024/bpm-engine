//! Worker runtime: poll loop and task spawning (design §3, §4).
//!
//! E3-2: the runtime is cancellation-aware. Every worker run owns a
//! [`CancellationToken`]; `start`/`start_until_signal` wire Ctrl-C/SIGTERM
//! (and the shutdown flag) to it, and [`Worker::spawn`] hands it to a
//! [`WorkerHandle`] whose `shutdown` cancels polling AND signals every
//! in-flight handler through [`crate::handler::TaskContext::cancelled`],
//! then waits (bounded) for them to finish. Handlers that ignore
//! cancellation past the grace period are aborted — their lease expires
//! and the engine redelivers (at-least-once), the same as a worker crash.
//! A fetch that resolves concurrently with the drain cannot leak a
//! handler past shutdown: `spawn_task` refuses to dispatch once the run
//! token is cancelled, and (belt and braces) any registration that raced
//! past the drain snapshot is aborted after the poll loop joins.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

use crate::client::EngineClient;
use crate::config::WorkerConfig;
use crate::handler::{TaskContext, TaskHandler};
use crate::types::{ExternalTask, TaskResult};

const BACKOFF_CAP_SECS: u64 = 30;

/// Worker: poll Engine for tasks and run handlers (design §3).
pub struct Worker {
    client: EngineClient,
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
    config: WorkerConfig,
}

/// One worker run: the poll loop state plus the cancellation/tracker
/// plumbing shared with the [`WorkerHandle`] (E3-2).
#[derive(Clone)]
struct WorkerRun {
    client: EngineClient,
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
    worker_id: String,
    max_tasks: usize,
    lock_duration: Duration,
    poll_interval: Duration,
    fetch_retry_max: usize,
    fetch_retry_backoff: Duration,
    shutdown_grace: Duration,
    /// Worker-run cancellation token: cancel to stop polling AND signal
    /// every in-flight task context.
    token: CancellationToken,
    /// Tracks the spawned task-completion wrappers (the outer tasks).
    tracker: TaskTracker,
    /// Join handles of the in-flight task-completion WRAPPERS (the
    /// `tracker.spawn` handles — NOT the inner handler handles), so
    /// shutdown can abort wrappers that ignored cancellation past the
    /// grace. Aborting a WRAPPER drops its [`AbortOnDrop`] guard, which
    /// aborts the inner HANDLER task. Finished handles are compacted
    /// every poll cycle.
    inflight: Arc<Mutex<Vec<InflightTask>>>,
    /// Monotonic sequence assigned to each registered wrapper (under the
    /// `inflight` lock, paired with the registration push).
    spawn_seq: Arc<AtomicUsize>,
    /// Snapshot of `spawn_seq` taken when the drain started
    /// ([`usize::MAX`] = no drain yet). Registrations with
    /// `seq >= drain_seq` happened after the drain snapshot — see
    /// [`WorkerRun::abort_late_registrations`].
    drain_seq: Arc<AtomicUsize>,
}

/// One registered in-flight task-completion wrapper: its registration
/// sequence (see [`WorkerRun::drain_seq`]) and its `tracker.spawn` handle.
struct InflightTask {
    seq: usize,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub struct WorkerBuilder {
    client: Option<EngineClient>,
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
    config: Option<WorkerConfig>,
}

impl WorkerBuilder {
    pub fn client(mut self, client: EngineClient) -> Self {
        self.client = Some(client);
        self
    }

    pub fn handler(mut self, h: impl TaskHandler + 'static) -> Self {
        let t = h.task_type().to_string();
        self.handlers.insert(t, Arc::new(h));
        self
    }

    pub fn config(mut self, config: WorkerConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn build(self) -> Worker {
        Worker {
            client: self.client.expect("EngineClient required"),
            handlers: self.handlers,
            config: self.config.expect("WorkerConfig required"),
        }
    }
}

/// Wait for the OS shutdown signal: Ctrl-C everywhere, SIGTERM on Unix.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "cannot install SIGTERM handler; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

impl Worker {
    pub fn builder() -> WorkerBuilder {
        WorkerBuilder::default()
    }

    /// Spawn the OS-shutdown-signal watcher that cancels this run's token.
    fn watch_signals(run: &WorkerRun) {
        let token = run.token.clone();
        tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            info!("shutdown signal received; cancelling worker run");
            token.cancel();
        });
    }

    fn new_run(&self) -> WorkerRun {
        let task_types: Vec<String> = self.handlers.keys().cloned().collect();
        if task_types.is_empty() {
            warn!("no handlers registered; worker will not fetch any tasks");
        }
        WorkerRun {
            client: self.client.clone(),
            handlers: self.handlers.clone(),
            worker_id: self.config.worker_id.clone(),
            max_tasks: self.config.max_tasks,
            lock_duration: self.config.lock_duration,
            poll_interval: self.config.poll_interval,
            fetch_retry_max: self.config.fetch_retry_max,
            fetch_retry_backoff: self.config.fetch_retry_backoff,
            shutdown_grace: self.config.shutdown_grace,
            token: CancellationToken::new(),
            tracker: TaskTracker::new(),
            inflight: Arc::new(Mutex::new(Vec::new())),
            spawn_seq: Arc::new(AtomicUsize::new(0)),
            drain_seq: Arc::new(AtomicUsize::new(usize::MAX)),
        }
    }

    /// Run the poll loop until cancelled (design §3).
    ///
    /// E3-2: Ctrl-C / SIGTERM cancel the run — the loop stops polling, all
    /// in-flight handlers are signalled via their task context, and the
    /// call returns after the graceful drain (see `WorkerConfig::shutdown_grace`).
    pub async fn start(&self) {
        let run = self.new_run();
        Self::watch_signals(&run);
        run.poll_loop().await;
        run.drain().await;
        info!(worker_id = %run.worker_id, "worker shut down");
    }

    /// Run the poll loop until `shutdown` is set to true (e.g. on SIGTERM),
    /// or the OS shutdown signal fires (E3-2: both cancel the run token).
    /// Finish the current poll cycle, signal running handlers, wait for
    /// them up to `shutdown_grace`, then exit.
    pub async fn start_until_signal(&self, shutdown: Arc<AtomicBool>) {
        let run = self.new_run();
        Self::watch_signals(&run);
        run.poll_loop_until(shutdown).await;
        run.drain().await;
        info!(worker_id = %run.worker_id, "worker shutting down");
    }

    /// Spawn the poll loop on the current runtime and return a handle
    /// (E3-2). The handle is the ergonomic shutdown path for embedded
    /// workers and tests:
    ///
    /// ```text
    /// let handle = worker.spawn();
    /// // ... later ...
    /// let drained = handle.shutdown(Duration::from_secs(5)).await;
    /// ```
    ///
    /// `shutdown(grace)` stops polling, signals in-flight handlers, waits
    /// up to `grace` for them to finish and aborts stragglers (their
    /// leases expire and the engine redelivers). Unlike
    /// `start`/`start_until_signal`, OS signals are NOT wired — the
    /// caller owns the lifecycle.
    pub fn spawn(&self) -> WorkerHandle {
        let run = self.new_run();
        let handle_run = run.clone();
        let join = tokio::spawn(async move { run.poll_loop().await });
        WorkerHandle {
            run: handle_run,
            join,
        }
    }
}

impl WorkerRun {
    /// The poll loop: fetch, dispatch, sleep — exiting promptly when the
    /// run token is cancelled.
    async fn poll_loop(&self) {
        self.poll_loop_inner(None).await;
    }

    /// Like [`WorkerRun::poll_loop`] but ALSO exits when the legacy
    /// `AtomicBool` flag is set (start_until_signal).
    async fn poll_loop_until(&self, shutdown: Arc<AtomicBool>) {
        self.poll_loop_inner(Some(shutdown)).await;
    }

    async fn poll_loop_inner(&self, shutdown: Option<Arc<AtomicBool>>) {
        let task_types: Vec<String> = self.handlers.keys().cloned().collect();
        let worker_id = self.worker_id.clone();
        loop {
            if self.token.is_cancelled()
                || shutdown.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
            {
                return;
            }
            // Compact finished wrapper handles (aborting a finished
            // JoinHandle is a no-op, but the list must not grow forever).
            self.inflight
                .lock()
                .unwrap()
                .retain(|t| !t.handle.is_finished());

            if task_types.is_empty() {
                tokio::select! {
                    _ = self.token.cancelled() => return,
                    _ = tokio::time::sleep(self.poll_interval) => {}
                }
                continue;
            }
            let mut backoff = self.fetch_retry_backoff;
            'fetch: for attempt in 0..=self.fetch_retry_max {
                if self.token.is_cancelled()
                    || shutdown.as_ref().is_some_and(|f| f.load(Ordering::Relaxed))
                {
                    return;
                }
                let fetch = self.client.fetch_and_lock(
                    &worker_id,
                    &task_types,
                    self.max_tasks,
                    self.lock_duration.as_millis() as u64,
                );
                let fetched = tokio::select! {
                    _ = self.token.cancelled() => return,
                    res = fetch => res,
                };
                match fetched {
                    Ok(tasks) => {
                        for task in tasks {
                            if self.handlers.contains_key(&task.task_type) {
                                self.spawn_task(task);
                            } else {
                                warn!(task_type = %task.task_type, "no handler for task type");
                            }
                        }
                        break 'fetch;
                    }
                    Err(e) => {
                        if attempt < self.fetch_retry_max {
                            let secs = backoff.as_secs().min(BACKOFF_CAP_SECS);
                            warn!(
                                attempt = attempt + 1,
                                backoff_secs = secs,
                                "fetch_and_lock failed, retrying"
                            );
                            tokio::select! {
                                _ = self.token.cancelled() => return,
                                _ = tokio::time::sleep(backoff) => {}
                            }
                            backoff = std::cmp::min(
                                backoff.saturating_mul(2),
                                Duration::from_secs(BACKOFF_CAP_SECS),
                            );
                        } else {
                            warn!(error = %e, "fetch_and_lock failed after retries");
                        }
                    }
                }
            }
            tokio::select! {
                _ = self.token.cancelled() => return,
                _ = tokio::time::sleep(self.poll_interval) => {}
            }
        }
    }

    /// Graceful drain (E3-2): stop polling (cancel), signal in-flight
    /// handlers, wait up to `shutdown_grace` for them to finish; abort
    /// handlers that ignored cancellation — their lease expires and the
    /// engine redelivers the task (at-least-once), like a worker crash.
    async fn drain(&self) -> bool {
        // Snapshot the registration sequence under the `inflight` lock
        // BEFORE cancelling: paired with the seq assignment + push in
        // `spawn_task` (same lock), every registration with
        // `seq < drain_seq` was registered before the drain began (the
        // tracker/wait below covers it), and every registration with
        // `seq >= drain_seq` raced past the drain — the shutdown-vs-fetch
        // race; `abort_late_registrations` cleans those up.
        let drain_seq = {
            let _guard = self.inflight.lock().unwrap();
            self.spawn_seq.load(Ordering::SeqCst)
        };
        self.drain_seq.store(drain_seq, Ordering::SeqCst);
        self.token.cancel();
        self.tracker.close();
        if tokio::time::timeout(self.shutdown_grace, self.tracker.wait())
            .await
            .is_ok()
        {
            return true;
        }
        self.abort_stragglers();
        false
    }

    /// Abort every still-running in-flight task wrapper. Called only after
    /// the drain grace expired: the handlers ignored cancellation, so the
    /// worker gives up on them — no `fail` is submitted (that would consume
    /// a retry); the lease simply expires and the engine redelivers.
    fn abort_stragglers(&self) {
        let stragglers: Vec<InflightTask> = {
            let mut guard = self.inflight.lock().unwrap();
            guard.drain(..).collect()
        };
        let n = stragglers.len();
        for t in stragglers {
            t.handle.abort();
        }
        if n > 0 {
            warn!(
                worker_id = %self.worker_id,
                aborted = n,
                grace = ?self.shutdown_grace,
                "aborted handlers that ignored cancellation; their leases will expire and the engine will redeliver"
            );
        }
    }

    /// Abort in-flight wrappers registered AFTER the drain snapshot (belt
    /// and braces for the shutdown-vs-fetch race): if a fetch was in
    /// flight when [`WorkerRun::drain`] cancelled the token, the drain may
    /// have already decided the worker was idle (tracker closed + empty,
    /// `wait` returned) — a wrapper registered past that point is tracked
    /// by nobody and would leak past shutdown. Called from
    /// [`WorkerHandle::shutdown`] after the poll loop is joined (or
    /// aborted), so no further registration can race this scan.
    /// Pre-snapshot registrations are untouched: the drain's `wait` (or
    /// `abort_stragglers`) already handled them, and aborting a finished
    /// handle is a no-op anyway.
    fn abort_late_registrations(&self) {
        let drain_seq = self.drain_seq.load(Ordering::SeqCst);
        if drain_seq == usize::MAX {
            return; // never drained: nothing to do
        }
        let all: Vec<InflightTask> = {
            let mut guard = self.inflight.lock().unwrap();
            guard.drain(..).collect()
        };
        // Late = registered after the snapshot AND still unfinished.
        let late: Vec<InflightTask> = all
            .into_iter()
            .filter(|t| t.seq >= drain_seq && !t.handle.is_finished())
            .collect();
        let n = late.len();
        for t in late {
            t.handle.abort();
        }
        if n > 0 {
            warn!(
                worker_id = %self.worker_id,
                aborted = n,
                "aborted task(s) dispatched after the drain snapshot (fetch resolved during shutdown); their leases will expire and the engine will redeliver"
            );
        }
    }

    /// Spawn one task; on handler panic, call fail (design §4, §6).
    ///
    /// Structure (E3-2): the HANDLER runs in its own `tokio::spawn` (panic
    /// isolation — a panic surfaces as a `JoinError`, never unwinds into
    /// the runtime); the completion wrapper is tracked by `tracker` (so
    /// shutdown can wait for results) and registered in `inflight` (so it
    /// can be aborted on drain timeout). Aborting the wrapper drops its
    /// [`AbortOnDrop`] guard, which aborts the handler task too.
    fn spawn_task(&self, task: ExternalTask) {
        // Shutdown-vs-fetch race guard: if the run token was cancelled
        // while our fetch was in flight, the drain may already have
        // decided the worker was idle (tracker closed + empty, wait
        // returned). Dispatching now would leak a handler past shutdown —
        // nobody waits for or aborts it. Instead drop the task on the
        // floor: NO `fail` is submitted (it would consume a retry); the
        // lease expires and the engine redelivers (at-least-once), exactly
        // like a worker crash. (Belt and braces: if the cancellation lands
        // between this check and the registration below, the task is
        // classified by `drain_seq` and aborted by
        // `abort_late_registrations` at the end of `shutdown`.)
        if self.token.is_cancelled() {
            warn!(
                task_id = %task.task_id,
                task_type = %task.task_type,
                worker_id = %self.worker_id,
                "fetch resolved during shutdown; task not dispatched — its lease will expire and the engine will redeliver"
            );
            return;
        }
        let handler = match self.handlers.get(&task.task_type) {
            Some(h) => Arc::clone(h),
            None => return,
        };
        let client = self.client.clone();
        let worker_id = self.worker_id.clone();
        let task_id = task.task_id.clone();
        let ctx = TaskContext::with_runtime(
            worker_id.clone(),
            task.task_id.clone(),
            client.clone(),
            self.token.child_token(),
            self.lock_duration,
        );

        let task_type = task.task_type.clone();
        let inner = tokio::spawn(async move { handler.handle(task, ctx).await });
        let guard = AbortOnDrop { inner };
        let wrapper = self.tracker.spawn(async move {
            let start = Instant::now();
            let duration_ms = move || start.elapsed().as_millis();
            match guard.await {
                Ok(TaskResult::Complete { variables }) => {
                    info!(
                        task_id = %task_id,
                        task_type = %task_type,
                        worker_id = %worker_id,
                        duration_ms = duration_ms(),
                        result = "Complete",
                        "task finished"
                    );
                    if let Err(e) = client.complete(&task_id, &worker_id, variables).await {
                        tracing::warn!(task_id = %task_id, error = %e, "complete failed");
                    }
                }
                Ok(TaskResult::Fail {
                    error,
                    retry_after,
                    category,
                }) => {
                    info!(
                        task_id = %task_id,
                        task_type = %task_type,
                        worker_id = %worker_id,
                        duration_ms = duration_ms(),
                        result = "Fail",
                        error = %error,
                        "task failed"
                    );
                    if let Err(e) = client
                        .fail(&task_id, &worker_id, error, retry_after, category)
                        .await
                    {
                        tracing::warn!(task_id = %task_id, error = %e, "fail failed");
                    }
                }
                Err(join_err) => {
                    if join_err.is_panic() {
                        info!(
                            task_id = %task_id,
                            task_type = %task_type,
                            worker_id = %worker_id,
                            duration_ms = duration_ms(),
                            result = "Panic",
                            error = "handler panic",
                            "task panic"
                        );
                        let _ = client
                            .fail(
                                &task_id,
                                &worker_id,
                                "handler panic".to_string(),
                                None,
                                None,
                            )
                            .await;
                    } else {
                        // Cancelled join = the handler task was aborted by
                        // our own drain timeout (via AbortOnDrop). Do NOT
                        // fail: a fail would consume a retry; the lease
                        // expiring and the task being redelivered is the
                        // intended crash semantics.
                        warn!(
                            task_id = %task_id,
                            task_type = %task_type,
                            worker_id = %worker_id,
                            "handler aborted during shutdown; lease will expire and the task will be redelivered"
                        );
                    }
                }
            }
        });
        // The wrapper is never awaited here: dropping its JoinHandle just
        // detaches it (tokio semantics); `inflight` keeps the handle so
        // the drain timeout can abort it (and thereby the handler). The
        // seq assignment and the push share the `inflight` lock so that
        // the drain snapshot (same lock) classifies this registration
        // unambiguously as before/after the drain.
        {
            let mut guard = self.inflight.lock().unwrap();
            let seq = self.spawn_seq.fetch_add(1, Ordering::SeqCst);
            guard.push(InflightTask {
                seq,
                handle: wrapper,
            });
        }
    }
}

/// Awaits the wrapped handler task; aborts it when dropped.
///
/// The completion wrapper awaits this guard instead of the raw
/// `JoinHandle`: aborting the WRAPPER (drain timeout) drops the guard,
/// which aborts the HANDLER — otherwise an aborted wrapper would leave the
/// handler running detached. On normal completion the handler task is
/// already finished, and aborting a finished handle is a no-op.
struct AbortOnDrop {
    inner: tokio::task::JoinHandle<TaskResult>,
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.inner.abort();
    }
}

impl std::future::Future for AbortOnDrop {
    type Output = Result<TaskResult, tokio::task::JoinError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // JoinHandle is Unpin; forward the poll without moving the field
        // out of the (Drop-implementing) guard.
        std::pin::Pin::new(&mut self.inner).poll(cx)
    }
}

/// Handle to a spawned worker run (E3-2): cancel or gracefully shut down
/// the poll loop and its in-flight tasks.
pub struct WorkerHandle {
    run: WorkerRun,
    join: tokio::task::JoinHandle<()>,
}

impl WorkerHandle {
    /// The worker-run cancellation token: cancelling it stops polling and
    /// signals every in-flight task context. Usually you want
    /// [`WorkerHandle::shutdown`] instead (it also waits for the drain).
    pub fn cancellation(&self) -> CancellationToken {
        self.run.token.clone()
    }

    /// Stop the worker now without waiting (fire-and-forget cancellation).
    pub fn cancel(&self) {
        self.run.token.cancel();
    }

    /// Graceful shutdown: stop polling, signal in-flight handlers, wait up
    /// to `grace` for them to finish and to submit their results, then
    /// abort stragglers (their leases expire and the engine redelivers —
    /// at-least-once). Also waits (bounded by the same `grace`) for the
    /// poll-loop task to exit.
    ///
    /// Returns `true` when every in-flight handler completed within
    /// `grace`; `false` when handlers had to be aborted.
    ///
    /// Unlike `Worker::start`, the spawned loop wires NO OS signal — the
    /// caller owns the lifecycle and calls this (or [`WorkerHandle::cancel`]).
    pub async fn shutdown(mut self, grace: Duration) -> bool {
        self.run.shutdown_grace = grace;
        let drained = self.run.drain().await;
        // The loop exits promptly on cancellation (every await in it is
        // cancellation-aware); bound the join by the same grace so a hung
        // fetch cannot stall the caller.
        let _ = tokio::time::timeout(grace, &mut self.join).await;
        // The poll loop is now definitely not running: either it exited on
        // cancellation, or it was hung (e.g. a stuck fetch) and we just
        // aborted it. Aborting a finished handle is a no-op; aborting
        // mid-`spawn_task` is safe — the `AbortOnDrop` guard on its stack
        // aborts any half-spawned handler.
        self.join.abort();
        // Belt and braces (shutdown-vs-fetch race): a fetch that resolved
        // around the drain may have registered a wrapper AFTER the drain
        // snapshot — the drain's `wait` never observed it. Now that the
        // loop is dead (no further registration possible), abort
        // everything registered after the snapshot.
        self.run.abort_late_registrations();
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::EngineClient;
    use crate::config::WorkerConfig;
    use crate::types::ExternalTask;
    use std::collections::HashMap;

    /// A handler that cooperatively observes cancellation: it would "work"
    /// for `work_ms`, but returns as soon as `ctx.cancelled()` resolves.
    struct CancelAwareHandler {
        work: std::sync::Arc<std::sync::atomic::AtomicU64>,
    }

    #[async_trait::async_trait]
    impl TaskHandler for CancelAwareHandler {
        fn task_type(&self) -> &str {
            "cancel-work"
        }

        async fn handle(&self, _task: ExternalTask, ctx: TaskContext) -> TaskResult {
            let work = tokio::time::sleep(Duration::from_secs(300));
            tokio::select! {
                _ = ctx.cancelled() => {
                    self.work.store(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                        Ordering::SeqCst,
                    );
                    TaskResult::Fail {
                        error: "worker_shutdown: cancelled".to_string(),
                        retry_after: None,
                        category: None,
                    }
                }
                _ = work => TaskResult::Complete {
                    variables: HashMap::new(),
                },
            }
        }
    }

    fn worker_for(base_url: &str) -> Worker {
        Worker::builder()
            .client(EngineClient::new(base_url))
            .handler(CancelAwareHandler {
                work: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            })
            .config(
                WorkerConfig::new("w-test")
                    .poll_interval(Duration::from_millis(20))
                    .fetch_retry_max(0),
            )
            .build()
    }

    #[tokio::test]
    async fn spawn_and_shutdown_exits_promptly_without_tasks() {
        // No server, no tasks: shutdown must return quickly (the loop is
        // cancellation-aware even mid-fetch-retry).
        let handle = worker_for("http://127.0.0.1:1").spawn();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let drained = tokio::time::timeout(
            Duration::from_secs(2),
            handle.shutdown(Duration::from_millis(500)),
        )
        .await
        .expect("shutdown must not hang");
        assert!(drained, "no in-flight tasks: drain must succeed");
    }

    #[tokio::test]
    async fn handle_cancel_signals_loop() {
        let handle = worker_for("http://127.0.0.1:1").spawn();
        handle.cancel();
        // The loop observes the token and exits; nothing hangs.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(handle.cancellation().is_cancelled());
        // Dropping without shutdown is fine (tasks detach).
    }

    // ---- drain-vs-fetch race regression tests (finding: shutdown could
    // leak a handler dispatched by a fetch that resolved after drain
    // decided the worker was idle) ----
    //
    // Coverage note: the full interleaving (a real fetch resolving
    // concurrently with drain) is not deterministically schedulable here,
    // so the two windows are pinned separately:
    // 1. `spawn_task_after_cancellation_dispatches_nothing` — the
    //    spawn-time guard (fetch resolved, token already cancelled).
    // 2. `abort_late_registrations_aborts_post_drain_leak` — the
    //    check-to-registration window INSIDE spawn_task (token cancelled
    //    between the guard check and the registration), exercised by
    //    injecting the raced registration directly.

    /// Records invocation, then sleeps "forever": if the guard leaks, the
    /// wrapper stays unfinished (tracker.wait hangs) and `ran` flips.
    struct RecordingHandler {
        ran: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TaskHandler for RecordingHandler {
        fn task_type(&self) -> &str {
            "record-work"
        }

        async fn handle(&self, _task: ExternalTask, _ctx: TaskContext) -> TaskResult {
            self.ran.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(300)).await;
            TaskResult::Complete {
                variables: HashMap::new(),
            }
        }
    }

    /// A minimal locked task for the race tests.
    fn external_task(task_id: &str, task_type: &str) -> ExternalTask {
        ExternalTask {
            task_id: task_id.to_string(),
            task_type: task_type.to_string(),
            variables: HashMap::new(),
            lock_expire_at: None,
            retries: 1,
            #[cfg(debug_assertions)]
            token_id: Some("tok".to_string()),
            #[cfg(debug_assertions)]
            process_instance_id: Some("inst".to_string()),
        }
    }

    #[tokio::test]
    async fn spawn_task_after_cancellation_dispatches_nothing() {
        // The guard: drain() cancelled the token while our fetch was in
        // flight (zero tracked tasks → drain returned immediately); the
        // fetch then resolved Ok and poll_loop called spawn_task. Nothing
        // may be dispatched: no wrapper registered, nothing tracked (a
        // tracked-but-unfinished wrapper would hang tracker.wait), and
        // the handler never runs.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = Worker::builder()
            .client(EngineClient::new("http://127.0.0.1:1"))
            .handler(RecordingHandler {
                ran: Arc::clone(&ran),
            })
            .config(WorkerConfig::new("w-race"))
            .build();
        let run = worker.new_run();

        run.token.cancel();
        run.spawn_task(external_task("t-race", "record-work"));

        assert!(
            run.inflight.lock().unwrap().is_empty(),
            "no wrapper may be registered after cancellation"
        );
        run.tracker.close();
        tokio::time::timeout(Duration::from_millis(200), run.tracker.wait())
            .await
            .expect("nothing may be tracked after cancellation (wait returns at once)");
        assert!(
            !ran.load(Ordering::SeqCst),
            "the handler must never run after cancellation"
        );
    }

    #[tokio::test]
    async fn abort_late_registrations_aborts_post_drain_leak() {
        // Belt and braces: the guard cannot see a cancellation landing
        // between its check and the registration inside spawn_task. That
        // raced wrapper (seq >= drain_seq) is aborted by
        // abort_late_registrations once the poll loop is dead, while
        // pre-drain registrations are left to the drain itself. Both
        // registrations are injected directly — deterministically, no
        // HTTP in the critical path; the real spawn_task path is covered
        // by the guard test above.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = Worker::builder()
            .client(EngineClient::new("http://127.0.0.1:1"))
            .handler(RecordingHandler {
                ran: Arc::clone(&ran),
            })
            .config(WorkerConfig::new("w-race"))
            .build();
        let run = worker.new_run();

        // Pre-drain registration: a wrapper that finishes immediately
        // (like every task the drain legitimately waited for).
        let pre_handle = run.tracker.spawn(async {});
        {
            let mut guard = run.inflight.lock().unwrap();
            let seq = run.spawn_seq.fetch_add(1, Ordering::SeqCst);
            guard.push(InflightTask {
                seq,
                handle: pre_handle,
            });
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            while !run
                .inflight
                .lock()
                .unwrap()
                .iter()
                .all(|t| t.handle.is_finished())
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("pre-drain wrapper must finish immediately");

        // Drain with an empty (closed) tracker: returns true at once and
        // snapshots drain_seq — exactly the "worker looked idle" moment.
        assert!(run.drain().await, "nothing unfinished: drain succeeds");

        // Inject the raced registration (what spawn_task would produce if
        // the cancellation landed inside its check-to-registration
        // window; the guard itself refuses the post-cancel path now).
        let late_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let started = Arc::clone(&late_started);
            let handle = run.tracker.spawn(async move {
                started.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_secs(300)).await;
            });
            let mut guard = run.inflight.lock().unwrap();
            let seq = run.spawn_seq.fetch_add(1, Ordering::SeqCst);
            guard.push(InflightTask { seq, handle });
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            late_started.load(Ordering::SeqCst),
            "the late wrapper must be running when the re-scan fires"
        );

        run.abort_late_registrations();

        // Positive evidence of the abort: the late wrapper is TRACKED and
        // the tracker is closed; tracker.wait only completes once every
        // tracked task has exited — the 300 s sleeper must have exited.
        tokio::time::timeout(Duration::from_millis(200), run.tracker.wait())
            .await
            .expect("the late registration must be aborted (tracked task must exit)");
        assert!(
            run.inflight.lock().unwrap().is_empty(),
            "the re-scan clears the inflight list"
        );
    }
}
