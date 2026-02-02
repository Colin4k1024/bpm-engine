# Invariants

This document lists **formal invariants** that the engine enforces. These are design goals that tests and runtime logic are built to preserve.

---

## Runtime vs test-only invariants

- **Runtime invariants**: Enforced by the engine at runtime (e.g. token state machine, join count, external task single-owner, lock expiry). Violations are rejected or corrected by the runtime (e.g. CAS failure, ownership check).
- **Test-only invariants**: Verified by tests and replay (e.g. "replay produces the same final state as live execution", "under concurrency only one claim succeeds per token"). They are not asserted in production code; tests and CI fail if they are violated.

---

## 1. Token invariants

- **Exactly-once completion**: A token can only reach a final state (Completed, Terminated) once. It cannot be re-executed after completion.
- **Valid state transitions**: Only allowed transitions are enforced (e.g. Ready → Executing → Waiting or Completed). Invalid transitions (e.g. Completed → Executing) are rejected.
- **Single owner**: At most one executor owns a token at any time. The claim mechanism (CAS on token version/status) ensures that only one worker can advance a given token.
- **Idempotent transitions**: Where the spec allows it, repeating the same transition is safe (no double side effects).

See [Execution Model (Token & Concurrency)](docs_execution_model.md) for the token lifecycle and claim algorithm.

---

## 2. Join invariants

- **All branches required**: A parallel join node only produces a single outgoing token when **all** incoming branches have arrived. The expected count is fixed at compile time; the runtime tracks arrivals per join key (process instance + gateway).
- **No partial join**: Join state is updated atomically; a join either waits for more tokens or emits one and clears.

---

## 3. External task invariants

- **Exactly one owner**: An external task is either unclaimed (READY) or held by one worker (LOCKED). Fetch-and-lock and complete/fail all check ownership; a task cannot be completed by a different worker than the one that locked it.
- **Lease-bound**: Locks expire; expired tasks return to READY. The engine does not rely on in-memory state for ownership.
- **Retries monotonic**: Retry count only decreases; the engine decides when to retry or fail finally.

---

## 4. Timer and persistence invariants

- **Timers are persistent**: Timer state is stored in the persistence layer, not only in memory. After a crash, the scheduler can recover by re-reading timers.
- **Execution is crash-safe**: Progress is driven by persisted state. Re-running the scheduler after a restart continues from the last committed state.

See [Crash Recovery & Rehydration](docs_recovery.md) and [Database Schema](docs_database_schema.md).

---

## 5. Testing invariants

Tests are written to assert these invariants under concurrency and failure:

- **Token claim**: Concurrent workers; only one succeeds per token. See [Testing Strategy](docs_testing_strategy.md).
- **Join**: Multiple tokens arriving at a join; exactly one merged token proceeds when all have arrived.
- **External task**: Lock, complete, and fail only by the owning worker; idempotent complete/fail where specified.

Adding or changing behavior should preserve these invariants; new tests should be added when extending the engine.

---

## 6. Invariant violations in errors

When the runtime rejects an operation because it would violate an invariant, it returns an error that carries an **invariant violation kind** (`InvariantViolationKind`). This makes failures identifiable in logs, crash post-mortems, and issue reports, and supports future tooling (e.g. Execution Inspector UI).

- **TokenFinalizedTwice** — Token already in a final state (Completed/Terminated); corresponds to §1 (Exactly-once completion).
- **JoinIncomplete** — Parallel join advanced before all branches arrived; corresponds to §2 (All branches required).
- **ExternalTaskLeaseConflict** — Complete or fail called by a worker that does not hold the task lock; corresponds to §3 (Exactly one owner).
- **TokenInvalidTransition** — Invalid token state transition; corresponds to §1 (Valid state transitions).

REST API: when an external-task complete or fail call fails with an invariant violation, the response includes a `X-Invariant-Violation` header with the kind name (e.g. `ExternalTaskLeaseConflict`).
