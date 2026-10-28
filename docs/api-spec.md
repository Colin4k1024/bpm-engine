---
layout: default
---

# REST & gRPC API Specification (Draft)

> This document defines the **external API contract** of the BPM Engine.
> The APIs are designed for **service usage**, **external workers**, and **operational tooling**.
>
> **Normative, testable contract (E2-4)**: [`docs/openapi.yaml`](openapi.yaml)
> is the machine-readable OpenAPI 3.1 contract of the REST API — paths,
> request/response schemas, security scheme, and the stable
> `ErrorResponse.code` enum. The CI consistency suite
> (`crates/server/rest/tests/openapi_contract.rs`) verifies the spec
> against the implementation in both directions (route parity, error-code
> set equality, sample-response validation). This document remains the
> prose specification; where the two disagree, `openapi.yaml` wins and a
> PR must fix the divergence.
>
> **Machine capability discovery (E4-1)**: `GET /capabilities` serves a
> derived, committed tool catalog (one tool per operation, parameter
> schemas, read-only/mutating classification, error examples) for AI
> agents — unauthenticated, like `/health` and `/ready`. See
> [`docs/agent-tools.md`](agent-tools.md).

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

#### Get Instance Steps (unified step timeline, E6-1)

```
GET /process-instances/{instance_id}/steps
```

One step per `(node_id, token_id)` in causal order, joining token
transitions, worker lease attempts (lock → complete/fail, a re-lock
after lease expiry closes the open attempt as `expired`) and the E3-1
AI task envelope metadata (model/tool versions, cost), keyed by the
trace id (`trace_id` = the instance id in v1). Read-only aggregation
over the instance, definition and history stores — no new stores, no
new event types; sensitive-by-default: the response carries no variable
values (only event types/timestamps, worker ids, retry counters,
variable name lists, and envelope metadata summaries; redacted or
unparsable envelopes degrade to `{"opaque": true}`). Full contract:
`docs/openapi.yaml` (`StepsResponse`); semantics and design decisions:
`docs/trace.md`.

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

- REST supports the `Idempotency-Key` header on `POST /api/v1/process-instances`
  (E2-4, implemented): see §5c for the exact semantics.
- gRPC tasks are **at-least-once**
- Duplicate completion is safe

---

## 5a. Error Contract (E2-4)

Every error response uses the `ErrorResponse` body:

```json
{
  "error": "human-readable message (wording is NOT a contract)",
  "code": "stable_snake_case",
  "invariant_violation": "OptionalKindName"
}
```

- `code` is present on every error emitted by the typed boundaries (the
  pre-E2-4 deploy parse/compile 400s keep their legacy shapes — see the
  notes below).
- `invariant_violation` is present only on invariant rejections and is
  mirrored in the `X-Invariant-Violation` response header (stable).
- 5xx bodies never leak internal details; they are logged server-side.

Stable error codes (single source of truth:
`crates/server/rest/src/routes.rs::error_codes::ERROR_CODES`, mirrored in
`docs/openapi.yaml` — the CI suite asserts set equality; **renaming a
code is a breaking change**):

| Code | HTTP | Meaning |
|------|------|---------|
| `not_found` | 404 | Unknown instance / definition / task / dead-letter / replay-session id (and, under JWT auth, an id of ANOTHER tenant — E5-1) |
| `unauthorized` | 401 | Missing or invalid API key |
| `invalid_token` | 401 | Missing/malformed/rejected JWT when JWT auth is configured (E5-1) |
| `forbidden` | 403 | RBAC: the verified role lacks permission, or the `X-Tenant-Id` header conflicts with the token's tenant claim (E5-1) |
| `rate_limited` | 429 | Request rate above the configured window |
| `invalid_request` | 400 | Generic client error (malformed path ids, bad query params, invalid idempotency key) |
| `invalid_json` | 400/422 | Request body is not valid JSON for the endpoint (syntax/content-type → 400; type mismatch → 422) |
| `internal` | 500 | Unexpected server-side failure |
| `deploy_failed` | 500 | Definition compiled but could not be persisted |
| `invalid_format` | 400 | validate/dry-run `format` is neither `bpmn` nor `dsl` (E2-2) |
| `empty_source` | 400 | validate/dry-run `source` is empty (E2-2) |
| `invalid_request_body` | 400 | validate/dry-run body is not deserializable JSON (E2-2) |
| `variable_too_large` | 400 | Variable value exceeds `engine.variable_max_bytes` (E2-3) |
| `variable_invalid_envelope` | 400 | Malformed/unsupported envelope version (E2-3) |
| `variable_invalid_artifact_uri` | 400 | Artifact URI violates the E2-3 URI contract |
| `sensitive_requires_encryption` | 400 | Sensitive variable not present / not encrypted (E2-3) |
| `encryption_key_not_configured` | 400 | `encrypted` requested while `BPM_VARIABLE_ENCRYPTION_KEY` is unset |
| `task_envelope_invalid` | 400 | AI task envelope failed validation (E3-1, sub-code in message) |
| `reserved_variable_key` | 400 | User-supplied `__task_envelope` variable (E3-1) |
| `invalid_retry_category` | 400 | fail `category` is not `transient`/`permanent` (E3-1; the category is behavioral since E3-2: `permanent` skips the retry loop — straight to `Failed` + DLQ) |
| `missing_required_fields` | 400 | user-task complete without the form's required fields |
| `task_not_found` | 400 | External-task store: unknown task (kept 400 for compat) |
| `task_not_locked` | 400 | External-task store: task not in the Locked state |
| `lock_expired` | 400 | External-task store: lease expired |
| `invariant_violation` | 400 | Engine invariant violated (kind in `invariant_violation` + header) |
| `lock_conflict` | 409 | Lock extension refused (unknown / not locked / other owner) |

Backward compatibility: pre-E2-4 error bodies only GAIN the `code` field;
existing codes (E2-2/E2-3/E3-1) are unchanged. The deploy endpoint's 400
parse (`{"error": ...}`) and compile (`{"errors": [...]}`) shapes are
legacy-stable (no code field); its persistence failure moved from the
parse shape to 500 `{error, code: "deploy_failed"}` (E1-1 review fix).

---

## 5b. Pagination (E2-4)

- `GET /api/v1/process-instances?limit&offset` — store-level pagination
  (SQL `LIMIT`/`OFFSET` in postgres mode; the hot path). `limit` ∈
  1..=500 (default **50**), `offset` ≥ 0 (default 0); malformed or
  out-of-range values → 400 `invalid_request`. Instances are ordered by
  instance id; an offset beyond the end returns an empty page. The
  response gains the additive `pagination: {limit, offset, returned}`
  object; the `instances` array shape is unchanged.
- `GET /api/v1/tasks` and `GET /api/v1/dead-letters` accept the same
  optional `limit`/`offset`, applied **in memory** over the assembled
  result (documented; the store traits are not extended for these —
  instance listing is the only store-level window). Absent parameters
  keep the previous full-list behavior. NOTE for `/dead-letters`: the
  fetch itself is bounded — the store returns at most **100** entries
  per request (memory and Postgres adapters alike), so a page can never
  expose more than the first 100 dead letters; deeper listing requires
  store-level pagination (not part of v1).
- `GET /api/v1/process-instances/:id/history` accepts an optional
  `limit` (1..=500) that truncates the event list after causal sorting;
  absent → full history (previous behavior).

---

## 5c. Idempotency Keys — start-instance (E2-4)

`POST /api/v1/process-instances` accepts an optional `Idempotency-Key`
HTTP header (1–128 printable ASCII bytes; malformed keys are rejected
with 400 `invalid_request` rather than silently ignored).

Because the `(key, tenant_id)` pair co-forms the idempotency primary
key, the co-stored `X-Tenant-Id` is bounded at this save boundary: when
an `Idempotency-Key` is supplied, the tenant id must be at most 128
printable ASCII bytes (400 `invalid_request` "tenant id too long"
otherwise). `X-Tenant-Id` keeps its unbounded, pass-through semantics on
every other endpoint and on key-less starts.

Semantics:

- **Known key** (recorded for the same `X-Tenant-Id` scope): the server
  returns **200** (not 201) with the SAME `{instance_id, status}`
  response shape for the originally created instance — `status` is the
  instance's CURRENT state (RUNNING/COMPLETED/TERMINATED), not a cached
  copy. No second instance is created and the engine emits no events.
- **Unknown key**: the instance is created as today (**201**) and the
  key → instance_id mapping is recorded afterwards (upsert; keys are
  tenant-scoped, so the same key under different tenants addresses
  different instances).
- **Persistence**: memory adapter keeps a map; PostgreSQL stores it in
  the `idempotency_key` table (migration 0003,
  `PRIMARY KEY (key, tenant_id)`, `tenant_id TEXT NOT NULL DEFAULT ''`).

Atomicity boundary (honest, single-service): the check-then-create
sequence is **not cross-process atomic** — the engine runs as one
service per database (the same boundary as the migration runner), so
two concurrent identical requests can still race. A key-recording
failure AFTER the instance was created is reported as 500 `internal`
with an explicit message (the retry may then create a second instance).
Cross-process uniqueness would require a unique-constraint insert path;
out of scope for v1.

---

## 6. Authentication & Authorization (Future)

- API Key / JWT
- Instance-level RBAC
- Task-level permission control

---

## 7. Versioning Strategy

- URL-based versioning (`/v1`)
- Backward-compatible changes only

### Versioning rules (E2-4, binding for `/api/v1`)

- **Path version `/api/v1` is frozen for additive changes only.** A
  change is additive if: a new endpoint, a new OPTIONAL request
  parameter, or a new OPTIONAL response field. Removing or renaming a
  field, changing a field's type, or changing a response status for a
  given condition is a BREAKING change and requires `/api/v2`.
- **Error codes are stable identifiers.** The list in §5a is the
  contract: renaming or removing a code is breaking; adding a code is
  additive (clients must treat unknown codes as fallback-to-`error`).
- **Response fields are additive-only (optional fields may appear).**
  Example: the instance list gained `pagination: {limit, offset,
  returned}` (E2-4) while keeping the `instances` array shape.
- **Pagination and idempotency are part of the contract** (§5b, §5c):
  parameter names, defaults (`limit=50`), bounds (`1..=500`), and the
  200-vs-201 idempotent replay semantics are stable.
- The OpenAPI document (`docs/openapi.yaml`) is the machine-readable
  form of these rules and is CI-checked against the implementation.

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
- `GET /api/v1/process-instances/:id/steps` — unified step timeline (E6-1: token steps + worker attempts + envelope metadata incl. cost; no variable values; see `docs/trace.md`)
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
