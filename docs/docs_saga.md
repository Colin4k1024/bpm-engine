# Saga & Compensation Model

> This document describes how the engine handles **long-running transactions** using **Saga-based compensation**.
> Saga is a first-class execution model, not an afterthought.

---

## 1. Why Saga Exists

Traditional database transactions cannot span:

- Multiple services
- Long-running operations
- Human interactions

In BPM systems, failures may occur **after some steps have already succeeded**.

> **Saga trades immediate consistency for eventual consistency.**

---

## 2. Core Principle

> **Saga is not rollback.**

- Rollback assumes atomicity
- Saga assumes partial success

Saga compensates **only what has actually happened**.

---

## 3. What Is Compensated

Only steps that:

- Have completed successfully
- Explicitly define a compensation action

are eligible for compensation.

---

## 4. Compensation as Data (CompensationRecord)

Compensation is driven by facts, not assumptions.

```
CompensationRecord
 ├─ instance_id
 ├─ node_id
 ├─ compensate_fn
 ├─ order
 └─ status
```

A record is written **only after** a step succeeds.

---

## 5. When Saga Starts

Saga is triggered when:

- A token fails
- Retry policy is exhausted
- Saga scope is enabled

This emits:

```
SagaStarted
```

---

## 6. Compensation Tokens

### 6.1 Dedicated Execution Mode

Compensation never reuses forward tokens.

```
TokenMode::Compensation
```

Each compensation step runs as its own token.

---

### 6.2 Execution Order

Compensation order is the **reverse** of successful execution order.

```
A → B → C (fail)
↓
Compensate C → B → A
```

---

## 7. Compensation Token Lifecycle

```
Ready → Executing → Completed / Failed
```

Rules:

- Compensation does not retry by default
- A failed compensation does not trigger further compensation

---

## 8. Saga Scope

Saga scope defines how far compensation propagates.

- Local: current subprocess only
- Global: entire process instance

---

## 9. Parallel Execution & Saga

### 9.1 Parallel Branches

- Each branch records its own compensation records
- Compensation includes **all successfully completed branches**

---

### 9.2 Join Behavior

- Unreached join paths are not compensated
- Only completed tokens participate in Saga

---

## 10. Persistence & Recovery

Saga state is fully persisted.

- Compensation records are immutable facts
- Recovery never invents compensation
- Saga resumes safely after crashes

---

## 11. Failure Semantics

A failed compensation:

- Marks the record as Failed
- Does not rollback other compensations
- Requires manual intervention if needed

---

## 12. Design Guarantees

This model guarantees:

- No double compensation
- Deterministic compensation order
- Compatibility with crash recovery

---

## 13. Relationship to Other Documents

- Token execution & concurrency: `execution-model.md`
- Crash recovery & rehydration: `recovery.md`

---

> **Saga makes failure survivable.**
