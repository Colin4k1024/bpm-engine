---
layout: default
---

# REST & gRPC API Specification (Draft)

> This document defines the **external API contract** of the BPM Engine.
> The APIs are designed for **service usage**, **external workers**, and **operational tooling**.

---

## 1. Design Principles

- Engine is a **service**, not just a library
- APIs are **stateless** and **idempotent** where possible
- REST for management & integration
- gRPC for high-throughput workers

---

## 2. Core Resources

| Resource | Description |
|--------|-------------|
| ProcessDefinition | Deployed workflow definition |
| ProcessInstance | Running workflow instance |
| Token | Execution unit (internal) |
| Task | Executable node (user / external) |
| Event | Engine event (outbox) |

---

## 3. REST API

Base URL:

```
/api/v1
```

---

### 3.1 Process Definition

#### Deploy a Process

```
POST /process-definitions
```

Request:

```json
{
  "id": "order_process",
  "version": "1.0.0",
  "definition": { /* DSL / BPMN JSON */ }
}
```

Response:

```json
{
  "id": "order_process",
  "version": "1.0.0",
  "status": "DEPLOYED"
}
```

---

### 3.2 Process Instance

#### Start a Process Instance

```
POST /process-instances
```

Request:

```json
{
  "process_def_id": "order_process",
  "variables": {
    "order_id": "123"
  }
}
```

Response:

```json
{
  "instance_id": "pi_abc123",
  "status": "RUNNING"
}
```

---

#### Get Instance State

```
GET /process-instances/{instance_id}
```

Response:

```json
{
  "instance_id": "pi_abc123",
  "status": "RUNNING",
  "current_nodes": ["task_a", "task_b"]
}
```

---

#### Get Instance Execution History

```
GET /process-instances/{instance_id}/history
```

Query (optional):

- `token_id` — filter events for a single token
- `event_type` — filter by event type (e.g. `ProcessStarted`, `TokenArrived`, `ExternalTaskLocked`)

Response:

```json
{
  "instance_id": "pi_abc123",
  "events": [
    {
      "sequence": 0,
      "id": "event-uuid",
      "event_type": "ProcessStarted",
      "category": "instance",
      "occurred_at": "1738454400",
      "payload": { "instance_id": "pi_abc123", "process_id": "order_process", ... }
    },
    {
      "sequence": 1,
      "id": "event-uuid-2",
      "event_type": "TokenArrived",
      "category": "token",
      "occurred_at": "1738454401",
      "payload": { "instance_id": "pi_abc123", "token_id": "...", "node_id": "task_a" }
    }
  ]
}
```

- **sequence**: Zero-based index; events are in causal order (by `occurred_at`, then `id`). Use for replay and debug.
- **category**: `instance` (ProcessStarted, ProcessCompleted), `token` (Token*, UserTask*, Timer*, Saga*), or `external` (ExternalTaskLocked, ExternalTaskCompleted, ExternalTaskFailed).
- Same event source as replay; use for auditing and debugging.

**History API Semantics**

- History events are **append-only**.
- Sequence is **globally ordered per process instance** (by `occurred_at`, then `id`).
- Replaying history must produce the same token state graph as live execution.
- History response schema is **backward-compatible** once released.

---

#### Get Instance Trace (aggregated view)

```
GET /process-instances/{instance_id}/trace
```

Returns instance state plus token timelines and external-task history aggregated by token/task. Use for a high-level view; use `/history` for the raw event timeline with sequence and category. Trace and History use the same event source; Trace is an aggregated view and shares the same semantics as History.

---

### 3.3 Tasks

#### List Pending Tasks

```
GET /tasks?type=user|external
```

Response:

```json
[
  {
    "task_id": "task_1",
    "node_id": "approve_order",
    "instance_id": "pi_abc123",
    "type": "user"
  }
]
```

---

#### Complete Task

```
POST /tasks/{task_id}/complete
```

Request:

```json
{
  "variables": {
    "approved": true
  }
}
```

Response:

```json
{
  "status": "COMPLETED"
}
```

---

### 3.4 Signals & Messages

#### Send Signal

```
POST /signals
```

Request:

```json
{
  "signal": "payment_received",
  "instance_id": "pi_abc123",
  "payload": {}
}
```

---

### 3.5 Operations

#### Cancel Instance

```
POST /process-instances/{id}/cancel
```

---

#### Retry Token

```
POST /tokens/{token_id}/retry
```

---

## 4. gRPC API (External Worker)

### 4.1 Design Goal

- High throughput
- Low latency
- Pull-based task execution

---

### 4.2 Service Definition

```proto
service ExternalTaskService {
  rpc FetchTasks(FetchRequest) returns (stream Task);
  rpc CompleteTask(CompleteRequest) returns (CompleteResponse);
  rpc FailTask(FailRequest) returns (FailResponse);
}
```

---

### 4.3 Fetch Tasks

```proto
message FetchRequest {
  string worker_id = 1;
  repeated string task_types = 2;
  int32 max_tasks = 3;
}
```

---

### 4.4 Complete Task

```proto
message CompleteRequest {
  string task_id = 1;
  map<string, string> variables = 2;
}
```

---

### 4.5 Fail Task

```proto
message FailRequest {
  string task_id = 1;
  string reason = 2;
  int32 retry_after_seconds = 3;
}
```

---

## 5. Idempotency & Safety

- REST supports `Idempotency-Key` header (accepted on `POST /process-instances` and `POST /tasks/{task_id}/complete`; cached response behavior to be implemented).
- gRPC tasks are **at-least-once**
- Duplicate completion is safe

---

## 6. Authentication & Authorization (Future)

- API Key / JWT
- Instance-level RBAC
- Task-level permission control

---

## 7. Versioning Strategy

- URL-based versioning (`/v1`)
- Backward-compatible changes only

---

## 8. API & Semantic Stability

### API Stability Policy

- **Stable**: The APIs and semantics listed in this section (and in §9 for History/Trace) are committed to backward-compatible changes only (new fields, new endpoints, new optional parameters).
- **Experimental**: Replay API, UI/Inspector, and any endpoint not listed in the stable set may be adjusted or deprecated; backward compatibility is not guaranteed.
- **Breaking changes**: Allowed only on a **major version** bump; the stable set’s semantics and response shapes will not be broken before then.

---

From **v0.1.0** onward, the following API and semantics are committed to **no breaking changes** (backward-compatible):

**Stable (v0.1.0)**

- `POST /api/v1/process-instances` — start instance
- `GET /api/v1/process-instances/:id` — get instance state
- `GET /api/v1/process-instances/:id/history` — execution history (response: `instance_id` + `events[]` with `sequence`, `category`, `occurred_at`, `payload`)
- `GET /api/v1/process-instances/:id/trace` — aggregated trace
- External-task APIs: `POST .../external-tasks/fetch-and-lock`, `.../complete`, `.../fail`
- **History API semantics**: Events are append-only; sequence is globally ordered per process instance; replay produces the same token state; response schema is backward-compatible once released.
- **Invariant violations**: REST 4xx for invariant violations include `X-Invariant-Violation` header; the header value (kind name) is part of the stable contract.

**May evolve**

- Replay API (session/step/seek), UI/Inspector, and SDK helper interfaces (e.g. future Python SDK) may be extended or adjusted in later versions; they do not affect the stable set above.

---

## 9. History & Trace Semantic Guarantees

This section defines the **semantic contract** of the History and Trace APIs.
These guarantees are **intentional design commitments** and should be considered stable once released.

### History Events

The History API exposes a sequence of immutable execution events emitted by the engine.

**Semantic guarantees:**

* **Append-only**
  History events are never modified or deleted once written.

* **Globally ordered per process instance**
  Each event has a monotonically increasing `sequence` number that defines a total order within the same process instance.

* **Causally consistent**
  The order of events reflects the actual execution order of token transitions, external task lifecycle changes, and instance-level state changes.

* **Deterministic replay**
  Replaying history events in sequence order must reproduce the same token state graph and final instance state.

* **Persistence-first**
  Every history event corresponds to a persisted state transition; no in-memory-only execution steps are emitted.

### Trace API

The Trace API provides a **derived, read-only view** over the underlying history events.

**Semantic guarantees:**

* Traces are computed exclusively from history events.
* Traces do not introduce new execution semantics.
* Multiple trace representations may exist for the same history sequence.
* Trace output format may evolve, but **must remain semantically consistent** with history.

### Stability & Compatibility

* History event semantics are **backward-compatible once released**.
* New event types may be added, but existing event meanings will not change.
* Clients may rely on the History API for:

  * Auditing
  * Debugging
  * Post-mortem analysis
  * Deterministic replay and verification

Breaking changes to history semantics require a **major version change**.

### Non-Goals

The History and Trace APIs are **not intended** to:

* Provide real-time streaming guarantees
* Replace metrics or logging systems
* Serve as a low-latency operational dashboard

They are correctness and auditability primitives.

---

## 10. Relationship to Other Docs

- Execution semantics: `execution-model.md`
- Storage guarantees: `database-schema.md`
- Testing strategy: `docs_testing_strategy.md`
- Release checklist: `release-checklist-v0.1.0.md`

---

> **A stable API turns an engine into a platform.**
