# Testing Strategy (Concurrency, Crash, Saga)

> This document defines **how the BPM engine is tested**.
> Tests are treated as part of the architecture, not an afterthought.

---

## 1. Testing Philosophy

The engine is tested against **failure**, not happy paths.

Core principles:

- Assume concurrency
- Assume crashes
- Assume duplicate execution

> If it passes these tests, it survives production.

---

## 2. Test Pyramid

```
Unit Tests        → engine-core
Integration Tests → engine-runtime
Chaos Tests       → full engine
```

---

## 3. engine-core Tests (Pure Logic)

### 3.1 Token State Machine

Test cases:

- Valid state transitions
- Invalid transitions rejected
- Idempotent transitions

Example:

- Ready → Executing ✅
- Completed → Executing ❌

---

### 3.2 Saga Ordering

Given compensation records:

```
A(order=1), B(order=2), C(order=3)
```

Expected compensation order:

```
C → B → A
```

No IO, pure deterministic test.

---

## 4. engine-runtime Integration Tests

### 4.1 Token Claim Concurrency Test

Goal:

- Ensure only one worker executes a token

Setup:

- 1 Ready token
- N concurrent workers

Assert:

- Exactly one handler executed
- Token ends in Completed

---

### 4.2 Parallel Join Idempotency

Setup:

- Parallel fork creating 3 tokens
- All reach join concurrently

Assert:

- Join fires exactly once
- Only one downstream token created

---

### 4.3 Retry & Timer Test

Setup:

- Handler fails twice
- Retry policy with backoff

Assert:

- Same token retried
- Delay respected
- Final success

---

## 5. Crash Recovery Tests

### 5.1 Crash During Execution

Simulate:

- Token claimed
- Engine crashes before completion

Recovery:

- Restart engine
- Reset Executing → Ready

Assert:

- Handler re-executed
- No duplicate side effects

---

### 5.2 Crash After Event Persisted

Simulate:

- Event written to outbox
- Crash before publish

Assert:

- Event published after restart
- Duplicate allowed

---

## 6. Saga Tests

### 6.1 Failure Triggers Saga

Setup:

- A → B → C
- C fails

Assert:

- Compensation C, B, A executed
- Correct order

---

### 6.2 Parallel Saga Test

Setup:

- Parallel A/B
- B fails

Assert:

- Compensate A only if completed
- No phantom compensation

---

## 7. Chaos Testing

### 7.1 Random Kill Test

Loop:

- Start engine
- Randomly kill process
- Restart

Assert:

- Eventually completes or fails deterministically

---

## 8. In-Memory vs Real DB

- In-memory store for fast tests
- Real Postgres for concurrency tests

Both must pass.

---

## 9. Non-Goals

Tests do not assert:

- Exact timing
- Ordering across independent tokens

---

## 10. Relationship to Other Docs

- Execution semantics: `execution-model.md`
- Recovery behavior: `recovery.md`
- Saga model: `saga.md`

---

> **If it can’t be tested, it doesn’t exist.**
