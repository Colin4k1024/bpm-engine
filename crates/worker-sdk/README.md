# bpm-worker-sdk

Rust Worker SDK: pull-based client and worker runtime for external tasks.

## Role

- **EngineClient** — HTTP client for the engine (fetch-and-lock, complete, fail).
- **Worker** — poll loop that fetches tasks, calls your handler, and completes or fails tasks.
- **TaskHandler** — trait to implement: `task_type()` and `handle(task, ctx) -> TaskResult`.

No BPM knowledge required in worker code; workers only see task type and variables.

## Usage

```bash
cargo run -p bpm-engine-server-rest   # start the engine first
cargo run -p bpm-engine-worker-sdk --example payment
```

See [examples/payment.rs](examples/payment.rs) for a full example.

## Retry and backoff

The worker retries `fetch_and_lock` on transport or engine errors with exponential backoff (configurable via `WorkerConfig::fetch_retry_max` and `fetch_retry_backoff`). Default: 5 retries, 1s initial backoff, cap 30s.

## Idempotency and graceful shutdown

- **Idempotency**: If the worker crashes after doing work but before calling `complete`, the engine will reclaim the lock and another worker may get the same task. Use an idempotency key (e.g. `task_id` or a business key from variables) and check "already processed?" before doing work. See [examples/idempotency.rs](examples/idempotency.rs).
- **Graceful shutdown**: Use `Worker::start_until_signal(shutdown: Arc<AtomicBool>)` and set the flag on SIGINT/SIGTERM; the worker finishes the current poll cycle and exits. See [examples/graceful_shutdown.rs](examples/graceful_shutdown.rs).
- **Duplicate workers**: Run multiple workers (different `worker_id`); each task is locked by at most one worker. See [examples/duplicate_workers.rs](examples/duplicate_workers.rs).

## Documentation

See [docs/sdk-rust.md](../../docs/sdk-rust.md) and [docs/api-spec.md](../../docs/api-spec.md).
