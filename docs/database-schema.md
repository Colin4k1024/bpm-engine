---
layout: default
---

# Database Schema & Transaction Boundaries

> This document defines the **relational database schema** and **transactional rules** for the BPM engine.
> The schema is designed for **high concurrency, crash safety, and optimistic locking**.

---

## 1. Design Principles

- Database is the **source of truth**
- Optimistic locking over pessimistic locking
- Small, bounded transactions
- Explicit state transitions

---

## 2. process_instance

Represents a running process.

```
process_instance
-----------------------------
id (pk)
process_def_id
status            -- Running | Completed | Failed
version            -- optimistic lock
variables          -- TEXT: JSON map {name: value-string}
created_at
updated_at
```

Notes:
- Rarely updated after creation
- Not used for execution locking

### 2.1 `variables` column semantics (E2-3)

The variables map is stored as **one TEXT column of JSON `{name: value}`**
— the value for every name is a string in one of three forms, with no
per-value metadata or extra columns:

| Value form | Meaning |
|------------|---------|
| plain string | legacy string variable (forever valid) |
| `__encrypted:<b64 nonce>:<b64 ciphertext>` | AES-256-GCM ciphertext |
| `__var:1:<compact JSON>` | versioned envelope: JSON value or artifact reference |

The same semantics apply to the `variables` columns on `external_task` and
`dead_letter`. Envelope format, size caps, and the reserved `artifact://`
scheme are specified in `docs/variables-and-artifacts.md` (contract) and
`docs/adr/ADR-007-structured-variables-and-artifact-uris.md` (decision) —
no schema change was needed for E2-3.

---

## 3. token

Execution unit and concurrency boundary.

```
token
-----------------------------
id (pk)
instance_id (fk)
node_id
state             -- Ready | Executing | Waiting | Completed | Failed
mode              -- Forward | Compensation
attempt
version            -- optimistic lock
parallel_group_id
created_at
updated_at
```

Indexes:
- (state, updated_at)
- (parallel_group_id)
- (instance_id) — added in migration 0002

---

## 4. Token Claim (Critical Path)

Claiming a token is a **single atomic UPDATE**:

```
UPDATE token
SET state = 'Executing',
    version = version + 1
WHERE id = ?
  AND state = 'Ready'
  AND version = ?;
```

Rows affected = 1 → claim success.

---

## 5. event_outbox

Durable event publication.

```
event_outbox
-----------------------------
id (pk)
event_type
payload
status            -- Pending | Published
created_at
```

Guarantees at-least-once delivery.

---

## 6. timer

Persistent timers and delays.

```
timer
-----------------------------
id (pk)
token_id (fk)
due_at
status            -- Scheduled | Fired | Cancelled
created_at
```

Expired timers are safe to re-fire.

---

## 7. compensation_record

Immutable compensation facts.

```
compensation_record
-----------------------------
id (pk)
instance_id
node_id
handler_ref
order
status            -- Pending | Completed | Failed
created_at
```

Never updated except status.

---

## 8. parallel_join

Tracks fork/join completion.

```
parallel_join
-----------------------------
id (pk)
parallel_group_id
expected
joined
```

Join succeeds when `joined == expected`.

---

## 9. Transaction Boundaries

### 9.1 Token Execution Transaction

One transaction per event:

1. Claim token
2. Execute handler
3. Persist results
4. Emit events

No IO inside transaction.

---

### 9.2 Join Update Transaction

Join update must be atomic:

```
UPDATE parallel_join
SET joined = joined + 1
WHERE parallel_group_id = ?
  AND joined < expected;
```

---

## 10. Failure & Recovery Guarantees

- Executing tokens are safe to reset
- Duplicate events are acceptable
- Schema supports deterministic recovery

---

## 11. Storage Backend Configuration

The REST server (`bpm-engine-server-rest`) supports two storage backends,
selected via configuration (defaults < TOML file < `BPM__`-prefixed env vars):

| Key | Default | Values | Env var |
|-----|---------|--------|---------|
| `database.driver` | `"memory"` | `memory` \| `postgres` | `BPM_DATABASE__DRIVER` |
| `database.url` | *(empty)* | e.g. `postgres://user:pass@host:5432/bpm` | `BPM_DATABASE__URL` |
| `database.max_connections` | `10` | postgres pool size | `BPM_DATABASE__MAX_CONNECTIONS` |
| `database.migrate_on_start` | `true` | auto-apply pending migrations on startup | `BPM_DATABASE__MIGRATE_ON_START` |

**memory** (default): everything lives in process memory. The bootstrap
definitions `minimal` and `payment-flow` are registered at startup. State is
lost on restart — suitable for development and testing.

**postgres**: all state (instances, tokens, timers, external tasks, history,
definitions, dead letters) is persisted through the store traits in
`bpm-engine-adapter-postgres`. On startup the server:

1. creates a deadpool connection pool (`max_connections`),
2. applies the versioned SQL migrations (see section 12) — when
   `database.migrate_on_start` is `true` (default) it applies pending
   migrations; when `false` it validates the schema and refuses to start if
   migrations are pending (the deployment contract for change-managed
   environments),
3. wires every store over the shared pool and adds `db_pool*`,
   `db_migrations`, and `db_schema_version` readiness checks to `/ready`.

Restart semantics: starting a second server process against the same URL
re-runs migrations (a no-op when current) and resumes from persisted state —
running instances, waiting tokens, and deployed definitions are all still
queryable. Programmatic
bootstrap definitions are NOT persisted in postgres mode; deploy definitions
via `POST /api/v1/process-definitions/deploy` (the BPMN XML source is stored
and recompiled on load).

Routes only depend on the storage traits (`bpm-engine-storage`), so switching
backends is a pure configuration change. The `postgres` feature is enabled by
default; build with `--no-default-features` for a memory-only binary.

---

## 12. Migrations

The PostgreSQL schema is managed by **versioned, forward-only SQL migrations**.
Strategy and rationale are recorded in
`docs/adr/ADR-005-forward-only-sql-migrations.md` (forward-only; rollback =
restore from backup / PITR, never automatic down-migrations).

### File layout

```
crates/adapters/postgres/migrations/
├── 0001_init.sql             # initial schema (exact inline-migrate-era DDL)
└── 0002_query_indexes.sql    # hot-path indexes (process_instance.state, token.instance_id)
```

Rules:

- One migration per version, filename `NNNN_description.sql`, versions
  strictly ascending.
- Files are embedded at compile time (`include_str!`) — the binary has no
  runtime file dependency.
- Migrations are append-only: never edit an already-released file; add a new
  version instead.
- Keep each migration runnable in a transaction (no `CREATE INDEX
  CONCURRENTLY`, no `CREATE DATABASE`).

### Runner

`bpm_engine_adapter_postgres::migrations`:

- `run_migrations(pool) -> Vec<u32>` — applies pending migrations in version
  order, **each in its own transaction together with its bookkeeping row**,
  so a migration is either fully applied or not applied at all. Returns the
  versions applied by this call; a second run is a no-op (empty `Vec`).
- `migration_status(pool) -> MigrationStatus` — reports `applied`,
  `latest`, and `pending` versions without applying anything.
- `validate_up_to_date(&status)` — the startup contract for
  `migrate_on_start = false`: errors with the pending versions and how to
  enable auto-migration.
- `migrate(pool)` — backward-compatible thin wrapper around
  `run_migrations`.

The source of truth for what has been applied is the `schema_migrations`
table:

```
schema_migrations
-----------------------------
version (pk)      -- migration version number
name              -- e.g. "0002_query_indexes"
applied_at        -- ISO 8601 UTC timestamp
```

### Baseline stamping (upgrading pre-versioning databases)

Databases created by the legacy inline `migrate()` (E1-1 and earlier) contain
the v1 schema but no `schema_migrations` rows. When the runner finds that
shape — v1 tables present, no recorded versions — it **stamps** 0001 as
applied (recording it, not re-executing it) and continues with 0002+. An
empty database has every migration applied from scratch. Both paths are
covered by CI (the `migrations` job) and by the dual-mode tests in
`crates/adapters/postgres/tests/migrations.rs` (testcontainers locally,
`BPM_TEST_DATABASE_URL` service container in CI, isolated database per test).

### Health checks

`/ready` reports, in postgres mode:

- `db_migrations: "ok"` (or `degraded` with pending versions — should not
  happen, since startup validation rejects pending migrations),
- `db_schema_version: <latest applied version>` (e.g. `2`).

### deploy/schema.sql

`deploy/schema.sql` is now a **reference artifact** kept in sync with
migrations 0001 + 0002 (it no longer drives schema creation). A database
initialized from it is recognized by the runner as a pre-versioning schema
and baseline-stamped, converging to the same versioned state.

---

## 13. Relationship to Other Docs

- Execution semantics: `execution-model.md`
- Saga & compensation: `saga.md`
- Recovery behavior: `recovery.md`
- Migration strategy (ADR): `adr/ADR-005-forward-only-sql-migrations.md`
- Variable value contract (plain | `__encrypted:` | `__var:1:`): `variables-and-artifacts.md`

---

> **Correct schema makes bugs impossible.**
