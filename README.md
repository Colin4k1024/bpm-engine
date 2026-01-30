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

---

## Usage

### Default demo (approval process)

Running the default binary starts the **approval** demo: Start → validate (ServiceTask) → gateway (ExclusiveGateway) → approve (UserTask) or reject (End) → end. State is stored in `bpm.db`; on boot, recovery runs and re-dispatches any Ready/Executing tokens.

```bash
cargo run
```

You will see the process start, pause at the UserTask `approve`, then complete after the engine receives `UserTaskCompleted`.

### Examples

Runnable examples live in `examples/`. Run any of them with:

```bash
cargo run --example <name>
```

| Example                | Command                                  | Description                                                                  |
| ---------------------- | ---------------------------------------- | ---------------------------------------------------------------------------- |
| **minimal**            | `cargo run --example minimal`            | Start → End. In-memory SQLite; process completes in one run.                 |
| **approval**           | `cargo run --example approval`           | Same flow as the default demo (validate → gateway → approve/reject → end).   |
| **exclusive_gateway**  | `cargo run --example exclusive_gateway`  | Start → ServiceTask → ExclusiveGateway (EL + VariableEq) → end_a or end_b.   |
| **el_gateway**         | `cargo run --example el_gateway`         | Gateway with EL expressions only: `choice == "a"`, `amount > 50`, Default.   |
| **service_task_chain** | `cargo run --example service_task_chain` | Start → step1 → step2 → step3 → End (linear ServiceTask chain).              |
| **reject_path**        | `cargo run --example reject_path`        | Approval topology but variable set to reject; process ends without UserTask. |
| **parallel_fork_join** | `cargo run --example parallel_fork_join` | Start → Fork → (branch_a, branch_b) → Join → End (ParallelFork/Join).        |

- **minimal**: Two-node process (start, end); template for the smallest run.
- **approval**: Full approval flow with ServiceTask, ExclusiveGateway, and UserTask; simulates “complete user task” so you see the same behavior as `cargo run` without `bpm.db`.
- **exclusive_gateway**: Branching with EL expression (`choice == "a"`) and VariableEq; first matching edge wins.
- **el_gateway**: Gateway conditions using EL only: string equality (`choice == "a"`), numeric comparison (`amount > 50`), and Default.
- **service_task_chain**: Three ServiceTasks in sequence; shows linear automation.
- **reject_path**: Same graph as approval; ServiceTask sets `valid = "false"` so the gateway takes Default → reject (no UserTask).
- **parallel_fork_join**: ParallelFork creates two tokens; both run branch_a/branch_b; ParallelJoin waits for both, then one token continues to End (uses in-memory join state).

### Using the engine as a library

**From [crates.io](https://crates.io/crates/bpm-engine)** (recommended):

Add to your project’s `Cargo.toml`:

```toml
[dependencies]
bpm-engine = "0.1"
```

**From a local path** (e.g. for development or forking):

```toml
[dependencies]
bpm-engine = { path = "../bpm-engine" }
```

Then define a [ProcessDefinition](src/model.rs), build an [EngineContext](src/engine/handler.rs) with repos (e.g. [InstanceRepo](src/persistence/sqlite.rs)), and run [BpmEngine::run](src/engine/pump.rs) with events such as `ProcessStarted` and `UserTaskCompleted`. Gateway conditions support **EL expressions** ([engine/el](src/engine/el.rs)): use `EdgeCondition::Expression("key == \"value\"")` or `"key > 100"` for numeric comparison. See the `examples/` directory in this repo for full code (minimal, approval, exclusive_gateway, el_gateway, service_task_chain, reject_path, parallel_fork_join).

---

## Documentation

- 📘 [Architecture Overview](docs/docs_architecture.md)
- ⚙️ [Execution Model (Token & Concurrency)](docs/docs_execution_model.md)
- 🔄 [Saga & Compensation](docs/docs_saga.md)
- ♻️ [Crash Recovery & Rehydration](docs/docs_recovery.md)
- 🗄️ [Database Schema](docs/docs_database_schema.md)
- 🧪 [Testing Strategy](docs/docs_testing_strategy.md)

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
