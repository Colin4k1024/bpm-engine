# Changelog

All notable changes to this project will be documented in this file.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

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
