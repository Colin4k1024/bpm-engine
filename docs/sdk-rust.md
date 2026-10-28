---
layout: default
---

# Rust SDK

The **Rust SDK** covers both sides of the engine: the **typed process builder** for embedding the engine and defining processes in Rust, and the **Rust Worker SDK** for implementing external-task workers that poll the engine, lock tasks, run your logic, and complete or fail tasks.

---

## Typed Process Builder (Rust developer API)

For embedded Rust developers, `ProcessBuilder` is the ergonomic entry point: you build a process as typed Rust code instead of hand-writing BPMN 2.0 XML or DSL JSON strings.

```rust
use bpm_engine::bpm_engine_bpmn::builder::ProcessBuilder;

let def = ProcessBuilder::new("order")
    .start_node("start")
    .external_task("payment", "payment-topic")
        .retries(5)
        .timeout_secs(120)
    .exclusive_gateway("decide")
    .end_node("end-paid")
    .end_node("end-skipped")
    .edge("start", "payment")
    .edge("payment", "decide")
    .edge("decide", "end-paid").when_variable_eq("paid", "true")
    .edge("decide", "end-skipped").is_default()
    .build()?;
```

Runnable example: `cargo run --example builder_process` (builds the flow above, deploys it to memory stores, runs it to completion and prints the outcome).

### One compiler, three entries

The builder follows ADR-006: it assembles the same intermediate model the BPMN XML parser produces and hands it to the shared `bpm_engine_bpmn::compile` **verbatim**. There is no separate validation path.

| Entry | Audience | Format |
|-------|----------|--------|
| `bpm_engine_bpmn::builder::ProcessBuilder` | Rust developers | Typed Rust code |
| `bpm_engine_dsl::compile(&str)` | AI agents / generators | JSON DSL (schema: `bpm_engine_dsl::schema_json()`) |
| `bpm_engine_bpmn::parse_and_compile(&str)` | BPMN interop / existing models | BPMN 2.0 XML |

All three compile to the same `ProcessDefinition` (enforced by the three-way parity test `tests/builder_parity.rs`) and execute identically (`tests/builder_execution.rs`).

### Node type coverage

| Builder method | Compiles to | Options (chained) |
|----------------|-------------|-------------------|
| `.start_node(id)` | `NodeType::Start` | `.name()` |
| `.end_node(id)` | `NodeType::End` | `.name()` |
| `.external_task(id, task_type)` | `NodeType::ExternalTask` | `.retries(n)`, `.timeout_secs(n)`, `.name()` |
| `.user_task(id)` | `NodeType::UserTask` | `.form_key(k)`, `.form_field(FormFieldSpec)`, `.name()` |
| `.timer_catch(id, duration)` | `NodeType::TimerIntermediateCatch` | `.name()` |
| `.exclusive_gateway(id)` | `NodeType::ExclusiveGateway` | `.name()` |
| `.parallel_gateway(id)` | `NodeType::ParallelFork` / `ParallelJoin` | `.name()` |
| `.edge(source, target)` | sequence flow | `.when_variable_eq(var, value)`, `.when_expression(expr)`, `.is_default()` |

Out of scope (use BPMN XML for these): boundary events, subprocesses, call activities, message/signal events, terminate ends.

### Defaults (identical to the BPMN parser and the DSL)

- `external_task`: `retries = 3`, `timeout_secs = 60`.
- Form fields: `label` defaults to the field `id`, `type` defaults to `string`, `required` defaults to `false`, empty `options` become `None`.
- `.when_variable_eq(var, value)` lowers to the canonical expression `${var == "value"}` (the same lowering as the DSL `variableEq`); `.when_expression` passes the expression through verbatim.
- Edge ids are generated (`flow_<index>`); the compiled definition keys edges by node id, so they never collide with your identifiers.
- `.name()` and `.process_name()` are cosmetic only: they are carried on the shared intermediate model — like the BPMN `name` attribute and the DSL `name` field — and do **not** appear in the compiled `ProcessDefinition` (the compiled model has no name fields; the builder matches the BPMN and DSL entries here).

### Error behavior

`.build()` performs **no validation of its own**: it returns the shared compiler's `CompileError`s verbatim with their stable `ErrorCode`s — `NoStartEvent`, `MultipleStartEvents`, `NoEndEvent`, `OrphanNode`, `DeadEnd`, `ExclusiveGatewayNoDefault`, `ParallelGatewayInvalidShape`, `SequenceFlowSourceNotFound`, `SequenceFlowTargetNotFound`. For example, an edge referencing an unknown node id fails at `build()` time with `SequenceFlowTargetNotFound`, exactly like a dangling `sourceRef`/`targetRef` in XML.

Option setters apply to the most recently added node/edge (the classic BPMN fluent-builder convention). Misuse — `.retries()` on a user task, a condition before any `.edge()`, a duplicate node id, or a `when_variable_eq` variable containing quotes — panics immediately with a precise message: it is static programmer error, never process data, so it never reaches `build()`.

### Boundary: inline `ServiceTask` closures

The core `NodeType` also has a `ServiceTask(fn(&mut ProcessInstance))` variant for synchronous in-process closures. Function pointers cannot round-trip through BPMN/DSL, so the builder deliberately does not cover them — build such definitions by constructing the `ProcessDefinition` by hand. The builder supports exactly the BPMN-expressible subset, the same set as the JSON DSL.

### Semver and the API surface test

The facade's public surface (builder entry, DSL compile, core model types, runtime engine types, storage traits, memory adapter, worker SDK) is pinned by `tests/api_surface.rs`: every documented item is referenced through its `bpm_engine::` facade path and the load-bearing signatures are checked by `fn`-pointer coercion. Removing, renaming or re-signaturing any of them fails that test — you cannot make a breaking facade change without updating the snapshot. Additive changes update the test's list and the changelog; removals/signature changes require a major version bump.

### Features

The facade follows the same pattern for all opt-in surfaces:

- `worker-sdk` (default on) — re-exports `bpm_engine_worker_sdk` (its reqwest/tokio HTTP stack comes with it). Embed the engine without workers with `bpm-engine = { version = "0.2", default-features = false }`; the surface test's worker entries are compile-pinned only when the feature is enabled.
- `api` (off) — the axum REST server (`bpm-engine-runtime`'s `api` wiring).
- `observability` (off) — Prometheus metrics.

---

## Rust Worker SDK

The **Rust Worker SDK** lets you implement external-task workers that poll the engine, lock tasks, run your logic, and complete or fail tasks. No BPM knowledge is required in worker code.

---

## Crates and example

- **Crate:** [crates/worker-sdk](crates/worker-sdk)
- **Example:** [crates/worker-sdk/examples/payment.rs](crates/worker-sdk/examples/payment.rs)
- **Root example:** `cargo run --example builder_process` (typed builder + engine, no server needed)

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

Through the `bpm-engine` facade the worker SDK is available behind the default-on `worker-sdk` feature (`bpm_engine::bpm_engine_worker_sdk`) — `default-features = false` embedders opt out of it (and its HTTP stack), mirroring the `api`/`observability` features.

---

## See also

- [dsl-guide.md](dsl-guide.md) — the JSON DSL entry (agents), sharing the same compiler
- [bpmn.md](bpmn.md) — the BPMN 2.0 XML entry (interop)
- [api-spec.md](api-spec.md) — REST API (external-tasks endpoints)
- [execution-model.md](execution-model.md) — Token claim and concurrency
- [architecture.md](architecture.md) — Engine overview
