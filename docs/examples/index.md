---
layout: default
---

# Runnable Examples Index

Every runnable example, template, and harness in the repo, mapped to what
it demonstrates and how to run it. Start with the
[15-minute quickstart](../quickstart.md); come here when you know what you
want to see next.

The three machine-readable contracts that make all of this scriptable:
the **agent tool catalog** (`GET /capabilities`, served verbatim from
`crates/server/rest/capabilities/agent-tools.json` —
[agent-tools.md](../agent-tools.md)), the **DSL JSON Schema**
(`crates/dsl/schema/bpm-dsl-v1.json` — [dsl-guide.md](../dsl-guide.md)),
and the **OpenAPI spec** (`docs/openapi.yaml` — [api-spec.md](../api-spec.md)).

## Root examples (`cargo run --example <name>` from the repo root)

| Example | Demonstrates | Run | Needs a server? |
|---|---|---|---|
| `simple_process` | The minimal loop over REST: start a `minimal` (Start → End) instance, poll to `COMPLETED`. No worker. | `cargo run --example simple_process` | Yes (`cargo run -p bpm-engine-server-rest`) |
| `scenario_payment_retry` | Accident-level behavior over REST: start `payment-flow`, fetch/lock the payment task, **fail it twice** (retry ladder), then complete; prints the trace's `external_task_history`. | `cargo run --example scenario_payment_retry` | Yes |
| `builder_process` | The **typed builder** (`ProcessBuilder`) + embedded engine (no HTTP at all): build, deploy, run to completion, print outcome. | `cargo run --example builder_process` | No |
| `ai_flow` | The AI reference **approval flow** end to end: spawns its own engine, deploys the DSL flow, runs it with the offline deterministic mock model provider; prints variables, step timeline, and each step's AI envelope (model + cost, tools). | `cargo run --example ai_flow` | No (self-contained) |
| `basic_order` | Scaffold stub only (`println!`) — not a working demo. | `cargo run --example basic_order` | — |

Docs: `docs/sdk-rust.md` (builder + worker SDK), `docs/ai-reference-flows.md`.

## Crate examples

| Example | Demonstrates | Run |
|---|---|---|
| `crates/worker-sdk/examples/payment.rs` | The worker side of the lease protocol: `EngineClient` + `Worker` + an idempotent `TaskHandler` for task type `payment`. The quickstart's worker. | `cargo run -p bpm-engine-worker-sdk --example payment` (needs a server; it polls until Ctrl-C) |
| `crates/worker-sdk/examples/graceful_shutdown.rs` | Graceful worker shutdown: `start_until_signal` with a stop flag; in-flight tasks drain before exit (`docs/worker-context.md`). | `cargo run -p bpm-engine-worker-sdk --example graceful_shutdown` (needs a server) |
| `crates/worker-sdk/examples/idempotency.rs` | Worker-side side-effect idempotency: the `IdempotencyGuard` key pattern for at-least-once redelivery. | `cargo run -p bpm-engine-worker-sdk --example idempotency` (needs a server) |
| `crates/worker-sdk/examples/duplicate_workers.rs` | Two workers competing for the same tasks: lease-based exactly-one-winner delivery. | `cargo run -p bpm-engine-worker-sdk --example duplicate_workers` (needs a server) |
| `crates/server/rest/examples/mint_jwt.rs` | Minting an HS256 JWT with the engine's secret (roles, tenants, expiry) for JWT mode / MCP wiring. | `BPM_JWT_SECRET=… cargo run -p bpm-engine-server-rest --example mint_jwt -- --sub alice --tenant acme --role operator` |
| `crates/server/rest/examples/dump_capabilities.rs` | Regenerating the agent tool catalog artifact from the OpenAPI spec. | `cargo run -p bpm-engine-server-rest --example dump_capabilities` |
| `crates/dsl/examples/dump_schema.rs` | Regenerating the DSL JSON Schema from the types (the lockstep test fails if the committed file drifts). | `cargo run -p bpm-engine-dsl --example dump_schema` |
| `crates/dsl/examples/simple_dsl.rs` | Compiling a DSL JSON document to a `ProcessDefinition` in Rust. | `cargo run -p bpm-engine-dsl --example simple_dsl` |
| `crates/ai-reference/examples/regression_eval.rs` | The offline **regression evaluation harness** (E6-3): run fixture flows against two definition versions and diff the reports. | `cargo run -p bpm-engine-ai-reference --example regression_eval [report.json]`; also `cargo test -p bpm-engine-ai-reference --test regression_eval` (`docs/regression-eval.md`) |

## Templates (`templates/`, E4-4)

| Template | Demonstrates | Use |
|---|---|---|
| `templates/flows/basic-gateway.json` | External task + exclusive gateway on a variable (`docs/dsl-guide.md` format; schema-validated by a workspace test). | validate/dry-run over REST, compile from Rust — `templates/flows/README.md` |
| `templates/flows/ai-approval-flow.json` | The AI approval shape: model task → human approval form → gateway → side-effect task. | same; worker for `ai-propose`/`ai-act` |
| `templates/flows/timer-wait.json` | A persistent `PT5S` timer advancing without any worker. | compile + run; `docs/execution-model.md` |
| `templates/ai-worker/` | A standalone, compiling worker crate: poll loop + E3-3 tool security boundary + E3-1 AI task envelope. | copy it — `templates/ai-worker/README.md` |
| `templates/mcp-config.json` | MCP client wiring for the E4-2 stdio server. | drop into an MCP client — `templates/README.md` |

## Scripts and harnesses

| What | Demonstrates | Run |
|---|---|---|
| `scripts/verify-quickstart.sh` | The quickstart's commands, verbatim, with assertions — the CI drift guard (quickstart job). | `bash scripts/verify-quickstart.sh` |
| `scripts/ci.sh` | The local mirror of CI: fmt, clippy, tests, invariants. | `./scripts/ci.sh` |
| `deploy/verify-recovery.sh` | Crash recovery: kill the engine mid-flight, restart against the same DB, verify ordered history and no duplicate completion. | `./deploy/verify-recovery.sh` (`deploy/README.md`) |
| `ui/` | The read-only operations console (process instances, tasks, trace views). | `cd ui && npm ci && npm run dev` (`ui/README.md`) |
| `tests/` (repo root) | Engine-level invariants, builder parity, API surface pinning. | `cargo test --workspace` |

## Server entry points

| Binary | Purpose | Run |
|---|---|---|
| `api_server` | The REST engine server (axum; memory by default, postgres optional). | `cargo run -p bpm-engine-server-rest` |
| `bpm-engine-mcp-server` | The MCP stdio adapter over the REST catalog. | `BPM_BASE_URL=… cargo run -p bpm-engine-mcp-server` (`docs/mcp-server.md`) |
