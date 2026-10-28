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

Verified at the REST level by `crates/server/rest/tests/scheduler_workers.rs` (memory driver, runs in CI) and `crates/server/rest/tests/scheduler_workers_postgres.rs` (PostgreSQL, Docker). Lease survival across a hard server kill and redelivery after restart are additionally proven by `crates/server/rest/tests/recovery_postgres.rs` (see §15).

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

## 15. CI 恢复验证 (CI Recovery Verification)

The `postgres-recovery` CI job starts a real PostgreSQL service and runs every
Docker/PostgreSQL-gated suite, including the crash-recovery suite
`crates/server/rest/tests/recovery_postgres.rs`, which kills the **real
`api_server` binary with SIGKILL** mid-flight and restarts a fresh process
against the same database. It proves:

1. **Kill/restart mid-flight** — an instance waiting on an external task
   survives a hard kill (still queryable, still `Running`, token still
   `Waiting`); the persisted lease survives too (no redelivery while valid);
   after the lease expires the SAME task is redelivered to a new worker
   (at-least-once), and completing it after the restart finishes the instance
   with exactly one completion in history.
2. **Timer continuation** — a PT2S timer catch scheduled before the kill is
   persisted (`timer` row, `Scheduled`); the restarted scheduler re-arms it
   and the instance completes through the timer (row becomes `Fired`).
3. **Completion durability** — an instance completed before the kill stays
   `Completed` after the restart: token set and full history are unchanged,
   and the completed task is never redelivered (no resurrection).

**Failure artifacts (失败时保留事件轨迹):** on any assertion failure the suite
dumps the instance state, token set, and full event history to stdout and to
`recovery-<test>-trail.log` under `CARGO_TARGET_TMPDIR`; each killed server
generation keeps its logs in `recovery-<test>-server<N>.{out,err}.log` next to
it. A one-shot panic hook is installed as a safety net for failure paths that
do not flow through the check/wait helpers (bare `.expect()`/assert panics):
it writes the trail header synchronously and fetches the instance + history
on a detached thread with a short deadline. The CI job uploads every
`recovery-*.log` on failure and the captured test log on every run.

**Run locally** (requires Docker):

```text
cargo test -p bpm-engine-server-rest --test recovery_postgres -- --ignored
```

CI mode against an existing PostgreSQL (no Docker needed):

```text
BPM_TEST_DATABASE_URL=postgres://bpm:bpm@localhost:5432/postgres \
    cargo test -p bpm-engine-server-rest --test recovery_postgres -- --ignored
```

Each test uses an isolated database (`bpm_rec_test_<name>`) so suites can run
in parallel against one server. The rest of the job runs the PostgreSQL
adapter integration/migration suites and the REST-level restart, scheduler
and lease suites (`storage_postgres_mode`, `scheduler_workers_postgres`) to
guard against interference.

---

> **A system that cannot recover is not a system.**
