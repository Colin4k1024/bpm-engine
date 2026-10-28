# ai-worker — External-Task Worker Template

A minimal, self-contained Rust crate you can copy to start an AI-driven
external-task worker for bpm-engine. It compiles on its own (`cargo build`
in this directory) and depends on the workspace's worker SDK by relative
path — when you copy it out of the repo, point the dependency at a
published version or your vendored checkout.

It is **excluded from the workspace** (root `Cargo.toml`, `exclude`) so it
never affects `cargo build --workspace`, but CI builds it separately in the
`quickstart` job, so it cannot drift.

## What it wires together

| Concern | Mechanism | Doc |
|---|---|---|
| Poll / lock / complete loop | `Worker`, `EngineClient`, `TaskHandler` | `docs/sdk-rust.md` |
| Tool allowlist (deny by default) | `ToolExecutor::register_tool` | `docs/tool-security.md` |
| Approval gate (fail-closed) | `ToolSpec::side_effects()` + `__approval` task input | `docs/tool-security.md` §3 |
| Credentials (worker env only) | `CredentialInjector` + `BPM_LEDGER_API_KEY` | `docs/tool-security.md` §1 |
| Output hygiene (redact, mask, size-limit) | `TaskOutput::add_tool_output` | `docs/tool-security.md` §1 |
| AI task envelope (`__task_envelope`) | `TaskOutput::envelope(new_envelope(...))` | `docs/ai-task-envelope.md` |
| Idempotency reminder | handlers are **at-least-once** | `docs/recovery.md` §8 |

## Run it

1. Start an engine (`docs/quickstart.md` steps 1–2).
2. Run a flow whose `serviceTask` has `task_type: "ai-act"` — e.g.
   `templates/flows/ai-approval-flow.json` (deploy it from Rust via
   `bpm_engine_dsl::compile`, or use the `ai_flow` example for a
   self-contained version of the same shape).
3. Provide the (fake) credential and start the worker:

   ```bash
   export BPM_LEDGER_API_KEY=sk-demo-not-a-real-secret
   export BPM_BASE_URL=http://127.0.0.1:3000   # default
   cargo run
   ```

4. Approve the human task (the `approve` user task) with `__approval =
   "true"` in the task variables — the `act` task's side-effect tool
   refuses to run without it (`tool_requires_approval`).

## Customization walkthrough

1. **Pick your task type** — change `TASK_TYPE` to the `task_type` your
   flow's `serviceTask` node declares. This is the fetch filter: the worker
   only ever receives tasks of the types it registers.
2. **Replace the tool** — keep the pattern from `LedgerPostTool`:
   implement `Tool::spec()` (mark `side_effects()` truthfully — the SDK
   enforces the DECLARED flags only) and `Tool::run()` (idempotent side
   effect). Register it in `main`. Everything else on the boundary
   (allowlist, gate, masking) keeps working unchanged.
3. **Add more tools or more handlers** — one `Worker` takes one handler;
   run several workers (one per task type) or dispatch inside `handle` on
   `task.task_type` like the reference handler does
   (`crates/ai-reference/src/handler.rs`).
4. **Add a model call** — see `docs/ai-reference-flows.md` for the
   provider adapter pattern (`ModelProvider`), `with_timeout` for hanging
   providers, and envelope `model`/`cost` fields.
5. **Long-running handlers** — wrap the work in `run_with_renewal` so a
   slow tool never loses its lease (`docs/worker-context.md`), and consider
   the `IdempotencyGuard` for dedup.
6. **Failing on purpose** — `TaskResult::Fail { category:
   Some(RetryCategory::Permanent) }` skips the retry loop (straight to
   Failed + dead letter); `None`/`Transient` retries with backoff.
   Tool-boundary errors already map correctly via `From<ToolError>`.

## Files

- `Cargo.toml` — standalone package; the only engine dependency is
  `bpm-engine-worker-sdk` (path).
- `src/main.rs` — the worker: tool, handler, wiring, ~180 commented lines.
