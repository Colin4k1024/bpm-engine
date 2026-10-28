# AI Reference Flows (E3-4)

Reference implementations for the five AI workflow shapes the engine's AI
stack supports — **检索 (retrieval)**, **推理 (inference)**, **审批
(approval)**, **工具 (tool)** and **补偿 (compensation)** — plus the
swappable **model-provider adapter layer** they run against.

Crate: `bpm-engine-ai-reference` (`crates/ai-reference`). Everything runs
**offline by default**: the mock model provider is deterministic (no
network, no clock, no randomness), and the only HTTP in the reference suite
is to a locally spawned engine — the same harness pattern as the engine's
own REST integration tests. CI runs the suite through plain
`cargo test --workspace`.

| artifact | location |
|---|---|
| provider adapter layer (trait, mock, cost, stub) | `crates/ai-reference/src/provider.rs` |
| OpenAI-compatible HTTP provider (feature `http`, off by default) | `crates/ai-reference/src/openai.rs` |
| the five flow DSL documents (inline consts) + `FlowSpec` constructors | `crates/ai-reference/src/flows.rs` |
| flow runner (`run_flow` / `run_flow_with`, `FlowOutcome`) | `crates/ai-reference/src/runner.rs` |
| the reference worker handler (task-type dispatch) | `crates/ai-reference/src/handler.rs` |
| the reference side-effect tool (E3-3 demo) | `crates/ai-reference/src/tools.rs` |
| the application-side saga coordinator | `crates/ai-reference/src/saga.rs` |
| integration tests (all five flows, REST-level) | `crates/ai-reference/tests/reference_flows.rs` |
| self-contained example (`cargo run --example ai_flow`) | `examples/ai_flow.rs` |

---

## 1. The model-provider adapter layer

Applications bring their own model backend; the crate defines the contract
and ships three implementations:

```rust
#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn generate(&self, request: ModelRequest) -> anyhow::Result<ModelResponse>;
}

pub struct ModelRequest { prompt: String, variables: HashMap<String,String>, max_tokens: Option<u32> }
pub struct ModelResponse { text: String, tokens_in: u64, tokens_out: u64 }
```

- **`MockModelProvider`** — deterministic and offline; the reference suite
  and CI always use it. Routing by prompt pattern:

  | prompt contains | response | tokens (in, out) |
  |---|---|---|
  | `summar` | canned summary (`MOCK_SUMMARY_TEXT`) | (48, 12) |
  | `classify` | `approved` / `rejected` — rejected iff the `risk` variable is exactly `"high"` | (24, 2) |
  | anything else | `echo: {prompt}` | (16, 8) |

  `max_tokens` clamps the reported output-token count. No randomness, no
  clock — every envelope a mock task produces is byte-stable.

- **`UnconfiguredProvider`** — the always-compiled "no provider configured"
  stub: `generate` fails with a clear error naming the options
  (`provider_unconfigured: ...`), so a forgotten wiring fails loudly at the
  first model task.

- **`OpenAiCompatProvider`** (crate feature `http`, **off by default**) —
  the real-provider adapter for applications, speaking the
  OpenAI-compatible chat-completions wire format (works against OpenAI and
  compatible gateways: vLLM, Ollama's OpenAI endpoint, ...). Configuration
  is environment-only and read **once, at construction** — never during
  `generate`, so tests can never depend on ambient credentials:

  | env var | meaning | default |
  |---|---|---|
  | `OPENAI_BASE_URL` | API root (without `/chat/completions`) | `https://api.openai.com/v1` |
  | `OPENAI_API_KEY` | bearer token (**required**) | — |
  | `OPENAI_MODEL` | model name in requests | `gpt-4o-mini` |

  Enable with `bpm-engine-ai-reference = { features = ["http"] }` (the
  feature also turns on TLS for the provider client). The adapter layer
  exists precisely so applications can swap providers; the reference suite
  itself always uses the mock.

### Cost helper (E6-1 envelope cost)

`ProviderConfig` holds the provider identity plus a per-model price table:

```rust
let config = ProviderConfig::mock();      // mock identity + default pricing
let cost: TaskCost = config.cost_for(&response);
```

`cost_for` maps a response's token counts onto the E3-1
[`TaskCost`](ai-task-envelope.md) with integer math:
`amount_micros = (tokens_in * input_price + tokens_out * output_price) / 1000`.
Mock pricing is 250 / 500 micro-USD per 1k input / output tokens, so the
three routes cost exactly 18 (summarize), 7 (classify) and 8 (echo)
micro-USD — assertable in CI. `duration_ms` stays `None` (the reference
suite uses no clock). Pass a custom table via
`run_flow_with(.., FlowOptions::default().pricing(config))` so a real
provider's envelopes carry its real prices.

---

## 2. The five reference flows

Each flow is a DSL v1 JSON document (`docs/dsl-guide.md`) stored inline as a
`pub const` (`RETRIEVAL_DSL` … `COMPENSATION_DSL`), wrapped by a
constructor into a `FlowSpec` (process id, task types, start variables,
user-task decisions). The DSL documents are the single source — the
constructors only add runtime parameters.

### 检索 Retrieval — `retrieval_flow()`

```json
start → retrieve (ref-retrieve) → summarize (ref-summarize, MODEL) → end
```

A plain external task returns a document list (`documents`); the model task
consumes it (chained variables, the E2-3 contract), calls the provider with
`summarize: <documents>`, and completes with `summary` plus the E3-1
envelope (model ref, cost, execution id, idempotency key). Demonstrates:
**model task over a plain task's output; envelope with model + cost visible
in `/steps`.**

### 推理 Inference — `inference_flow(risk)`

```json
start → classify (ref-classify, MODEL) → decide (exclusiveGateway)
          ├─ verdict == "approved" → act-approve → end-approved
          └─ default               → act-reject  → end-rejected
```

The classify task routes the model's verdict (`risk` variable: `"high"` →
`rejected`, else `approved`) into the instance as `verdict`; the gateway
routes on it via `variableEq`. Demonstrates: **the model's output driving
flow routing.**

### 审批 Approval — `approval_flow(approve)`

```json
start → propose (ref-propose, MODEL) → approve (userTask, enum form)
      → act (ref-act, side-effect tool behind the E3-3 approval gate) → end
```

The human submits the decision (`decision: approved|rejected`) at the user
task; the `act` task executes the `ledger_post` tool through the E3-3
boundary. **Fail-closed**: without the `__approval` task input the tool is
refused (`tool_requires_approval`, a *permanent* E3-2 failure — straight
to Failed + DLQ, no redelivery); with it the tool executes and its output
is credential-masked.

> v1 honesty: user-task completion variables are **not** merged into the
> instance by the runtime today (external-task completions are), so the
> approver's `__approval` rides in the instance (start) variables — the
> task-variables channel the SDK's gate actually reads (`docs/tool-security.md`
> §3.1). The user task records the human decision; the engine-side
> persistent approval flow (E5-2) will set `__approval` automatically in a
> future release.

### 工具 Tool — `tool_flow(memo, approved)`

```json
start → run-tool (ref-run-tool) → end
```

The worker executes the side-effectful `ledger_post` tool through the full
E3-3 boundary:

1. **Allowlist, deny by default** — only registered tools run;
2. **Approval gate** — side-effect tools need the `__approval = "true"`
   task input (here a start variable: the `__approval` variable path);
3. **Credentials from the worker environment only** —
   `CredentialSource::env(FlowOptions::tool_env_var)` (default
   `BPM_REFERENCE_LEDGER_KEY`); a missing var is a permanent
   `credential_missing` failure naming the var, never a value;
4. **Output masking + limits** — the raw tool output embeds the credential
   on purpose, so the boundary's masking (`***`) is observable in
   `ledger_result`.

### 补偿 Compensation — `compensation_flow(fail)`

```json
start → reserve (ref-saga-reserve) → charge (ref-saga-charge) → saga-gate
          ├─ saga_outcome == "failed" → undo (ref-saga-compensate) → end-compensated
          └─ default                  → ship (ref-saga-ship)        → end-ok
```

The saga shape (`docs/architecture.md` §Failure & Compensation): forward
steps (`reserve`, `charge`) complete; when the charge step reports business
failure (`saga_fail = "true"` start variable), the gateway routes to the
`undo` step, which compensates the completed forward steps in **REVERSE
order** (`compensation_log = "undo-charge;undo-reserve"`).

How compensation works in this engine (kept honest): the engine *persists*
[`CompensationRecordRow`]s through the `CompensationRecordRepo` (wired into
every `EngineContext`, including the REST server's) and records
`SagaStarted`/`SagaCompleted` history events, but the loop itself is
**application-coordinated**. The crate therefore ships `SagaCoordinator`:

```rust
// record completed forward steps (observing the step trace)
SagaCoordinator::record_from_steps(&repo, &outcome, &SAGA_FORWARD_STEPS).await?;
// compensate newest-first, stopping at the first failure
let outcomes = SagaCoordinator::compensate(&repo, &instance_id, |record| {
    // invoke the undo handler for record.node_id
    Ok(())
}).await?;
```

The integration test wires this against a live server's real compensation
store (`state.storage.compensation_repo`). Statuses: the v1 storage trait
is append-only (`add` + `list_by_instance`), so the coordinator reports
what ran through returned `CompensationOutcome`s (mirroring
`tests/saga_compensation.rs`).

---

## 3. Running the flows — `run_flow`

```rust
use std::sync::Arc;
use bpm_engine_ai_reference::{deploy_flow, run_flow, retrieval_flow, MockModelProvider, ModelProvider};

// 1. Deploy (once): the REST deploy endpoint accepts BPMN XML only, so DSL
//    flows register in-process through the DefinitionDeployer trait.
deploy_flow(state.storage.deployer.as_ref(), &retrieval_flow()).await?;

// 2. Run: spawns a real worker, starts the instance, drives it to a
//    terminal state, snapshots state + variables + steps.
let provider: Arc<dyn ModelProvider> = Arc::new(MockModelProvider::new());
let outcome = run_flow(&base_url, retrieval_flow(), provider).await?;

assert!(outcome.is_completed());
assert_eq!(outcome.variable("summary"), Some(MOCK_SUMMARY_TEXT));
outcome.step("summarize").unwrap().envelope.as_ref().unwrap()["cost"]["amount_micros"];
```

`run_flow_with(server_url, flow, provider, options)` tunes
`FlowOptions`: worker id, timeout (blocked flows surface as `RUNNING`
outcomes, not errors), poll intervals, `tool_env_var`, envelope pricing,
model-call timeout, shutdown grace.

The worker is one dispatching handler registered once per flow task type
(`Worker::spawn` + graceful `WorkerHandle::shutdown`, E3-2); every dispatch
runs under `run_with_renewal` and every model call under `with_timeout`
(E3-2 ergonomics). Model tasks complete via `TaskOutput::build()`: the E3-1
envelope rides in the reserved `__task_envelope` variable of the completion
body — byte-identical to `EngineClient::complete_with_envelope`'s wire
format (the SDK's own contract: "a worker completes exactly as today, with
the envelope traveling in the same variables map"); a unit test pins the
parity. Tool errors map to E3-2-aware failures:
`tool_not_allowed`/`tool_requires_approval`/`credential_missing` are
permanent, `tool_execution_failed` stays transient.

A runnable, fully self-contained version (spawns the server itself) is
`examples/ai_flow.rs`:

```text
cargo run --example ai_flow
```

---

## 4. How CI runs them

`cargo test --workspace` picks up:

- `crates/ai-reference` unit tests (provider routing, cost math, handler
  dispatch, envelope wire-format parity, saga loop semantics, DSL compile
  smoke for all five documents),
- `crates/ai-reference/tests/reference_flows.rs` — REST-level integration
  for all five flows: `build_app` + ephemeral port (memory storage), real
  workers over real HTTP to `127.0.0.1`, the mock provider. Asserts
  routing decisions, envelope presence in `/steps` summaries (model + cost
  + tools), the approval gate blocked without `__approval` and passing with
  it, DLQ entries for permanently failed tasks, credential masking, and
  reverse-order compensation over the engine's store.

No Docker, no network, no keys — the suite is part of the default CI job
(`.github/workflows/ci.yml`: `cargo test --workspace`), the same job that
runs `cargo fmt --check` and
`cargo clippy --workspace --all-targets -- -D warnings`.

## 5. Extending

- **A new flow**: author the DSL JSON (validate with
  `POST /process-definitions/validate` with `format: "dsl"` or
  `bpm_engine_dsl::compile`), add a `pub const *_DSL`, a constructor, and
  task types to `handler::task_type` + a dispatch arm. `compile_flow`
  fails fast on broken documents.
- **A new tool**: implement `bpm_engine_worker_sdk::tools::Tool` (declare
  `has_side_effects`/`requires_approval` honestly — the SDK enforces the
  declared flags) and register it in the handler's executor.
- **A new provider**: implement `ModelProvider` (`generate` only); pass it
  to `run_flow` and (for real pricing) `FlowOptions::pricing`. See
  `openai.rs` for the HTTP pattern — keep credential reads at construction
  time.
