# Rust BPM Engine

> A native Rust BPM runtime engine for long-running, stateful workflows.

**Rust BPM Engine** is a lightweight, embeddable **Business Process Management (BPM) runtime**, designed for executing long-running workflows with **parallelism, timers, retries, human tasks, and Saga compensation** — without relying on BPMN XML or heavyweight platforms.

This project focuses on the **execution engine**, not visual modeling or low-code tooling.

---

## Why This Project

The Rust ecosystem lacks a production-grade BPM runtime that:

- Works natively in Rust
- Supports long-running business processes
- Handles failures, retries, and compensation correctly
- Does not depend on JVM, BPMN XML, or external workflow servers

This project fills that gap.

---

## What This Is (and Is Not)

### ✅ This is

- A **BPM runtime engine**
- Token-based execution model
- Event-driven core
- Crash-safe and resumable
- Designed for backend systems and orchestration

### ❌ This is NOT

- A BPMN modeler
- A low-code platform
- A workflow UI tool
- A distributed workflow SaaS (yet)

---

## Core Concepts

### Token-based Execution

- **Token** is the unit of execution
- Parallelism is achieved by multiple tokens, not threads
- Each token advances independently through the process graph

### Event-driven Engine

- All state transitions are triggered by events
- Event handlers are deterministic and transactional
- Engine progression is observable and replayable

### Saga Compensation

- Long-running transactions are handled via Saga
- Only successfully completed steps are compensated
- Compensation executes in reverse order using dedicated tokens

### Crash Recovery

- Engine state is fully persisted
- Tokens can be safely resumed after crashes
- No in-memory assumptions

---

## Key Features

- 🧠 Token-based workflow execution
- 🔀 Parallel fork / join support
- ⏱ Timers, delays, and timeouts
- 🔁 Retry with backoff
- 👤 Human task integration
- 📦 **External Task Worker** (pull-based fetch-and-lock / complete / fail; Worker SDK for Rust)
- 🔄 Saga compensation (long transactions)
- 💾 Persistent state & crash recovery
- ⚙️ Native Rust, async-friendly design

---

## High-level Architecture

```

API / Adapter
↓
Application Services
↓
BPM Engine Core

* Event Dispatcher
* Token Scheduler
* Node Executor
* Saga Coordinator
  ↓
  Persistence Layer
  ↓
  Infrastructure (DB / Clock / Logger)

```

For detailed design, see the architecture documentation.

---

## Getting Started

**Prerequisites:** Rust 1.70+ (`rustup`).

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
```

The project is a **Cargo workspace** with crates: `bpm-core`, `bpm-storage`, `bpm-runtime`, `bpm-adapter-memory`, `bpm-server-rest`, `bpm-worker-sdk`.

---

## Usage

### 1. REST API server

Run the Engine as an HTTP service (in-memory storage, no DB):

```bash
cargo run -p bpm-server-rest
```

Server listens on **http://127.0.0.1:3000**. Built-in process definitions: `minimal` (Start → End), `payment-flow` (Start → ExternalTask `payment` → End).

**Endpoints (base path `/api/v1`):**

| Method | Path | Description |
|--------|------|--------------|
| POST | `/process-instances` | Start instance. Body: `{ "process_def_id", "variables"?: {} }` → `{ "instance_id", "status" }` |
| GET | `/process-instances/:id` | Get instance. → `{ "instance_id", "status", "current_nodes" }` |
| GET | `/tasks?type=user\|external` | List waiting tasks. → `[{ "task_id", "node_id", "instance_id", "task_type" }]` |
| POST | `/tasks/:task_id/complete` | Complete user task. Body: `{ "variables"?: {} }` |
| POST | `/external-tasks/fetch-and-lock` | Worker: fetch and lock. Body: `{ "worker_id", "task_types", "max_tasks"?, "lock_duration_ms" }` → array of tasks |
| POST | `/external-tasks/:task_id/complete` | Worker: complete. Body: `{ "worker_id", "variables"?: {} }` |
| POST | `/external-tasks/:task_id/fail` | Worker: fail. Body: `{ "worker_id", "error", "retry_after_ms"?: u64 }` |

Optional header: `x-tenant-id` for tenant isolation.

### 2. External Task Worker (Worker SDK)

Use the **Worker SDK** to run pull-based workers that fetch, execute, and complete/fail external tasks without touching BPM concepts.

**Quick run (payment example):**

1. Start the Engine:
   ```bash
   cargo run -p bpm-server-rest
   ```
2. In another terminal, start a process instance (e.g. `payment-flow`):
   ```bash
   curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
     -H "Content-Type: application/json" \
     -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}'
   ```
3. Run the payment worker:
   ```bash
   cargo run -p bpm-worker-sdk --example payment
   ```

The worker polls the Engine, locks the `payment` task, runs the handler, then completes it; the process continues to End.

**Using the Worker SDK in your code:**

Add to `Cargo.toml` (workspace member or path dependency):

```toml
[dependencies]
bpm-worker-sdk = { path = "crates/worker-sdk" }
```

Implement [TaskHandler](crates/worker-sdk/src/handler.rs) and run a [Worker](crates/worker-sdk/src/worker.rs):

```rust
use bpm_worker_sdk::{
    EngineClient, ExternalTask, TaskContext, TaskHandler, TaskResult, Worker, WorkerConfig,
};
use std::time::Duration;

struct MyHandler;
#[async_trait::async_trait]
impl TaskHandler for MyHandler {
    fn task_type(&self) -> &str { "my-task" }
    async fn handle(&self, task: ExternalTask, _ctx: TaskContext) -> TaskResult {
        // ... use task.variables; return TaskResult::Complete { variables } or Fail { error, retry_after }
    }
}

let worker = Worker::builder()
    .client(EngineClient::new("http://127.0.0.1:3000"))
    .handler(MyHandler)
    .config(WorkerConfig::new("worker-1").poll_interval(Duration::from_secs(1)))
    .build();
worker.start().await;
```

See [crates/worker-sdk/examples/payment.rs](crates/worker-sdk/examples/payment.rs) for a full example.

### 3. Examples

| Location | Command | Description |
|----------|---------|--------------|
| **Payment worker** | `cargo run -p bpm-worker-sdk --example payment` | Pull-based worker for `payment` external tasks; requires Engine running. |
| **Basic order** | `cargo run --example basic_order` | Minimal stub (root package). |

### 4. Using the engine as a library

Depend on workspace crates by path:

```toml
[dependencies]
bpm-core     = { path = "crates/core" }
bpm-storage  = { path = "crates/storage" }
bpm-runtime  = { path = "crates/runtime" }
bpm-adapter-memory = { path = "crates/adapters/memory" }
# Optional: Worker SDK (HTTP client + worker runtime)
bpm-worker-sdk = { path = "crates/worker-sdk" }
```

- **bpm-core**: ProcessDefinition, NodeType (Start, End, UserTask, ExternalTask, gateways), Token, ProcessInstance, EngineEvent.
- **bpm-storage**: Async traits (ProcessInstanceRepo, TokenRepo, ExternalTaskStore, etc.).
- **bpm-runtime**: BpmEngine, handlers (ProcessStart, TokenArrived, UserTaskCompleted, etc.), transition helpers.
- **bpm-adapter-memory**: MemoryRepo implementing storage traits; ProcessDefStore for in-memory definitions.
- **bpm-worker-sdk**: EngineClient, Worker, TaskHandler, TaskResult; no BPM knowledge required for worker code.

Build an [EngineContext](crates/runtime/src/handler.rs) with repos, then run `BpmEngine::run_async(initial_event, &mut ctx)`. See [crates/server/rest](crates/server/rest) for wiring.

---

## Documentation

- 📘 [Architecture Overview](docs/docs_architecture.md)
- ⚙️ [Execution Model (Token & Concurrency)](docs/docs_execution_model.md)
- 🔄 [Saga & Compensation](docs/docs_saga.md)
- ♻️ [Crash Recovery & Rehydration](docs/docs_recovery.md)
- 🗄️ [Database Schema](docs/docs_database_schema.md)
- 🧪 [Testing Strategy](docs/docs_testing_strategy.md)
- 📋 [BPMN mapping](docs/docs_bpmn_mapping.md)
- ❓ [FAQ and common errors](docs/docs_faq.md)

---

## Status

🚧 **Early development / Architecture-first phase**

- Core design is stable
- Implementation is in progress
- APIs may change

---

## Design Philosophy

> Token is the unit of execution.  
> Event is the unit of progress.  
> Saga is the unit of resilience.

---

## Roadmap

### v1

- Single-node engine
- Code-defined workflows
- Core runtime features

### v2

- BPMN adapter layer
- Improved timer scheduling
- Execution visualization

### v3

- Multi-engine coordination
- Horizontal scalability
- Advanced observability

---

## License

MIT
