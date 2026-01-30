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

- REST supports `Idempotency-Key` header
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

## 8. Relationship to Other Docs

- Execution semantics: `execution-model.md`
- Storage guarantees: `database-schema.md`
- Testing strategy: `testing-strategy.md`

---

> **A stable API turns an engine into a platform.**

