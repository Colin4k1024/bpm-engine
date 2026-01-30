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

## 11. Relationship to Other Docs

- Execution semantics: `execution-model.md`
- Saga & compensation: `saga.md`
- Recovery behavior: `recovery.md`

---

> **Correct schema makes bugs impossible.**
