# Execution Model (Token & Concurrency)

> This document describes **how workflows are actually executed at runtime**.
> It defines the token lifecycle, concurrency rules, and transactional boundaries of the engine.

---

## 1. Core Idea

> **Token is the unit of execution.**

The engine does not execute a workflow as a call stack.
Instead, it advances the workflow by **moving tokens through a process graph**.

- One token = one execution authority
- Multiple tokens = parallel execution

---

## 2. Token Structure

A token represents the right to execute a node.

```
Token
 ├─ id
 ├─ instance_id
 ├─ node_id
 ├─ status
 ├─ mode (Forward / Compensation)
 ├─ parallel_group_id?
 ├─ version        // optimistic lock
 └─ timestamps
```

---

## 3. Token Lifecycle

### 3.1 States

```
Created → Ready → Executing → (Waiting) → Completed
                         ↘︎ Failed / Terminated
```

| State | Meaning |
|------|--------|
| Created | Token has been created but not scheduled |
| Ready | Token can be claimed by an executor |
| Executing | Token is currently executing a node |
| Waiting | Token is blocked (timer / user task) |
| Completed | Token finished its work |
| Terminated | Token will never be scheduled again |

---

## 4. Token Claim Mechanism

### 4.1 Why Claim Is Required

Without a claim step:
- Multiple executors may run the same token
- Side effects may be duplicated

---

### 4.2 Claim Algorithm (CAS)

A token can only be claimed if it is **Ready**.

```
UPDATE token
SET status = Executing,
    version = version + 1
WHERE id = ?
  AND status = Ready
  AND version = ?
```

- Success (1 row updated): claim acquired
- Failure (0 rows): token already taken

> Claim is the **concurrency gate** of the engine.

---

## 5. Transaction Boundary

> **One event handler = one database transaction**

### Handler Responsibilities

Within a single transaction, a handler may:

- Claim or update tokens
- Create or terminate tokens
- Persist timers / user tasks
- Persist outgoing events (Outbox)

A handler must **not**:

- Call other handlers directly
- Perform long-running IO

---

## 6. Parallel Execution Model

### 6.1 Fork

- One token arrives at a parallel gateway
- Engine creates N child tokens
- Parent token is completed

```
Token A → Fork → Token B + Token C + Token D
```

---

### 6.2 Parallel Group

All tokens created by a fork share the same `parallel_group_id`.

This group ID is used to coordinate joins.

---

### 6.3 Join

A join completes when **all tokens in the same parallel group** reach the join gateway.

To prevent duplicate joins:

```
parallel_join
 ├─ group_id (unique)
 └─ joined (bool)
```

Join algorithm:

```
UPDATE parallel_join
SET joined = true
WHERE group_id = ? AND joined = false
```

- Success → create next token
- Failure → join already completed

---

## 7. Waiting Tokens

A token enters `Waiting` when execution cannot continue immediately.

Typical reasons:

- Timer (delay / timeout / retry backoff)
- User task
- External signal

Waiting tokens are **not executable** until resumed by an event.

---

## 8. Failure Handling

### 8.1 Failure as Event

Failures are not exceptions.
They are emitted as events:

```
TokenFailed
```

---

### 8.2 Retry

- Retry does not create new tokens
- The same token is rescheduled
- Backoff is driven by timers

---

## 9. Idempotency & Safety

The execution model guarantees:

- A token is executed at most once at a time
- Parallel joins happen exactly once
- State transitions are atomic

This is achieved via:

- Optimistic locking (version)
- Unique constraints
- Event-driven progression

---

## 10. Relationship to Other Documents

- Saga & compensation behavior: `saga.md`
- Crash recovery & rehydration: `recovery.md`

---

> **If token execution is correct, the engine is correct.**

