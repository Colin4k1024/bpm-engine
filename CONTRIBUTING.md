# Contributing to bpm-engine

Contributions are welcome. This document explains how to get started.

## How to contribute

1. **Pick an issue** — Check [Issues](https://github.com/fanjia1024/bpm-engine/issues) for bugs or features labeled "good first issue", or open a new issue to discuss a change.
2. **Branch** — Create a branch from the default branch (e.g. `fix/description` or `feat/description`).
3. **Implement** — Follow code style (fmt, clippy) and add or update tests as needed.
4. **PR** — Open a pull request against the default branch. Use the [PR template](.github/PULL_REQUEST_TEMPLATE.md) checklist. Describe the change and how it fits the project focus (correctness, execution semantics, persistence).

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

## Code style and CI

- Format with `cargo fmt`
- Lint with `cargo clippy --workspace --all-targets -- -D warnings`
- Ensure all tests pass: `cargo test --workspace`
- CI runs on push/PR: fmt check, clippy, and tests must pass

## Areas where help is valuable

- **Testing and invariant cases**: Add tests that assert formal invariants (token lifecycle, join semantics, external task ownership). See [docs/invariants.md](docs/invariants.md) and [docs/docs_testing_strategy.md](docs/docs_testing_strategy.md). See also [docs/cheat-sheet.md](docs/cheat-sheet.md) for quick commands.
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

See [docs/architecture.md](docs/architecture.md) for details.

## Documentation and tests

- When adding or changing behavior, update relevant docs under [docs/](docs/) (e.g. [architecture.md](docs/architecture.md), [execution-model.md](docs/execution-model.md), [faq.md](docs/faq.md)).
- When fixing a bug or adding a feature, add or extend tests (unit or integration under [tests/](tests/)). See [docs/docs_testing_strategy.md](docs/docs_testing_strategy.md) and [docs/invariants.md](docs/invariants.md).

## Good first issues

- Add one invariant test and document it in `docs/invariants.md`
- Improve README Getting Started for a specific OS or environment
- Fix or extend a doc comment in a core crate
- Add a small example (e.g. timer-based process) under `examples/`

## Pull requests

Open a PR against the default branch. Use the [PR template](.github/PULL_REQUEST_TEMPLATE.md). Describe the change and how it fits the project’s focus on correctness and execution semantics.
