# bpm-server-rest

HTTP API server for the BPM engine.

## Role

- REST API: deploy process definitions (BPMN 2.0 XML), start process instances, get instance state, list tasks, complete user tasks.
- External task API: fetch-and-lock, complete, fail (for workers).
- Wires `bpm-runtime` with `bpm-adapter-memory` (or another store implementation) and runs the engine in response to API calls.

## Usage

```bash
cargo run -p bpm-server-rest
```

Server listens on http://127.0.0.1:3000. See [docs/api-spec.md](../../docs/api-spec.md) and [docs/cheat-sheet.md](../../docs/cheat-sheet.md).

## Documentation

See [docs/api-spec.md](../../docs/api-spec.md) and [docs/architecture.md](../../docs/architecture.md).
