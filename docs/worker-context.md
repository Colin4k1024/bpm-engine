# Worker Execution Context (E3-2)

The Rust worker SDK (`bpm-engine-worker-sdk`) gives every task handler an
execution context — `TaskContext` — with cancellation, automatic lease
renewal, a timeout helper and structured failure, plus a per-process
idempotency guard. This page documents each helper, when to use it, and
what the ENGINE guarantees underneath (see `docs/recovery.md` §8 for the
lease/at-least-once semantics).

Reference tests: `crates/server/rest/tests/worker_context.rs` (REST-level,
memory driver, real workers — the authoritative acceptance suite for this
page) and the unit tests inside `crates/worker-sdk/src/`.

## 1. Cancellation (graceful shutdown)

Every worker run owns a `CancellationToken`
([`tokio_util::sync::CancellationToken`](https://docs.rs/tokio-util)).
`Worker::start` and `Worker::start_until_signal` wire **Ctrl-C / SIGTERM**
(and the legacy `AtomicBool` shutdown flag) to it; `Worker::spawn` hands
control to you:

```rust
let handle = worker.spawn();
// ... later ...
// stop polling, signal in-flight handlers, wait up to 5s, abort stragglers:
let drained = handle.shutdown(Duration::from_secs(5)).await;
```

Inside a handler, poll the context:

```rust
async fn handle(&self, task: ExternalTask, ctx: TaskContext) -> TaskResult {
    tokio::select! {
        _ = ctx.cancelled() => {
            // shut down promptly; returning a result still submits it
            TaskResult::Fail { error: "cancelled".into(), retry_after: None, category: None }
        }
        res = do_work(task) => res,
    }
}
```

API surface:

| Item | Purpose |
|------|---------|
| `ctx.cancellation()` | the worker-run token (clone it to `select!` many branches) |
| `ctx.cancelled().await` | resolves when the worker run is shutting down |
| `ctx.is_cancelled()` | cheap poll |
| `handle.cancel()` | fire-and-forget stop (no drain) |
| `handle.shutdown(grace).await` | stop + signal + drain (returns `true` when all in-flight handlers finished) |

Shutdown sequence:

1. stop polling (the loop is cancellation-aware even mid-fetch);
2. signal every in-flight handler via its `TaskContext`;
3. wait up to `grace` (`WorkerConfig::shutdown_grace`, default
   `lock_duration`) for handlers to finish AND their complete/fail to be
   submitted;
4. handlers that ignored cancellation past the grace are **aborted** —
   deliberately WITHOUT a `fail` call (a fail would consume a retry):
   their lease simply expires and the engine redelivers, exactly like a
   worker crash (§4 of `docs/recovery.md`).

Cancellation is **cooperative**: a handler that never polls
`ctx.cancelled()` is only ever stopped by the abort at step 4.

## 2. Auto lease renewal (long-running handlers)

The lease granted by `fetch_and_lock` lasts `lock_duration` (worker
config, default 30 s). A handler that runs longer must extend the lock or
risk the task being redelivered while it still works.

```rust
// Option A — scope renewal to a future (recommended):
let result = run_with_renewal(ctx, |ctx| async move {
    // ctx is handed back to the future
    long_work().await
}).await;

// Option B — manual guard:
let guard = ctx.spawn_auto_renewal(ctx.recommended_renewal_interval());
let out = long_work().await;
guard.stop(); // or just drop it
```

- **Interval**: `lock_duration / 3` (`ctx.recommended_renewal_interval()`),
  the documented recommendation: a single failed renewal still leaves time
  for the next tick before the lease actually expires. Each renewal
  extends the lease by a full `lock_duration`.
- **Renewal failures log a warning and continue.** The lease may have been
  reclaimed by another worker (at-least-once) or the task already
  completed/failed — the handler should still finish and return its
  result; the server **rejects stale completes with 4xx** (`lock_expired` /
  `invariant_violation`), so a post-reclaim complete is safely discarded.
- The renewal stops when the guard is dropped, when the worker run is
  cancelled, or when the handler future completes.
- Without renewal (control behavior): a handler that outlives the lease
  and then completes is rejected 4xx, and the task is redelivered to the
  next worker — verified by `late_complete_without_renewal_is_rejected`.

## 3. Timeout helper

```rust
use bpm_engine_worker_sdk::with_timeout;

match with_timeout(Duration::from_secs(30), call_model()).await {
    Ok(output) => { /* use it */ }
    Err(result) => result, // TaskResult::Fail, submit it directly
}
```

On elapse the helper returns `TaskResult::Fail` with the stable
`"<code>: <message>"` convention (`task_timeout: task timed out after
30s`) — readable in the engine's `error_message`, the
`ExternalTaskFailed` history payload and the DLQ. The category is left
`None`: a timeout is **transient** by default (the engine decrements
retries and requeues). A timeout is a policy boundary, not a lease
boundary — the lease may still be valid when it fires.

## 4. Structured failure: permanent vs transient

`TaskResult::Fail` carries an optional `category`
(`bpm_engine_core::RetryCategory`). On the wire this is the fail request's
optional `category` field (omitted when `None` — legacy workers are
byte-identical).

| Category | Engine behavior (E3-2, behavioral) | Use for |
|----------|------------------------------------|---------|
| `None` / `Some(Transient)` | retries as today: decrement retries → `READY` (redeliverable) → at 0 `FAILED` + DLQ + `TokenFailed` | network blips, rate limits, timeouts |
| `Some(Permanent)` | **skips the retry loop**: straight to `FAILED` (no retry decrement), DLQ entry, `TokenFailed` event — identical terminal path to retry exhaustion; **no redelivery** | bad input, policy refusal, unregistered tool |

```rust
TaskResult::Fail {
    error: "bad_input: amount is not a number".to_string(),
    retry_after: None,
    category: Some(RetryCategory::Permanent),
}
```

Notes:

- The category is ALSO recorded for observability: the persisted
  `error_message` gets the structured prefix
  `{"retry_category":"permanent"}: <error>` (E3-1 convention) which
  survives the task row, history, and the DLQ.
- The worker tool boundary already maps policy errors
  (`tool_not_allowed`, `tool_requires_approval`, `credential_missing`) to
  `Permanent` automatically (`ToolError` → `TaskResult` conversion);
  `tool_execution_failed` stays transient.
- Instance semantics are exactly retry exhaustion's: the token stays at
  the task node, the instance remains `RUNNING`, recovery is a DLQ
  requeue — a permanent failure never silently completes anything.
- REST clients send the same thing with the `category` field of
  `POST /external-tasks/:id/fail` (see `docs/openapi.yaml`).

## 5. Idempotency helpers (per process)

At-least-once redelivery means the same task may reach a worker twice.
`IdempotencyGuard` answers "already processed?" for the CURRENT worker
process:

```rust
use bpm_engine_worker_sdk::{derive_key, IdempotencyGuard};

let mut guard = IdempotencyGuard::new(); // per handler/worker
let key = derive_key(&task.task_id, &relevant_input);
if !guard.check_and_mark(&key) {
    return cached_or_complete_result; // already did the side effect
}
let result = do_side_effect(&relevant_input).await;
if result.is_err() { guard.unmark(&key); } // let the retry run
```

- `derive_key(task_id, input)` = `"<task_id>:sha256:<hex-of-input>"` —
  stable across redeliveries (same task id + same inputs), different per
  input.
- `check_and_mark` is check-and-set: two concurrent deliveries of the
  same key cannot both pass.
- The guard is **bounded** (default 10 000 keys, FIFO eviction of the
  oldest) — **per process only**: it does NOT survive restarts and is not
  shared between worker processes. An evicted or post-restart key is
  processed again — the guard is best-effort dedup, not a correctness
  boundary.
- **Durable alternative**: derive a stable key and check it against YOUR
  durable store before performing the side effect, recording it
  transactionally with the effect. The engine-side hook is the E3-1
  envelope's `idempotency_key` (see `docs/ai-task-envelope.md` §4): a
  deterministic hash over the task input + model + tool versions, carried
  in history for audit.

## 6. Related

- `docs/recovery.md` §8 — lease semantics, at-least-once, crash recovery.
- `docs/ai-task-envelope.md` — the E3-1 envelope (`retry_category`,
  `idempotency_key`, `timeout_secs` hints).
- `docs/sdk-rust.md` — the worker SDK overview.
- `docs/invariants.md` — external task invariants (single owner, monotonic
  retries).
