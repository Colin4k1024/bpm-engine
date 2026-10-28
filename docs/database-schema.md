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
created_at
updated_at
```

Notes:
- Rarely updated after creation
- Not used for execution locking

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

**memory** (default): everything lives in process memory. The bootstrap
definitions `minimal` and `payment-flow` are registered at startup. State is
lost on restart — suitable for development and testing.

**postgres**: all state (instances, tokens, timers, external tasks, history,
definitions, dead letters) is persisted through the store traits in
`bpm-engine-adapter-postgres`. On startup the server:

1. creates a deadpool connection pool (`max_connections`),
2. runs `migrate()` — idempotent `CREATE TABLE IF NOT EXISTS` DDL, safe on every start,
3. wires every store over the shared pool and adds `db_pool*` readiness checks to `/ready`.

Restart semantics: starting a second server process against the same URL
re-runs migrations and resumes from persisted state — running instances,
waiting tokens, and deployed definitions are all still queryable. Programmatic
bootstrap definitions are NOT persisted in postgres mode; deploy definitions
via `POST /api/v1/process-definitions/deploy` (the BPMN XML source is stored
and recompiled on load).

Routes only depend on the storage traits (`bpm-engine-storage`), so switching
backends is a pure configuration change. The `postgres` feature is enabled by
default; build with `--no-default-features` for a memory-only binary.

---

## 12. Relationship to Other Docs

- Execution semantics: `execution-model.md`
- Saga & compensation: `saga.md`
- Recovery behavior: `recovery.md`

---

> **Correct schema makes bugs impossible.**
