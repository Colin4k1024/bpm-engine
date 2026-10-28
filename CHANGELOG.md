# Changelog

All notable changes to this project will be documented in this file.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **AI Task Envelope (E3-1)**: Typed execution metadata for AI-driven external tasks, layered on the E2-3 variables channel with zero storage/schema changes. New pure module `bpm_engine_core::ai_task`: `AiTaskEnvelope` (`schema_version` (only 1), `execution_id`, `idempotency_key`, `timeout_secs`, `retry_category` (`transient`/`permanent`), `model` `{provider,name,version}`, `tools`, `artifacts` (E2-3 URI rules), `input_schema`/`output_schema` JSON fragments; snake_case keys, `deny_unknown_fields`), `AiTaskEnvelope::validate/to_envelope_value/from_envelope_value/from_variables` with stable sub-codes (`ai_task_envelope_unsupported_version`, `ai_task_envelope_malformed`, `ai_task_envelope_invalid_artifact_ref`/`_model_ref`/`_tool_ref`/`_execution_id`/`_idempotency_key`/`_timeout`/`_schema`), and the `execution_id(task_id, attempt)` helper. The envelope travels as ONE variable under the reserved key `__task_envelope` (a `__var:1:` json envelope carrying the whole typed envelope), so the existing external-task API is untouched: `POST /external-tasks/:id/complete` gains an OPTIONAL typed `envelope` field (validated → 4xx `task_envelope_invalid` with the sub-code in the message, merged into the reserved key, E2-3 caps + history redaction then apply to the merged map; also accepted as the worker-sdk wire format — a serialized envelope value at the reserved key; junk there or dual-channel supply → 4xx `reserved_variable_key`; absent → byte-identical legacy behavior). `POST /external-tasks/:id/fail` gains OPTIONAL `category` (`transient`|`permanent`, else 4xx `invalid_retry_category`) and `envelope`: v1 records both for observability and does NOT change retry behavior (E3-2 scope) — the category is persisted as a structured prefix on the error message (`{"retry_category":…}: <error>`, surviving task row, `ExternalTaskFailed` history and the DLQ) and the envelope is recorded redacted in the `ExternalTaskFailed` history payload (the store's `fail()` has no variables channel and the trait is not extended — decision documented in `docs/ai-task-envelope.md` §8). `__task_envelope` is reserved at all variable write boundaries (start-instance, user-task complete: strict 4xx `reserved_variable_key`). New worker-sdk module `bpm_engine_worker_sdk::ai_task` (core dependency added): `TaskInput` (typed `plain`/`json`/`artifact`/`raw`/`envelope` access over fetched task variables), `TaskOutput` builder (mixed plain/json/artifact output variables + envelope, validated client-side in `build()`), `new_envelope`, `ModelRef`/`ToolRef`/`RetryCategory` re-exports, and `EngineClient::complete_with_envelope` (envelope serialized into the variables map before the existing complete call — same wire format). Core unit tests (round trips, every validation error, `from_variables` classification), worker-sdk unit tests including the full TaskOutput→variables→complete-body→core-parse round trip, and REST integration tests `crates/server/rest/tests/ai_task_envelope.rs` (memory mode): merge into task/instance/history, oversized-envelope history redaction, invalid-envelope rejection leaving the task completable, reserved-key guards at all three boundaries, dual-channel collision, fail-with-category+envelope recording with unchanged retry behavior, and the worker-sdk end-to-end typed round trip over a two-task chain. Docs: `docs/ai-task-envelope.md` (field table, execution-id derivation, idempotency-key guidance, storage visibility, fail-path decision, error codes, versioning), cross-referenced from `docs/variables-and-artifacts.md`.

- **Static Validation & Dry-Run (E2-2, issue #12)**: Pre-deploy safety gate for AI-agent-generated definitions. New pure functions in `bpm-engine-bpmn`: `validate_definition(&ProcessDefinition) -> Vec<ValidationFinding>` (stable snake_case codes — `unreachable_node`, `missing_default_branch`, `dead_branch_after_unconditional`, `join_in_cycle`, `cycle_detected`, `invalid_task_config`, `task_timeout_zero`, `duplicate_form_field`, `condition_variable_scope` — each with severity Critical/Warning/Info, never fails, boundary events counted reachable via their host) and `dry_run(def, variables, max_steps) -> DryRunReport` (side-effect-free walk: steps, terminus `completedAt`/`waitingAt`/`stuckAt`/`unresolvedBranch`/`maxStepsExceeded`, bounded by `DEFAULT_MAX_STEPS` = 100). Condition evaluation uses a documented lightweight subset (`condition_lite`): exact `lhs == "lit"` / `!=`, bare-variable truthiness, `VariableEq`; anything else is honestly reported as an unresolved branch instead of guessed (full EL stays in the runtime; dry-run is advisory). New REST endpoints `POST /api/v1/process-definitions/validate` and `POST /api/v1/process-definitions/dry-run` accepting `{ format: "bpmn"|"dsl", source, variables? }`: compile errors are returned as HTTP 200 data (`valid: false` + `compile_errors`, reusing each compiler's public DTO shapes), successful compiles report findings (`valid` = no Critical), and dry-run adds the walk report — nothing is deployed and no instance/task/timer/history is created (proven by the `validate_and_dry_run_have_no_side_effects` integration test; deploy behavior itself is unchanged — validation is an explicit pre-deploy call). 4xx is reserved for malformed envelopes (`invalid_format`, `empty_source`, `invalid_request_body`). Unit tests for every finding code and simulator semantics; REST integration tests in `crates/server/rest/tests/validation_dry_run.rs` (memory mode). Docs: `docs/validation-and-dry-run.md`.
- **Typed Rust Builder API (E4-3)**: New `bpm_engine_bpmn::builder` module (`ProcessBuilder`, `FormFieldSpec`) gives embedded Rust developers a fluent, validated entry point without hand-writing XML/JSON strings: `start_node`/`end_node`/`external_task`/`user_task`/`timer_catch`/`exclusive_gateway`/`parallel_gateway` nodes, `edge(...)` with `when_variable_eq`/`when_expression`/`is_default` conditions, and chained options (`.retries()`, `.timeout_secs()`, `.form_key()`, `.form_field(...)`) applying to the most recently added node/edge. It assembles the same `BpmnProcess` intermediate model the XML parser produces and calls `bpm_engine_bpmn::compile` verbatim (ADR-006): identical defaults (retries 3, timeout 60s, form label = field id), identical validation (`build()` returns the shared `CompileError`s with their stable `ErrorCode`s), and identical execution semantics — proven by a three-way parity test (builder == DSL JSON == BPMN XML compile identically, `tests/builder_parity.rs`) and end-to-end execution tests (`tests/builder_execution.rs`). New runnable example: `cargo run --example builder_process`. Docs: `docs/sdk-rust.md` ("Typed Process Builder"). Out of scope by design: the `NodeType::ServiceTask` closure variant and BPMN-only constructs (boundary events, subprocesses, ...).
- **API Surface Snapshot (version compatibility tests, E4-3)**: `tests/api_surface.rs` pins the `bpm-engine` facade's documented public surface — builder entry, DSL compile/schema, core model and event types, runtime engine/handlers, storage traits, memory adapter, worker SDK — through facade paths with `fn`-pointer signature checks, so renaming, removing or re-signaturing a public item breaks the build. The facade now also re-exports `bpm_engine_worker_sdk`, making `bpm-engine` the one-stop crate for both embedded engine users and worker authors. Semver for the facade is guarded by this test (additive changes update the snapshot; breaking changes require a major bump).
- **Structured Variables & Artifact URI Contract (E2-3)**: Process variables keep their `HashMap<String, String>` storage/wire form, with a self-describing versioned value format layered on top (ADR-007, `docs/variables-and-artifacts.md`). New `bpm_engine_core::variable_contract`: `__var:1:<compact JSON>` envelopes (`{"t":"json","val":…}` or `{"t":"artifact","uri":…,"size":…,"sha256":…,"media_type":…}`) with `parse_envelope` / `to_storage_string` round-trips; `validate_artifact_uri` (absolute scheme, no userinfo, no fragment, ≤ 2048 bytes, sha256 = 64 hex, `artifact://` reserved but opaque, engine never dereferences); unknown envelope versions (`__var:2:`+) are typed `UnsupportedVersion` and treated as opaque. REST write boundaries (start-instance, user-task complete, external-task complete) enforce `engine.variable_max_bytes` (default 65_536, env `BPM_ENGINE__VARIABLE_MAX_BYTES`) with stable 4xx codes `variable_too_large` / `variable_invalid_envelope` / `variable_invalid_artifact_uri` on the existing error body plus a new optional `code` field. `StartInstanceRequest` gains `sensitive`: every sensitive name must exist in the variables and be listed in `encrypted` (else `sensitive_requires_encryption`), so history and logs only ever hold `__encrypted:` ciphertext. Event history is redacted by default via the new pure `redact_history_variable_value` + `EngineContext::history_redaction_policy` (default 1024-byte cap, `engine.history_variable_max_bytes`): oversized values become `__truncated:<len>:<sha256-12-hex>:` markers, unknown versions become `__opaque:` — instance variables themselves are never truncated, and existing history rows are not rewritten. Plain strings, `__encrypted:` values, gateway conditions, and fetch-and-lock delivery to workers are unchanged; no core struct or SQL changes (no object-storage adapter this round — explicit non-goal). Memory-mode REST integration tests (`crates/server/rest/tests/variable_contract.rs`) and a Docker PostgreSQL variant (`variable_contract_postgres.rs`) cover envelopes, gateway routing on plain variables, every URI rejection, caps at all three boundaries, sensitive+encrypted history ciphertext, redaction markers with correct lengths, and verbatim worker delivery.
- **PostgreSQL CI Recovery Verification (E1-4)**: New crash-recovery suite `crates/server/rest/tests/recovery_postgres.rs` that kills the real `api_server` binary (SIGKILL, spawned via `CARGO_BIN_EXE_api_server` with a generated `BPM_CONFIG_PATH` TOML on a pinned free port) mid-flight and restarts a fresh process against the same database. Proves: an instance with a locked external task survives the hard kill (lease persisted — no redelivery while valid — and the SAME task redelivered to a new worker after lease expiry, completing the instance); a PT2S timer catch scheduled before the kill fires after the restart (persisted timer re-armed by the restarted scheduler); and a pre-kill completion is durable (still Completed, token set and history unchanged, no task resurrection). Dual-mode like the migration tests (`BPM_TEST_DATABASE_URL` or testcontainers; isolated `bpm_rec_test_*` database per test). On any failure (assertion paths via the check/wait helpers, plus a one-shot panic-hook safety net covering bare `.expect()`/assert panics) the suite dumps the instance state, tokens and full event history to stdout and to `recovery-<test>-trail.log` under `CARGO_TARGET_TMPDIR`, with each killed server generation's logs kept in `recovery-<test>-server<N>.{out,err}.log`. CI gains a `postgres-recovery` job (postgres:16 service) running all PostgreSQL-gated suites (adapter `integration` + `migrations`, REST `storage_postgres_mode` + `scheduler_workers_postgres` + `recovery_postgres`), uploading the `recovery-*.log` event trails on failure and the captured test log on every run. `docs/recovery.md` §15 documents what the job proves and how to run it locally.
- **Minimal Declarative DSL (E2-1)**: New `bpm-engine-dsl` crate compiles a restricted JSON DSL (nodes: start/end/serviceTask/userTask/exclusiveGateway/parallelGateway/timerIntermediateCatch; edges with `variableEq`/`expression` conditions and a `default` flag) to `ProcessDefinition`. It lowers onto the BPMN intermediate model and reuses `bpm_engine_bpmn::compile` verbatim, so DSL and BPMN XML entries share one validation/execution semantics and compile identically (parity-enforced by tests). Invalid documents return a `DslCompileReport` collecting all errors with stable snake_case codes and JSON-pointer paths (e.g. `/nodes/0/taskTypo`). A `schemars`-generated JSON Schema (`schema_json()`, committed at `crates/dsl/schema/bpm-dsl-v1.json`, kept in lockstep by test) publishes the format for AI agents. Docs: `docs/dsl-guide.md`, ADR-006.
- **Versioned SQL Migrations (E1-2)**: The PostgreSQL schema is now managed by forward-only versioned migrations in `crates/adapters/postgres/migrations/` (embedded at compile time), applied by a new runner in the adapter: `run_migrations` (per-migration transactions, idempotent), `migration_status` (applied/latest/pending for health and validation), and baseline stamping that upgrades pre-versioning (inline-migrate era) databases without re-executing 0001 or losing data. `migrate()` remains as a thin backward-compatible wrapper. Migration 0002 adds hot-path indexes (`process_instance(state)`, `token(instance_id)`); strategy and rollback policy are recorded in ADR-005 (forward-only; rollback = restore from backup/PITR). CI gains a `migrations` job validating empty-database and legacy-upgrade paths against a postgres:16 service container (tests run dual-mode: testcontainers locally, `BPM_TEST_DATABASE_URL` in CI, isolated database per test).
- **Migration Deployment Contract**: New `database.migrate_on_start` config (default `true`, env `BPM_DATABASE__MIGRATE_ON_START`, TOML `[database] migrate_on_start`). When disabled, the server validates the schema at startup and fails fast naming any pending migrations instead of running against a stale schema. `/ready` now reports `db_migrations` (`ok`/pending-degraded) and `db_schema_version` in postgres mode.
- **Scheduler & Worker Lease Verification (E1-3)**: REST-level integration tests over a single running service with two workers — dual-worker single delivery under concurrent fetch (exactly one winner), duplicate completion rejected with 4xx and no double advance, expired-lease reclaim + redelivery with the previous owner's late complete rejected, timer intermediate catch firing end-to-end through the timer scheduler + drain task, and duplicate `TimerFired` injection proving the token is not double-advanced. Memory mode (`crates/server/rest/tests/scheduler_workers.rs`, CI) and PostgreSQL mode (`scheduler_workers_postgres.rs`, Docker).
- **At-Least-Once Side-Effect Semantics Documentation**: `docs/recovery.md` §8 documents the lease/at-least-once contract for external task side effects (single owner while locked, expiry → redelivery, duplicate completion 4xx, workers must be idempotent, `retry_after` does not enforce a fetch backoff).
- **PostgreSQL Adapter Tests**: `external_task_store_complete_expiry_guard_and_epoch_updated_at` — complete rejects an expired (unreclaimed) lease with `LockExpired`, rejects a non-owner with a lease conflict, and `updated_at` round-trips as TEXT unix-epoch seconds after complete/fail.
- **Storage Backend Configuration**: REST server selects memory or PostgreSQL via `database.driver` (default `memory`, env `BPM_DATABASE__DRIVER`). Postgres mode creates a pool (`database.max_connections`), runs idempotent migrations on start, and wires all 10 stores via the new `Storage` bundle; state (instances, tokens, definitions) survives restarts. `/ready` gains `db_pool*` checks in postgres mode. The server crate is now lib + bin (`build_app`/`serve`) and routes depend only on storage traits.
- **Storage Trait Extensions**: `ProcessInstanceStore::list_all` (all states, tenant-filtered), `ProcessDefinitionStore::list_all` (every version), new `DefinitionDeployer` trait (persist BPMN XML source), and a backend-agnostic `StoreInvariantChecker` in `bpm-engine-storage`. All adapters implement them.
- **Storage Mode Integration Tests**: Memory-mode HTTP round-trip (bootstrap defs, deploy, start, invariants) and a Docker-based PostgreSQL restart acceptance test (instance/token/definition survive a full server restart), plus config-validation tests.
- **Timer Scheduler**: Background `tokio::spawn` task polls `TimerStore::list_due()` and fires `TimerFired` events with crash recovery on startup (`crates/runtime/src/timer_scheduler.rs`).
- **Observability**: Prometheus metrics endpoint at `GET /metrics` (feature-gated: `--features observability`). 9 metrics covering process lifecycle, tokens, timers, external tasks, and event pump duration (`src/metrics.rs`).
- **Dead Letter Queue**: Failed external tasks after retry exhaustion moved to DLQ. REST endpoints for listing, inspecting, requeuing, and deleting dead letters.
- **Lock Extension**: `extend_lock` API for long-running external tasks in REST, Rust SDK, and Python SDK.
- **PostgreSQL Integration Tests**: 9 testcontainer-based tests covering TokenStore, ProcessDefinitionStore, ProcessInstanceStore, TimerStore, ExternalTaskStore, and HistoryRepo.
- **Parallel Saga Compensation Tests**: Multi-branch compensation ordering and failure halt behavior tests.
- **API Documentation**: All public items in `core`, `storage`, and `runtime` have `///` doc comments. `#![warn(missing_docs)]` enforced (235 missing docs resolved).
- **Python Worker SDK**: Full worker lifecycle — `Client`, `Worker`, `TaskHandler`, `TaskContext` with `complete()`, `fail()`, `extend_lock()`.
- **ExternalTaskCompletedHandler**: External task completion now goes through the engine event loop (previously bypassed it with manual token manipulation). Ensures consistent event-driven architecture.

### Changed

- **Schema Management**: The inline `CREATE TABLE IF NOT EXISTS` DDL in the Postgres adapter's `migrate()` was replaced by the versioned migration runner (ADR-005); `deploy/schema.sql` is now a reference artifact kept in sync with the migration files (0001 + 0002) rather than the schema source of truth.
- **EngineContext**: Fields changed from `Option<Arc<dyn T>>` to `Arc<dyn T>` with `EngineContextBuilder`. Eliminates runtime `unwrap()` risk.
- **Schema Timestamps**: Unified to TEXT (ISO 8601 UTC) across `deploy/schema.sql` and Postgres adapter `migrate()`.
- **Postgres Adapter**: All 10 store traits implemented (was 7/10). Added `ProcessDefinitionStore` with `deploy()`.
- **ExternalTaskStore Error Handling**: `complete()` and `fail()` now return typed `ExternalTaskError` instead of `anyhow::Error`. Internal errors return generic messages to API clients.

### Fixed

- **EL boolean splitting inside quoted literals (E2-1 review finding)**: `eval_condition` split on ` or ` / ` and ` before checking whether the separator was inside a single/double-quoted string literal, so conditions like `x == "salt and pepper"` silently mis-evaluated (splitting the quoted literal into boolean operands). Splitting is now quote-aware (`split_outside_quotes`, with regression tests). This affects both BPMN `<conditionExpression>` and DSL `variableEq` lowering, restoring the exact-string-equality equivalence claimed by ADR-006.
- **Timer Intermediate Catch Misfire (E1-3 finding)**: `TokenArrivedHandler` marked the token `Waiting` at a `timerIntermediateCatchEvent` but never persisted a `TimerRecord`, so instances waiting on a timer catch never advanced (the scheduler had nothing to fire). The handler now schedules the persistent timer (ADR-004) when the token arrives, using the same `list_due`/`TimerFired` path as boundary timers.
- **PostgreSQL `ExternalTaskStore::complete` lock-expiry gap (E1-3 finding)**: unlike the memory adapter, complete did not check `lock_expire_at`, so a worker whose lease had expired (but was not yet reclaimed) could still complete the task. It now runs in a transaction (`SELECT ... FOR UPDATE`) and returns the same typed errors as memory (`TaskNotFound` / `TaskNotLocked` / `ExternalTaskLeaseConflict` / `LockExpired`), all mapped to HTTP 400.
- **PostgreSQL `external_task.updated_at` mixed formats**: `complete`/`fail` wrote `CURRENT_TIMESTAMP` into the TEXT column while `create`/`extend_lock` wrote unix-epoch seconds. Unified on TEXT unix-epoch seconds.
- **Postgres migrate()**: `idx_token_state_updated` referenced the nonexistent `token(state)` column (the column is `status`), so every migration failed; all postgres integration tests were blocked by this.
- **Postgres ProcessStore::load**: SELECT did not include `parent_instance_id`/`parent_token_id` but read them, panicking on every load.
- **Postgres ExternalTaskStore**: parameter type mismatches (`i64` vs `int4`) and mixed timestamp conventions (`to_timestamp`/`CURRENT_TIMESTAMP` vs TEXT epoch seconds) broke create/fetch-and-lock/reclaim; unified on TEXT unix-epoch seconds like the memory adapter.

### Removed

- **Legacy Code**: Removed `src/legacy_engine.rs` and 46 legacy files. Root crate is now a pure re-export facade.
- **rusqlite Dependency**: Removed from workspace.

## [0.2.0] - 2026-01-30

### Added

- **GitHub Pages Documentation Site**: Complete bilingual (EN/ZH) documentation website
- **PostgreSQL Adapter**: Production-ready persistence layer (`bpm-adapter-postgres`)
- **Worker SDK**: External task worker runtime with lease-based execution
- **BPMN 2.0 Parser**: XML to ProcessDefinition compiler
- **Formal Invariants**: Mathematically proven correctness guarantees
- **Crash Recovery**: Deterministic recovery from failures via event replay
- **Persistent Timers**: Timer state survives restarts
- **Saga Compensation**: Built-in compensation pattern support

### Changed

- Improved workspace organization with clear crate boundaries
- Enhanced error handling with `thiserror` and `anyhow`
- Updated dependencies to latest stable versions

### Documentation

- Complete architecture documentation
- Execution model specification
- Database schema reference
- API specification with OpenAPI
- Quick start guides (EN/ZH)
- FAQ and cheat sheet

---

## [0.1.0] - First formal commitment

**What is stable**

- Process-instances, history, trace, and external-task APIs, and their semantics (docs/api-spec.md §8, §9).
- History/Trace: append-only events, globally ordered per instance, deterministic replay, persistence-first.
- Invariant violation response: `X-Invariant-Violation` header and error body contract.

**What is NOT stable / may evolve**

- Replay API (session/step/seek), UI/Inspector, SDK helper interfaces, and future Python SDK may be extended or adjusted; they do not affect the stable set above.

**What users can rely on**

- The stable APIs and semantics above will not receive breaking changes within minor/patch versions.
- History is auditable and replayable; causal order and persistence-first are design commitments.

---

### Added (v0.1.0)

- Defined and frozen History / Trace semantic guarantees (docs/api-spec.md).
- Workspace layout: `bpm-core`, `bpm-storage`, `bpm-runtime`, `bpm-adapter-memory`, `bpm-server-rest` crates (refactor.md).
- Async storage traits in `bpm-storage`; memory adapter and unified `MemoryRepo` in `bpm-adapter-memory`.
- Async event pump and handlers in `bpm-runtime`; REST server in `bpm-server-rest`.

### Changed (v0.1.0)

- Root package re-exports workspace crates; main binary moved to `bpm-server-rest` (`cargo run -p bpm-server-rest`).
