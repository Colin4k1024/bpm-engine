---
layout: default
---

# Recovery & Rehydration Model

> This document describes how the engine **recovers from crashes, restarts, and partial failures**.
> Recovery is a **first-class capability**, not an edge case.

---

## 1. Failure Is Assumed

The engine assumes:

- Process crashes
- Node restarts
- Network partitions
- Partial transaction commits

> **If it can fail, it eventually will.**

---

## 2. Design Philosophy

Recovery is built on three principles:

1. Persist facts, not intentions
2. Make execution resumable
3. Never guess state

---

## 3. Persisted Runtime State

The following entities are fully persisted:

- ProcessInstance
- Token
- Event (outbox)
- Timer
- CompensationRecord

Memory contains **no critical state**.

---

## 4. Token States During Failure

At crash time, tokens may be in any state:

- Ready
- Executing
- Waiting
- Completed
- Failed

Only `Executing` requires special handling.

---

## 5. Executing Token Recovery

### 5.1 The Problem

A token marked as `Executing` may have:

- Completed handler logic
- Not completed handler logic
- Partially completed side effects

The engine must assume **the worst**.

---

### 5.2 Safe Rule

> **Executing tokens are always considered incomplete.**

They are never assumed successful.

---

### 5.3 Recovery Strategy

On engine startup:

1. Scan tokens in `Executing`
2. Reset them to `Ready`
3. Increment attempt counter
4. Re-dispatch execution

This is safe because:

- Handlers must be idempotent
- Side effects must be protected

---

## 6. Idempotency Contract

All handlers must obey:

- At-least-once execution
- External side effects must be idempotent
- Engine does not guarantee exactly-once

---

## 7. Outbox Recovery

Events are persisted before dispatch.

On restart:

- Undelivered events are re-published
- Duplicate delivery is allowed
- Consumers must be idempotent

---

## 8. External Task Side-Effect Semantics (At-Least-Once)

External tasks delegate work to external workers under a lease. The engine guarantees **exactly-once state transitions** but only **at-least-once side-effect execution**.

### 8.1 Lease ownership

- While a task is `LOCKED`, exactly one worker owns it
- `fetch_and_lock` is atomic: the memory adapter locks in a single write-lock critical section (ADR-001); the PostgreSQL adapter uses a transaction with `FOR UPDATE SKIP LOCKED`
- `complete` / `fail` verify state, owner, and (for `complete`) lease expiry

### 8.2 Lease expiry

- Locks expire; `lock_expire_at` is persisted (TEXT unix-epoch seconds)
- An expired task is reclaimed to `READY` on the next fetch sweep and **redelivered to another worker**
- The original owner's late `complete` is rejected (`LockExpired` / lease-conflict; HTTP 400)

### 8.3 Duplicate completion

- A duplicate `complete` is rejected by the store (the task is no longer `LOCKED`)
- The REST layer maps this to a 4xx — the rejected request never reaches the engine event loop
- Token transitions therefore happen exactly once (see `invariants.md` §1 and §3)

### 8.4 Worker crash mid-execution

- A crashed worker never completes; its lease expires; the task is redelivered
- The same side effect may execute **more than once**
- Workers must be idempotent — use the `task_id` (or a business key from variables) as an idempotency key; see `crates/worker-sdk/examples/idempotency.rs`

### 8.5 Retry backoff (current limitation)

`fail(retry_after)` resets the task to `READY` with a future `lock_expire_at`, but `READY` tasks are immediately fetchable: the backoff is **not enforced**. Do not rely on `retry_after` to delay re-execution.

### 8.6 The boundary

> The engine guarantees token transitions happen exactly once. Side effects in workers may execute more than once.

Verified at the REST level by `crates/server/rest/tests/scheduler_workers.rs` (memory driver, runs in CI) and `crates/server/rest/tests/scheduler_workers_postgres.rs` (PostgreSQL, Docker).

Cross-references: `adr/ADR-001-fetch-and-lock-race.md` (fetch-and-lock race), `invariants.md` (external task invariants).

---

## 9. Timer Recovery

Timers are persisted with:

- due_at
- status

On recovery:

- Expired timers are immediately fired
- Pending timers are rescheduled

---

## 10. Saga Recovery

Saga recovery relies on facts:

- CompensationRecord is immutable
- Completed compensations are never repeated
- Failed compensations remain failed

Saga resumes deterministically.

---

## 11. Rehydration Process

Rehydration steps:

1. Load all runtime entities
2. Restore in-memory indexes
3. Resume schedulers
4. Resume token dispatch

---

## 12. What Recovery Never Does

Recovery does **not**:

- Infer missing compensation
- Skip failed steps
- Assume success

If state is unclear, the engine chooses **safety over progress**.

---

## 13. Operational Guarantees

This model guarantees:

- No lost tokens
- No phantom success
- Deterministic restart behavior

---

## 14. Relationship to Other Documents

- Execution & concurrency: `execution-model.md`
- Saga & compensation: `saga.md`
- External task invariants: `invariants.md` §3
- Fetch-and-lock race fix: `adr/ADR-001-fetch-and-lock-race.md`

---

> **A system that cannot recover is not a system.**
