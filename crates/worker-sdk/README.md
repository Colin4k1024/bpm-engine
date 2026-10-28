# bpm-worker-sdk

Rust Worker SDK: pull-based client and worker runtime for external tasks.

## Role

- **EngineClient** — HTTP client for the engine (fetch-and-lock, complete, fail).
- **Worker** — poll loop that fetches tasks, calls your handler, and completes or fails tasks. `Worker::spawn()` returns a `WorkerHandle` with graceful `shutdown(grace)`.
- **TaskHandler** — trait to implement: `task_type()` and `handle(task, ctx) -> TaskResult`.
- **TaskContext** — per-task execution context: cancellation token, lock extension, auto lease renewal (E3-2).

No BPM knowledge required in worker code; workers only see task type and variables.

## Usage

```bash
cargo run -p bpm-engine-server-rest   # start the engine first
cargo run -p bpm-engine-worker-sdk --example payment
```

See [examples/payment.rs](examples/payment.rs) for a full example.

## Retry and backoff

The worker retries `fetch_and_lock` on transport or engine errors with exponential backoff (configurable via `WorkerConfig::fetch_retry_max` and `fetch_retry_backoff`). Default: 5 retries, 1s initial backoff, cap 30s.

## Execution context (E3-2)

- **Cancellation**: `Worker::start`/`start_until_signal` wire Ctrl-C/SIGTERM (and the shutdown flag) to a `CancellationToken`; poll `ctx.cancelled()` in long-running handlers to finish promptly during shutdown. `Worker::spawn()` + `handle.shutdown(grace)` stops polling, signals handlers and drains (aborted stragglers rely on lease expiry + redelivery).
- **Auto lease renewal**: `run_with_renewal(ctx, |ctx| async {...})` (or `ctx.spawn_auto_renewal(interval)`) extends the lock every `lock_duration / 3` while your handler runs — for handlers that outlive the lease.
- **Timeout**: `with_timeout(dur, fut)` maps elapse to `TaskResult::Fail` with the stable `task_timeout:` code.
- **Structured failure**: `TaskResult::Fail { category: Some(RetryCategory::Permanent) }` skips the engine's retry loop (straight to Failed + DLQ); `None`/`Transient` retries as before.
- **Idempotency**: `IdempotencyGuard` + `derive_key(task_id, input)` — per-process dedup (see the scope note in [docs/worker-context.md](../../docs/worker-context.md) for the durable alternative).

## Idempotency and graceful shutdown

- **Idempotency**: If the worker crashes after doing work but before calling `complete`, the engine will reclaim the lock and another worker may get the same task. Use an idempotency key (e.g. `task_id` or a business key from variables) and check "already processed?" before doing work. See [examples/idempotency.rs](examples/idempotency.rs).
- **Graceful shutdown**: Use `Worker::start_until_signal(shutdown: Arc<AtomicBool>)` and set the flag on SIGINT/SIGTERM — the worker stops polling, signals in-flight handlers via their context and drains them (up to `WorkerConfig::shutdown_grace`) before exiting. See [examples/graceful_shutdown.rs](examples/graceful_shutdown.rs).
- **Duplicate workers**: Run multiple workers (different `worker_id`); each task is locked by at most one worker. See [examples/duplicate_workers.rs](examples/duplicate_workers.rs).

## Documentation

See [docs/worker-context.md](../../docs/worker-context.md) (execution context), [docs/sdk-rust.md](../../docs/sdk-rust.md) and [docs/api-spec.md](../../docs/api-spec.md).
