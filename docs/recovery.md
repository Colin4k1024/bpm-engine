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

## 8. Timer Recovery

Timers are persisted with:

- due_at
- status

On recovery:

- Expired timers are immediately fired
- Pending timers are rescheduled

---

## 9. Saga Recovery

Saga recovery relies on facts:

- CompensationRecord is immutable
- Completed compensations are never repeated
- Failed compensations remain failed

Saga resumes deterministically.

---

## 10. Rehydration Process

Rehydration steps:

1. Load all runtime entities
2. Restore in-memory indexes
3. Resume schedulers
4. Resume token dispatch

---

## 11. What Recovery Never Does

Recovery does **not**:

- Infer missing compensation
- Skip failed steps
- Assume success

If state is unclear, the engine chooses **safety over progress**.

---

## 12. Operational Guarantees

This model guarantees:

- No lost tokens
- No phantom success
- Deterministic restart behavior

---

## 13. Relationship to Other Documents

- Execution & concurrency: `execution-model.md`
- Saga & compensation: `saga.md`

---

> **A system that cannot recover is not a system.**
