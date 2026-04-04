# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`bpm-engine` is a **token-driven, persistence-first BPM engine** written in Rust. It executes long-running workflows as persistent state machines where:
- Every execution step is driven by database state
- Every state transition is recorded as history
- Execution is crash-safe by construction
- External tasks use lease-based execution
- Timers are fully persistent

## Build & Test Commands

```bash
# Build all crates
cargo build

# Run all tests (workspace)
cargo test --workspace

# Run tests for a specific crate
cargo test -p bpm-core
cargo test -p bpm-runtime

# Run a specific test
cargo test -p bpm-core -- test_token_name

# Format code
cargo fmt

# Lint (fails on warnings)
cargo clippy --workspace --all-targets -- -D warnings
```

## Running Locally

```bash
# Terminal 1: Start REST server (http://127.0.0.1:3000)
cargo run -p bpm-server-rest

# Terminal 2: Run simple process example
cargo run --example simple_process

# Terminal 3: Run payment worker (external task)
cargo run -p bpm-worker-sdk --example payment
```

## Workspace Crates

| Crate | Responsibility |
|-------|---------------|
| `crates/core` | Core semantics: ProcessDefinition, NodeType, Token, EngineEvent, Saga. **No I/O, no storage.** Do not change casually. |
| `crates/storage` | Async persistence traits (ProcessInstanceStore, TokenStore, ExternalTaskStore, TimerStore, etc.) |
| `crates/runtime` | BpmEngine event loop, EngineContext, event handlers, gateway evaluation. Depends on storage traits only. |
| `crates/adapters/memory` | In-memory implementations of storage traits. Default for development/testing. |
| `crates/bpmn` | BPMN 2.0 XML parser → ProcessDefinition compiler |
| `crates/server/rest` | HTTP API server (axum). Wires EngineContext with memory adapter. |
| `crates/worker-sdk` | External task worker runtime: EngineClient, Worker, TaskHandler. Workers are stateless and horizontally scalable. |

## Core Abstractions

**Token** — Unit of execution. Represents authority to execute at a specific node. Multiple tokens enable parallelism. Token state transitions are persisted.

**EngineEvent** — Immutable event driving all state transitions. Handlers are deterministic and transactional. Guarantees observability, replayability, and crash safety.

**ProcessInstance** — Runtime container holding tokens and variables. Has lifecycle: Running → Completed/Terminated.

**ExternalTask** — Work delegated to external workers. Protected by lease (one owner at a time). Supports retries, timeouts, and crash handling.

## Key Entry Points

- `BpmEngine::run_async(event, &mut ctx)` — Main event loop (crates/runtime)
- `EngineContext` — Holds all store references, constructed by the server/embedder
- `bpm_server_rest::serve()` — HTTP server entry point

## Design Principles

- **Token over thread** — Concurrency is token-scoped
- **Event over call stack** — All state transitions are event-driven
- **Compensation over rollback** — Saga pattern for long-running consistency
- **Persistence over memory** — All state is persisted; engine recovers by replay

## Invariants

The engine enforces formal invariants (see `docs/invariants.md`):
- A token reaches a final state exactly once
- Join nodes only complete when all branches complete
- External tasks have exactly one owner at a time
- Retries are monotonic

## Important Constraints

- `crates/core` is **pure logic** — no I/O, no async, no storage traits. Changing it casually affects all downstream crates.
- The REST server uses the **memory adapter** by default (no database). For production, implement storage traits over PostgreSQL (see `docs/database-schema.md`).
- CI enforces: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`

## Feature Flags

- `api` — Enables axum-based REST server (`bpm-server-rest` crate)
- `observability` — Enables metrics + Prometheus exporter

## Key Documentation

- `docs/architecture.md` — Runtime architecture and design principles
- `docs/execution-model.md` — Token lifecycle and concurrency model
- `docs/invariants.md` — Formal invariants the engine guarantees
- `docs/recovery.md` — Crash recovery mechanism
- `docs/database-schema.md` — Persistence schema reference
