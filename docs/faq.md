# FAQ and common errors (plan v2.0 E.3)

## Frequently asked questions

### How do I define a process without writing Rust?

Use the **DSL (JSON)**. Define your process in JSON (see [dsl](src/dsl/mod.rs) for the schema), register ServiceTask handlers by name via [ServiceTaskRegistry](src/dsl/registry.rs), then call [load_and_register_json](src/dsl/load.rs) or [load_from_json](src/dsl/load.rs) + [to_process_definition](src/dsl/convert.rs) and register the result in [ProcessDefStore](src/persistence/memory.rs).

### How do I add a custom ServiceTask handler?

Create a function `fn(&mut ProcessInstance)` and register it: `registry.register("my_handler", my_fn)`. Use the same name in your DSL/BPMN as `handler_ref` for the ServiceTask node.

### Can I use BPMN 2.0 models?

A minimal **BPMN-like JSON** is supported (see [bpmn](src/bpmn/mod.rs) and [bpmn-spec-mapping.md](bpmn-spec-mapping.md)). Map StartEvent, EndEvent, UserTask, ServiceTask, ExclusiveGateway, ParallelGateway to the engine. Full BPMN XML/JSON from external tools may require a custom adapter.

### How do I run the engine as an HTTP service?

Build with the `api` feature and run the API server: `cargo run --bin api_server --features api`. Optionally add `observability` for tracing and Prometheus metrics: `cargo run --bin api_server --features "api,observability"`.

---

## Common errors and solutions

| Symptom / message | Cause | Solution |
|-------------------|--------|----------|
| **"process instance not found"** (404) | GET /processes/:id for non-existent instance | Ensure the instance was started (POST /processes/start) and the id is correct. |
| **"ServiceTask handler not registered: X"** | DSL/BPMN references a handler name not in the registry | Call `registry.register("X", your_fn)` before loading the process definition. |
| **"definition not found"** (404) | GET /definitions/:id/diagram for unknown definition | Register the process definition (e.g. via ProcessDefStore or load_and_register_json) with that id. |
| **"empty expression"** (EL) | Gateway condition is blank | Use a non-empty condition: e.g. `key == "value"` or `key` for truthy. |
| **"unrecognized expression"** (EL) | Condition format not supported | Use supported forms: `key`, `key == "x"`, `key != "x"`, `key > n`, or `a and b`, `a or b`. |
| **Token claim failed (CAS)** (warn log) | Concurrent claim; another worker took the token | Normal under concurrency; only one claim succeeds. No change needed. |
| **Metrics not initialized** (503 on /metrics) | GET /metrics without observability init | Run api_server with `--features "api,observability"` and ensure Prometheus recorder is installed at startup. |

---

## Design philosophy (short)

- **Token-based**: One token per path of execution; parallelism via multiple tokens.
- **Event-driven**: All state changes are driven by events (ProcessStarted, TokenArrived, UserTaskCompleted, etc.).
- **Pluggable storage**: Use [MemoryRepo](src/persistence/memory_repo.rs) for tests and embedding, or [InstanceRepo](src/persistence/sqlite.rs) (SQLite) for persistence.
- **Recovery**: Running instances with Ready/Executing tokens are reconciled on startup via [recovery::recover](src/recovery.rs).
