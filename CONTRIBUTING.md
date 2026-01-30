# Contributing to bpm-engine

Contributions are welcome. This document explains how to get started.

## Build and test

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
cargo test --workspace
```

Run the REST server and a minimal example:

```bash
# Terminal 1
cargo run -p bpm-server-rest

# Terminal 2
cargo run --example simple_process
```

## Code style

- Format with `cargo fmt`
- Lint with `cargo clippy`
- Ensure all tests pass before submitting a PR

## Areas where help is valuable

- **Testing and invariant cases**: Add tests that assert formal invariants (token lifecycle, join semantics, external task ownership). See [docs/invariants.md](docs/invariants.md) and [docs/docs_testing_strategy.md](docs/docs_testing_strategy.md).
- **Documentation**: Improve README, doc comments, and design docs (architecture, persistence, replay).
- **Worker SDK ergonomics**: Improve the Rust Worker SDK API and examples.
- **Visualization tools**: Read-only execution inspector, trace viewers.

## Module boundaries

The workspace is split into crates with clear roles:

- **crates/core**: Core semantics (process, token, node types). Do not change casually.
- **crates/storage**: Persistence traits and runtime tables.
- **crates/runtime**: Scheduler and token execution (handlers, transition, gateway).
- **crates/adapters/memory**: In-memory implementation of storage traits.
- **crates/bpmn**: BPMN 2.0 XML parser and compiler to ProcessDefinition.
- **crates/server/rest**: HTTP API and deploy endpoint.
- **crates/worker-sdk**: External task fetch/lock/complete client and worker runtime.

See [docs/docs_architecture.md](docs/docs_architecture.md) for details.

## Good first issues

- Add one invariant test and document it in `docs/invariants.md`
- Improve README Getting Started for a specific OS or environment
- Fix or extend a doc comment in a core crate
- Add a small example (e.g. timer-based process) under `examples/`

## Pull requests

Open a PR against the default branch. Describe the change and how it fits the project’s focus on correctness and execution semantics.
