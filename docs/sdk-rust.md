# Rust Worker SDK

The **Rust Worker SDK** lets you implement external-task workers that poll the engine, lock tasks, run your logic, and complete or fail tasks. No BPM knowledge is required in worker code.

---

## Crates and example

- **Crate:** [crates/worker-sdk](crates/worker-sdk)
- **Example:** [crates/worker-sdk/examples/payment.rs](crates/worker-sdk/examples/payment.rs)

---

## Quick start

1. Start the engine: `cargo run -p bpm-server-rest`
2. Deploy or use a process that has an ExternalTask node (e.g. `task_type = "payment"`).
3. Run the worker: `cargo run -p bpm-worker-sdk --example payment`

---

## Delivery and idempotency

- The **engine** guarantees **exactly-once token completion**: each token is advanced at most once.
- **Workers** receive tasks **at least once**: the same task may be fetched again after a crash or lease expiry. Handlers **must be idempotent** (e.g. keyed by business id or task id).

---

## Worker Responsibility Contract

- Tasks may be **redelivered** (same task_id / token after worker crash or lease expiry).
- Handlers **must be idempotent** (e.g. keyed by business id or task id; duplicate completion must be safe).
- Do **not** perform non-rollbackable side effects in the handler unless you implement your own deduplication or compensation (e.g. outbox or saga step).

---

## Core types

- **EngineClient** — HTTP client for the engine REST API (base URL, fetch-and-lock, complete, fail).
- **WorkerConfig** — Worker id, max tasks, lock duration, poll interval.
- **TaskHandler** — Trait you implement: `task_type()` and `handle(task, ctx) -> TaskResult`.
- **Worker** — Poll loop: fetches tasks, dispatches to your handler, completes or fails.

---

## Implementing a handler

Handler must be idempotent; the same task may be delivered more than once.

```rust
use async_trait::async_trait;
use bpm_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};
use std::collections::HashMap;

struct PaymentHandler;

#[async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str {
        "payment"
    }

    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        let amount = task.variables.get("amount").cloned().unwrap_or_else(|| "0".to_string());
        // ... your logic ...
        let mut variables = HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        TaskResult::Complete { variables }
    }
}
```

---

## Running the worker

```rust
let client = EngineClient::new("http://127.0.0.1:3000");
let config = WorkerConfig::new("payment-worker-1")
    .max_tasks(5)
    .lock_duration(Duration::from_secs(30))
    .poll_interval(Duration::from_secs(1));

let worker = Worker::builder()
    .client(client)
    .handler(PaymentHandler)
    .config(config)
    .build();

worker.start().await;
```

---

## Task result

- **TaskResult::Complete { variables }** — Complete the task and merge variables back into the process.
- **TaskResult::Fail { error, retry_after }** — Fail the task; engine may retry according to retries/timeout.

---

## Dependencies

Add to your `Cargo.toml`:

```toml
bpm-worker-sdk = { path = "../crates/worker-sdk" }
# or from the workspace root
```

You need `tokio` and `async-trait` for the async handler.

---

## See also

- [api-spec.md](api-spec.md) — REST API (external-tasks endpoints)
- [execution-model.md](execution-model.md) — Token claim and concurrency
- [architecture.md](architecture.md) — Engine overview
