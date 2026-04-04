---
artifact: api-contract
task: bpm-engine-evolution-plan
date: 2026-04-04
role: architect
status: draft
---

# REST API Contract v1

**Base URL**: `/api/v1`

All endpoints are relative to the base URL. The server listens on `http://127.0.0.1:3000` by default.

## Authentication & Headers

| Header | Required | Description |
|--------|----------|-------------|
| `X-Tenant-Id` | No | Tenant identifier for multi-tenant deployments. Passed through to engine context. |
| `Idempotency-Key` | Recommended | Reserved header for idempotent POST operations. **Not yet implemented** — see Missing Implementation section. |

## Shared Types

### ErrorResponse

```json
{
  "error": "string",
  "invariant_violation": "string | null"
}
```

- `invariant_violation` is present only when the error is an `InvariantViolation` (e.g. "EXTERNAL_TASK_LOCKED_BY_ANOTHER_WORKER"). When present, the response also includes `X-Invariant-Violation` header with the same value.

### CompleteTaskResponse

```json
{
  "status": "COMPLETED | FAILED"
}
```

---

## Process Definitions

### `GET /process-definitions/:id`

Get a process definition diagram view for the Trace UI.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `id` | string | Process definition ID |

**Response** `200 OK`

```json
{
  "id": "payment-flow",
  "start": "start",
  "nodes": [
    { "id": "start", "node_type": "Start" },
    { "id": "payment", "node_type": "ExternalTask" },
    { "id": "end", "node_type": "End" }
  ],
  "edges": [
    { "source": "start", "target": "payment" },
    { "source": "payment", "target": "end" }
  ]
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `404` | `ErrorResponse` | Process definition not found |

---

### `POST /process-definitions/deploy`

Deploy a process definition from BPMN 2.0 XML.

**Request Body**: raw BPMN 2.0 XML string (Content-Type: `text/plain`)

**Response** `201 Created`

```json
{
  "process_definition_id": "my-process"
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `400` | `DeployErrorResponse` (Parse variant) | XML parse error |
| `400` | `DeployErrorResponse` (Compile variant) | BPMN compile errors (list of `CompilerError`) |

---

## Process Instances

### `POST /process-instances`

Start a new process instance.

**Request Body**

```json
{
  "process_def_id": "string",
  "variables": {}
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `process_def_id` | string | Yes | ID of the process definition to start |
| `variables` | object | No | Initial variables as key-value pairs |

**Response** `201 Created`

```json
{
  "instance_id": "uuid",
  "status": "RUNNING"
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `400` | `ErrorResponse` | Process definition not found or invalid request |

---

### `GET /process-instances/:id`

Get the current state of a process instance.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `id` | string | Process instance ID |

**Response** `200 OK`

```json
{
  "instance_id": "string",
  "process_def_id": "string",
  "status": "RUNNING | COMPLETED | TERMINATED",
  "current_nodes": ["node_id_1", "node_id_2"],
  "tokens": [Token, ...]
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `404` | `ErrorResponse` | Process instance not found |

---

### `GET /process-instances/:id/trace`

Aggregated execution trace: instance state + token timelines + external task history.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `id` | string | Process instance ID |

**Response** `200 OK`

```json
{
  "instance": InstanceStateResponse,
  "token_timelines": [
    {
      "token_id": "string",
      "node_id": "string",
      "status": "CREATED | READY | EXECUTING | WAITING | SUSPENDED | COMPLETED | TERMINATED",
      "events": [
        {
          "event_type": "string",
          "occurred_at": "unix_timestamp",
          "payload": {}
        }
      ]
    }
  ],
  "external_task_history": [
    {
      "task_id": "string",
      "token_id": "string",
      "process_instance_id": "string",
      "events": [TraceEventView, ...]
    }
  ]
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `404` | `ErrorResponse` | Process instance not found |
| `500` | `ErrorResponse` | History retrieval error |

---

### `GET /process-instances/:id/history`

Execution history for auditing and Trace UI. Returns events in causal order.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `id` | string | Process instance ID |

**Query Parameters**

| Name | Type | Description |
|------|------|-------------|
| `token_id` | string | Optional filter: only events for this token |
| `event_type` | string | Optional filter: only events of this type |

**Response** `200 OK`

```json
{
  "instance_id": "string",
  "events": [
    {
      "sequence": 0,
      "id": "string",
      "event_type": "ProcessStarted | TokenArrived | TokenCompleted | ...",
      "category": "instance | token | external",
      "occurred_at": "unix_timestamp",
      "payload": {}
    }
  ]
}
```

**Event Categories**

| Category | Event Types |
|----------|-------------|
| `instance` | `ProcessStarted`, `ProcessCompleted` |
| `token` | `TokenArrived`, `TokenCompleted`, `TokenFailed`, `UserTaskCreated`, `UserTaskCompleted`, `TimerScheduled`, `TimerFired`, `SagaStarted`, `SagaCompleted` |
| `external` | `ExternalTaskLocked`, `ExternalTaskCompleted`, `ExternalTaskFailed` |

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `500` | `ErrorResponse` | History retrieval error |

---

## Tasks

### `GET /tasks`

List waiting tasks (user tasks and external tasks) across all running process instances.

**Query Parameters**

| Name | Type | Description |
|------|------|-------------|
| `type` | string | Optional filter: `user` or `external` |

**Response** `200 OK`

```json
[
  {
    "task_id": "instance_id:node_id",
    "node_id": "string",
    "instance_id": "string",
    "task_type": "user | external"
  }
]
```

---

### `POST /tasks/:task_id/complete`

Complete a user task.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `task_id` | string | Task ID in `instance_id:node_id` format |

**Request Body**

```json
{
  "variables": {}
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `variables` | object | No | Output variables from task completion |

**Response** `200 OK`

```json
{ "status": "COMPLETED" }
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `400` | `ErrorResponse` | Invalid `task_id` format |
| `500` | `ErrorResponse` | Engine execution error |

---

## External Tasks

External tasks use a lease model: a worker fetches and locks tasks, then completes or fails them. Only one worker can hold a lock at any time. Expired locks are automatically reclaimed.

### `POST /external-tasks/fetch-and-lock`

Fetch and lock available external tasks for a worker.

**Request Body**

```json
{
  "worker_id": "string",
  "task_types": ["payment", "notification"],
  "max_tasks": 10,
  "lock_duration_ms": 60000
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `worker_id` | string | Yes | Identifier of the requesting worker |
| `task_types` | array of string | Yes | Task types to fetch (e.g. `["payment"]`) |
| `max_tasks` | integer | No | Maximum tasks to lock (default: 10) |
| `lock_duration_ms` | integer | Yes | Lock duration in milliseconds; tasks not completed in time are reclaimed |

**Response** `200 OK`

```json
[
  {
    "task_id": "string",
    "token_id": "string",
    "process_instance_id": "string",
    "task_type": "string",
    "variables": {}
  }
]
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `500` | `ErrorResponse` | Lock reclamation or fetch error |

---

### `POST /external-tasks/:task_id/complete`

Complete a locked external task.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `task_id` | string | External task ID |

**Request Body**

```json
{
  "worker_id": "string",
  "variables": {}
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `worker_id` | string | Yes | Worker ID (must match the lock holder) |
| `variables` | object | No | Output variables to merge into process instance |

**Response** `200 OK`

```json
{ "status": "COMPLETED" }
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `400` | `ErrorResponse` with `X-Invariant-Violation: EXTERNAL_TASK_LOCKED_BY_ANOTHER_WORKER` | Task is locked by another worker |
| `400` | `ErrorResponse` with `X-Invariant-Violation: EXTERNAL_TASK_NOT_LOCKED` | Task is not currently locked |
| `404` | `ErrorResponse` | Task not found |
| `500` | `ErrorResponse` | Engine execution error |

---

### `POST /external-tasks/:task_id/fail`

Report a locked external task as failed. If the task has exhausted retries, the token is also marked as failed.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `task_id` | string | External task ID |

**Request Body**

```json
{
  "worker_id": "string",
  "error": "string",
  "retry_after_ms": 30000
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `worker_id` | string | Yes | Worker ID (must match the lock holder) |
| `error` | string | Yes | Human-readable error description |
| `retry_after_ms` | integer | No | If present, the task will be re-fetchable after this duration |

**Response** `200 OK`

```json
{ "status": "FAILED" }
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `400` | `ErrorResponse` with `X-Invariant-Violation` | Invariant violation (e.g. wrong worker) |
| `404` | `ErrorResponse` | Task not found |
| `500` | `ErrorResponse` | Engine execution error |

---

## Replay Sessions

Replay sessions are ephemeral (not persisted). They allow stepping through historical events of a process instance to produce a read-only snapshot.

### `POST /process-instances/:id/replay`

Create a replay session for a process instance.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `id` | string | Process instance ID |

**Response** `201 Created`

```json
{
  "session_id": "uuid",
  "instance_id": "string",
  "total_events": 42
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `404` | `ErrorResponse` | Process instance not found |
| `500` | `ErrorResponse` | History retrieval error |

---

### `GET /replay/:session_id/snapshot`

Read-only snapshot of the replay session at current cursor.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `session_id` | string | Replay session ID |

**Response** `200 OK`

```json
{
  "cursor": 10,
  "total_events": 42,
  "completed": false,
  "tokens": [
    { "token_id": "string", "node_id": "string", "state": "WAITING" }
  ]
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `404` | `ErrorResponse` | Session not found or expired |

---

### `POST /replay/:session_id/step`

Apply the next event to the replay and advance the cursor by one.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `session_id` | string | Replay session ID |

**Response** `200 OK`

```json
{
  "cursor": 11,
  "event": {
    "event_type": "TokenArrived",
    "occurred_at": "1700000000",
    "token_id": "string | null",
    "node_id": "string | null"
  },
  "snapshot": {
    "completed": false,
    "tokens": [{ "token_id": "string", "node_id": "string", "state": "WAITING" }]
  }
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `400` | `ErrorResponse` | No more events to step |
| `404` | `ErrorResponse` | Session not found or expired |
| `500` | `ErrorResponse` | Replay apply failed |

---

### `POST /replay/:session_id/seek`

Jump to a specific cursor position by replaying all events from `0` to `cursor`.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `session_id` | string | Replay session ID |

**Request Body**

```json
{
  "cursor": 20
}
```

**Response** `200 OK`

```json
{
  "cursor": 20,
  "snapshot": {
    "completed": false,
    "tokens": [{ "token_id": "string", "node_id": "string", "state": "WAITING" }]
  }
}
```

**Error Responses**

| Status | Body | Description |
|--------|------|-------------|
| `404` | `ErrorResponse` | Session not found or expired |
| `500` | `ErrorResponse` | Replay apply failed |

---

### `DELETE /replay/:session_id`

Destroy a replay session and release resources.

**Path Parameters**

| Name | Type | Description |
|------|------|-------------|
| `session_id` | string | Replay session ID |

**Response** `204 No Content`

---

## Error Code Definitions

All errors return `ErrorResponse` JSON with an `error` string field.

### HTTP Status Codes

| Status | Meaning |
|--------|---------|
| `200` | Success |
| `201` | Created |
| `204` | No Content (successful delete) |
| `400` | Bad Request (invalid input, invariant violation, parse/compile error) |
| `404` | Not Found |
| `500` | Internal Server Error |

### Invariant Violation Kinds

When the engine detects a protocol violation, `invariant_violation` field is populated and the response includes an `X-Invariant-Violation` header.

| Kind | Meaning | Affected Operations |
|------|---------|---------------------|
| `EXTERNAL_TASK_LOCKED_BY_ANOTHER_WORKER` | Task is locked by a different worker | `external-task-complete`, `external-task-fail` |
| `EXTERNAL_TASK_NOT_LOCKED` | Task is not currently locked | `external-task-complete`, `external-task-fail` |
| `EXTERNAL_TASK_NOT_FOUND` | Task does not exist | `external-task-complete`, `external-task-fail` |
| `TOKEN_NOT_FOUND` | Token not found in instance | `external-task-complete` |

---

## Missing Implementation: Idempotency Key

The `Idempotency-Key` header is reserved on the REST API but **not currently implemented** in any endpoint handler. The header position is not yet wired up in the route handlers.

**Impact**: All mutating endpoints (`POST /process-instances`, `POST /tasks/:task_id/complete`, `POST /external-tasks/fetch-and-lock`, `POST /external-tasks/:task_id/complete`, `POST /external-tasks/:task_id/fail`, `POST /process-definitions/deploy`, `POST /process-instances/:id/replay`) are not idempotent. Retrying a request may produce duplicate side effects.

**Affected Endpoints**: All POST mutation endpoints listed above.

**Implementation Guidance**: To add idempotency support:

1. Extract `Idempotency-Key` header in each mutating handler using `headers.get("Idempotency-Key")`.
2. Store key → response mapping in a dedicated store (e.g. Redis or the existing `MemoryRepo`).
3. On subsequent requests with the same key:
   - If a cached response exists and the operation succeeded, return the cached response with `200 OK`.
   - If a cached response exists and the operation is still in-flight, return `409 Conflict`.
4. Consider TTL (e.g. 24 hours) for idempotency records.
5. At minimum, idempotency should be added to `POST /external-tasks/:task_id/complete` since it is the most likely operation to be retried by worker SDK clients.

---

## Endpoint Summary

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/process-definitions/:id` | Get process definition diagram view |
| `POST` | `/process-definitions/deploy` | Deploy BPMN 2.0 XML |
| `POST` | `/process-instances` | Start a process instance |
| `GET` | `/process-instances/:id` | Get process instance state |
| `GET` | `/process-instances/:id/trace` | Get aggregated execution trace |
| `GET` | `/process-instances/:id/history` | Get execution history |
| `POST` | `/process-instances/:id/replay` | Create replay session |
| `GET` | `/tasks` | List waiting tasks |
| `POST` | `/tasks/:task_id/complete` | Complete a user task |
| `POST` | `/external-tasks/fetch-and-lock` | Fetch and lock external tasks |
| `POST` | `/external-tasks/:task_id/complete` | Complete a locked external task |
| `POST` | `/external-tasks/:task_id/fail` | Fail a locked external task |
| `POST` | `/replay/:session_id/step` | Step forward one replay event |
| `POST` | `/replay/:session_id/seek` | Jump to cursor position |
| `GET` | `/replay/:session_id/snapshot` | Get replay snapshot |
| `DELETE` | `/replay/:session_id` | Delete replay session |

**Total: 16 endpoints**
