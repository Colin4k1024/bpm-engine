# Cheat Sheet — Quick commands and local run

Short reference for building, testing, and running the BPM engine.

---

## Build & test

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
cargo test --workspace
cargo fmt
cargo clippy --workspace --all-targets
```

---

## Run the REST server

```bash
cargo run -p bpm-server-rest
```

Server: **http://127.0.0.1:3000**

---

## Run a minimal process (Start → End)

**Terminal 1:** start server (above).

**Terminal 2:**

```bash
cargo run --example simple_process
```

Or with curl:

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"minimal"}'
```

Then poll until completed:

```bash
curl http://127.0.0.1:3000/api/v1/process-instances/<instance_id>
```

---

## Run a process with external task (payment)

**Terminal 1:** `cargo run -p bpm-server-rest`  
**Terminal 2:** start instance:

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}'
```

**Terminal 3:** run the payment worker:

```bash
cargo run -p bpm-worker-sdk --example payment
```

---

## REST API (base `/api/v1`)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/process-instances` | Start instance. Body: `{ "process_def_id", "variables"?: {} }` |
| GET | `/process-instances/:id` | Get instance status |
| POST | `/process-definitions/deploy` | Deploy from BPMN 2.0 XML (body: raw XML) |
| GET | `/tasks?type=user\|external` | List waiting tasks |
| POST | `/tasks/:task_id/complete` | Complete user task |
| POST | `/external-tasks/fetch-and-lock` | Worker: fetch and lock tasks |
| POST | `/external-tasks/:task_id/complete` | Worker: complete task |
| POST | `/external-tasks/:task_id/fail` | Worker: fail task |

Optional header: `x-tenant-id`.

---

## Curl examples

**Start instance:**

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"minimal"}'
```

**Get instance:**

```bash
curl http://127.0.0.1:3000/api/v1/process-instances/<id>
```

**Deploy definition (XML):**

```bash
curl -X POST http://127.0.0.1:3000/api/v1/process-definitions/deploy \
  -H "Content-Type: application/xml" \
  --data-binary @your.bpmn
```

---

## Documentation index

- [architecture.md](architecture.md) — Runtime architecture
- [execution-model.md](execution-model.md) — Token lifecycle, concurrency
- [database-schema.md](database-schema.md) — Persistence schema
- [recovery.md](recovery.md) — Crash recovery
- [bpmn-spec-mapping.md](bpmn-spec-mapping.md) — BPMN support
- [api-spec.md](api-spec.md) — REST & gRPC API
- [faq.md](faq.md) — FAQ and common errors
- [sdk-rust.md](sdk-rust.md) — Rust Worker SDK
- [invariants.md](invariants.md) — Formal invariants
- [docs_testing_strategy.md](docs_testing_strategy.md) — Testing strategy
