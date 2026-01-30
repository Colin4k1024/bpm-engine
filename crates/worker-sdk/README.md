# bpm-worker-sdk

Rust Worker SDK: pull-based client and worker runtime for external tasks.

## Role

- **EngineClient** — HTTP client for the engine (fetch-and-lock, complete, fail).
- **Worker** — poll loop that fetches tasks, calls your handler, and completes or fails tasks.
- **TaskHandler** — trait to implement: `task_type()` and `handle(task, ctx) -> TaskResult`.

No BPM knowledge required in worker code; workers only see task type and variables.

## Usage

```bash
cargo run -p bpm-worker-sdk --example payment
```

See [crates/worker-sdk/examples/payment.rs](examples/payment.rs) for a full example.

## Documentation

See [docs/sdk-rust.md](../../docs/sdk-rust.md) and [docs/api-spec.md](../../docs/api-spec.md).
