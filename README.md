# bpm-engine

**A correctness-first workflow execution kernel in Rust, designed for deterministic replay and crash-safe long-running processes.**

[![Docs](https://img.shields.io/badge/docs-github.pages-blue)](https://fanjia1024.github.io/bpm-engine/)
[![crates.io](https://img.shields.io/crates/v/bpm-engine)](https://crates.io/crates/bpm-engine)
[![CI](https://github.com/fanjia1024/bpm-engine/actions/workflows/ci.yml/badge.svg)](https://github.com/fanjia1024/bpm-engine/actions/workflows/ci.yml)

> **Documentation Site**: [https://fanjia1024.github.io/bpm-engine/](https://fanjia1024.github.io/bpm-engine/)

This project focuses on **execution semantics, persistence correctness, and crash safety**, rather than UI or low-code features. It is designed as a **token-driven, persistence-first BPM engine** with formally defined invariants.

---

## What is this?

`bpm-engine` is a **workflow / BPM execution engine** implemented in Rust.

At its core, it executes processes as **persistent token state machines**, where:

- Every execution step is driven by database state
- Every state transition is recorded as history
- Every execution can be replayed and verified
- Concurrency, retries, and crashes are first-class concerns

This makes the engine suitable for **long-running, distributed, and failure-prone workflows**.

---

## Why another BPM engine?

Most BPM engines optimize for **features and modeling UX**.

This engine is a **correctness-first workflow execution kernel**: it optimizes for **correctness**.

Specifically:

- Token state is **explicit and persisted**
- Execution is **crash-safe by construction**
- External tasks use **lease-based execution**
- Timers are **fully persistent**
- All executions are **auditable and replayable**
- Core behavior is protected by **formal invariants**

If you care about _why_ a process reached a certain state — not just _that_ it did — this engine is for you.

---

## When NOT to use bpm-engine

This engine is built for **correctness and auditability first**. Consider alternatives if:

- **You need low-code BPMN modeling and form designers** — Use Camunda or similar platforms that offer visual modeling and task UIs out of the box.
- **You rely heavily on complex human workflows and approval UIs** — This engine focuses on execution and semantics; it does not provide built-in task lists or forms.
- **Execution semantics do not matter; you only need “fast” or simple DAGs** — Lighter options (e.g. AWS Step Functions) may be simpler to adopt.

If your priority is **correctness, replay, and clear execution semantics**, this engine is a good fit.

---

## Core Concepts

### Process & Instance

- A **process definition** is an immutable execution graph
- A **process instance** is a container for runtime tokens

### Token

A token represents a unit of execution.

- Each token has a clear lifecycle
- State transitions are persisted
- Parallelism is modeled via token forking and joining

### External Task

External tasks allow work to be executed by external workers:

- Workers fetch tasks by topic
- Tasks are protected by **leases**
- Retries, timeouts, and crashes are handled by the engine
- **Engine** guarantees exactly-once token completion; **workers** are at-least-once and **must** implement idempotent handlers

### Timer

Timers are persistent and scheduler-driven:

- No in-memory timers
- Safe across restarts
- Naturally scalable

### History & Replay

- Every state change emits a history event
- Execution can be replayed deterministically
- History can be used for debugging, auditing, and verification

**Observability APIs:**

- **Execution history**: `GET /api/v1/process-instances/:id/history` — returns events with `sequence` and `category` (instance | token | external) for auditing and debug.
- **Aggregated trace**: `GET /api/v1/process-instances/:id/trace` — token timelines and external-task history for a high-level view.

**History API Semantics:** Events are append-only; sequence is globally ordered per instance; replay reproduces the same token state; schema is backward-compatible once released. API stability and History/Trace semantic guarantees: see [api-spec.md](docs/api-spec.md) (§ API & Semantic Stability, § History & Trace Semantic Guarantees).

**Crash recovery verification:** To verify correctness after kill → restart (no duplicate completion, ordered history), follow [deploy/README.md](deploy/README.md) and run `./deploy/verify-recovery.sh` from the repo root. For an accident-driven narrative (payment timeout → worker restart → idempotent complete, and why invariants hold), see [docs/accident-scenarios.md](docs/accident-scenarios.md).

### Invariants

The engine enforces formal invariants such as:

- A token can only reach a final state once
- Join nodes only complete when all branches complete
- External tasks have exactly one owner at a time
- Retries are monotonic

See [docs/invariants.md](docs/invariants.md) for details. For a semantic comparison with Camunda, Temporal, and AWS Step Functions, see [docs/why-correctness.md](docs/why-correctness.md).

---

## Architecture Overview

```
+-------------------+
| Process Engine    |
| ----------------- |
| Scheduler         |
| Token Executor    |
| Invariants        |
+-------------------+
        |
        v
+-------------------+
| Persistence       |
| (in-memory / DB)  |
| Runtime Tables    |
| History / Timers  |
+-------------------+

External Workers (fetch / lock / complete via API)
```

The persistence layer is the **single source of truth**. The default backend is **in-memory** (no database required for quick start). The engine can recover by re-running its schedulers. For a persistence-oriented deployment with PostgreSQL, see [docs/recovery.md](docs/recovery.md) and [docs/database-schema.md](docs/database-schema.md).

### Where to start reading the code

- **Engine entry**: `bpm-engine-runtime::BpmEngine::run_async`
- **Token transitions**: `crates/runtime/src/handler/*` (and related handlers)
- **Persistence boundary**: `bpm-engine-storage` traits (process, token, history, external task, timer)
- **History emission**: `EngineEvent` and `HistoryHandler` in runtime; `GET .../history` in REST
- **Invariants**: [docs/invariants.md](docs/invariants.md) and `tests/invariant_*.rs`

---

## Getting Started (5 minutes)

**Requirements:** Rust (stable). No Docker required for the default in-memory backend.

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
```

**1. Start the engine**

```bash
cargo run -p bpm-server-rest
```

Server listens on **http://127.0.0.1:3000**. Built-in process definitions: `minimal` (Start → End), `payment-flow` (Start → ExternalTask `payment` → End).

**2. Run a minimal process (Start → End)**

In another terminal, start an instance and poll until completed:

```bash
cargo run --example simple_process
```

Or with curl:

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"minimal"}'
# Then GET /api/v1/process-instances/:id until status is COMPLETED
```

**3. Run a process with an external task (payment)**

Start a process instance:

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}'
```

Run the payment worker (in a third terminal):

```bash
cargo run -p bpm-worker-sdk --example payment
```

The worker polls the engine, locks the `payment` task, runs the handler, then completes it; the process continues to End.

---

## Example: External Task Worker

```rust
use bpm_worker_sdk::{EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig};

struct PaymentHandler;

#[async_trait::async_trait]
impl TaskHandler for PaymentHandler {
    fn task_type(&self) -> &str { "payment" }
    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        // business logic
        let mut variables = std::collections::HashMap::new();
        variables.insert("status".to_string(), "PAID".to_string());
        TaskResult::Complete { variables }
    }
}

// Worker: stateless, crash-safe, horizontally scalable
let worker = Worker::builder()
    .client(EngineClient::new("http://127.0.0.1:3000"))
    .handler(PaymentHandler)
    .config(WorkerConfig::new("worker-1").poll_interval(std::time::Duration::from_secs(1)))
    .build();
worker.start().await;
```

See [crates/worker-sdk/examples/payment.rs](crates/worker-sdk/examples/payment.rs) for the full example.

---

## Guarantees

This engine provides the following guarantees:

- **Exactly-once token completion**
- **Crash-safe execution**
- **Deterministic replay**
- **Persistent timers**
- **Formal invariants** checked in tests

These guarantees are **design goals**, not best-effort behavior.

---

## Usage & API

### REST API (base path `/api/v1`)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/process-instances` | Start instance. Body: `{ "process_def_id", "variables"?: {} }` |
| GET | `/process-instances/:id` | Get instance status and current nodes |
| POST | `/process-definitions/deploy` | Deploy a process from BPMN 2.0 XML (body: raw XML) |
| GET | `/tasks?type=user\|external` | List waiting tasks |
| POST | `/tasks/:task_id/complete` | Complete user task |
| POST | `/external-tasks/fetch-and-lock` | Worker: fetch and lock tasks |
| POST | `/external-tasks/:task_id/complete` | Worker: complete task |
| POST | `/external-tasks/:task_id/fail` | Worker: fail task |

Optional header: `x-tenant-id` for tenant isolation.

### Workspace crates

- **bpm-core**: ProcessDefinition, NodeType (Start, End, UserTask, ExternalTask, gateways), Token, ProcessInstance, EngineEvent
- **bpm-storage**: Async traits (ProcessInstanceStore, TokenStore, ExternalTaskStore, etc.)
- **bpm-runtime**: BpmEngine, handlers, transition helpers
- **bpm-adapter-memory**: MemoryRepo; ProcessDefStore for in-memory definitions
- **bpm-bpmn**: BPMN 2.0 XML parser and compiler to ProcessDefinition
- **bpm-server-rest**: HTTP API server
- **bpm-worker-sdk**: EngineClient, Worker, TaskHandler; no BPM knowledge required for worker code

Using the engine as a library: depend on the crates above by path, build an [EngineContext](crates/runtime/src/handler.rs) with repos, then run `BpmEngine::run_async(initial_event, &mut ctx)`. See [crates/server/rest](crates/server/rest) for wiring.

---

## Documentation

- [Architecture Overview](docs/architecture.md)
- [Execution Model (Token & Concurrency)](docs/execution-model.md)
- [Invariants](docs/invariants.md)
- [Persistence & Recovery](docs/recovery.md)
- [Recovery demo (kill → restart)](docs/recovery-demo.md)
- [Accident-level scenarios](docs/accident-scenarios.md) (payment+retry, fork fail, worker reclaim)
- [Database Schema](docs/database-schema.md)
- [Saga & Compensation](docs/docs_saga.md)
- [Testing Strategy](docs/docs_testing_strategy.md)
- [BPMN mapping](docs/bpmn-spec-mapping.md)
- [API spec](docs/api-spec.md)
- [FAQ](docs/faq.md)
- [Cheat sheet](docs/cheat-sheet.md)
- [Rust Worker SDK](docs/sdk-rust.md)
- [Python SDK (planned)](docs/sdk-python.md)

---

## Project Status

This project is in **active development**.

- Core execution semantics are stable
- APIs may evolve
- Not yet recommended for mission-critical production use

That said, the engine is already suitable for:

- Research
- Prototyping
- Internal systems
- Correctness-focused experimentation

---

## Roadmap

- Worker SDK stabilization (Rust / Python)
- Read-only execution inspector (Cockpit-like UI)
- More invariant coverage
- Documentation & examples
- History / Replay documentation (TBD)

---

## Contributing

Contributions are welcome.

Areas where help is especially valuable:

- Testing and invariant cases
- Documentation
- Worker SDK ergonomics
- Visualization tools

Please see [CONTRIBUTING.md](CONTRIBUTING.md).

---

## License

MIT
