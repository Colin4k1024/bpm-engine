---
layout: default
---

# Step Trace — 统一步骤轨迹 (E6-1, issue #26)

> One per-instance step timeline that joins token transitions, history
> events, worker lease attempts, and the AI metadata (model/tool
> versions, cost) recorded in the E3-1 envelopes, keyed by a trace id —
> with sensitive values never surfaced. Implementation:
> `bpm_engine_server_rest::step_trace` (pure assembly) + the
> `GET /api/v1/process-instances/:id/steps` route; the wire contract is
> `docs/openapi.yaml` (`StepsResponse` schema family, normative).

---

## 1. The endpoint

```
GET /api/v1/process-instances/:id/steps
→ 200 { "trace_id": "...", "instance_id": "...", "process_def_id": "...",
        "state": "RUNNING|COMPLETED|TERMINATED",
        "steps": [ ... step objects ... ] }
→ 404 not_found (unknown instance) · 500 internal (storage failure)
```

Response shape (abridged; the full contract is `docs/openapi.yaml`):

```json
{
  "trace_id": "<instance_id>", "instance_id": "<instance_id>",
  "process_def_id": "ai-work-flow", "state": "COMPLETED",
  "steps": [
    { "step_no": 1, "node_id": "start", "token_id": "t1", "kind": "node",
      "events": [ { "event_type": "TokenArrived", "occurred_at": "1793..." } ] },
    { "step_no": 2, "node_id": "ai-work", "token_id": "t2", "kind": "external_task",
      "events": [ { "event_type": "TokenArrived", "occurred_at": "..." }, ... ],
      "attempts": [
        { "worker_id": "worker-a", "outcome": "failed",
          "locked_at": "...", "completed_or_failed": "...",
          "error": "{\"retry_category\":\"transient\"}: model over quota",
          "retries_remaining": 2,
          "envelope": { "model": { ... }, "tools": [ ... ], "cost": { ... },
                        "execution_id": "task:0", "retry_category": "transient" } },
        { "worker_id": "worker-b", "outcome": "completed",
          "locked_at": "...", "completed_or_failed": "...",
          "error": null, "retries_remaining": 2,
          "envelope": { "model": { ... }, "cost": { ... }, "artifacts": [ ... ],
                        "execution_id": "task:1" } }
      ],
      "envelope_summary": { "model": { ... }, "cost": { ... }, "artifacts": [ ... ],
                            "execution_id": "task:1" },
      "variables_touched": [ "summary" ] }
  ]
}
```

The pre-existing `GET /api/v1/process-instances/:id/trace` graph view
(for the Trace UI) is unchanged; `/steps` is a NEW, additive endpoint.

## 2. What joins what

`/steps` is a **read-only aggregation over existing stores** — no new
store traits, no new event types, no new writes:

| Data | Source | Used for |
|------|--------|----------|
| Process instance (tokens) | `ProcessInstanceStore::load` | token→node correlation, instance `state` |
| Process definition (node types) | `ProcessDefinitionStore::load` (optional) | step `kind` classification — absent ⇒ event-type fallback; a LOAD failure is a storage error (500 `internal`), not a silent degradation |
| History events | `HistoryRepo::list_by_instance` (ALL events) | everything else |

Assembly (`step_trace::assemble_steps`, pure):

1. **Correlate token ↔ node.** In this engine a token id belongs to
   exactly one node (each transition mints a fresh token,
   `crates/runtime/src/transition.rs`), so `(node_id, token_id)` is the
   natural step identity. The map is built from the instance's tokens
   plus every history event carrying both ids; events with only a
   `token_id` (e.g. `TokenCompleted`, `ExternalTaskLocked`) resolve their
   node through it, and node-scoped events without a token
   (`UserTaskCreated`/`UserTaskCompleted`) join the token that MOST
   RECENTLY ARRIVED at that node — a user task is created when its token
   arrives, so the last `TokenArrived` of the node before such an event
   is the correct attribution, also when several tokens share one node
   (loop re-entry, parallel branches). (Fallback when the visible
   history contains no arrival for the node — e.g. a truncated window:
   any token correlated to the node, lexicographically first.)
2. **Group by `(node_id, token_id)` in causal order.** Steps accumulate
   in first-event order and are numbered from 1. Instance-level events
   (`ProcessStarted`, `ProcessCompleted`, `ProcessTerminated` — no node,
   no token) are intentionally not steps; the instance state is the
   top-level `state` field.
3. **Join worker attempts.** For external-task steps, each
   `ExternalTaskLocked` event (appended by the fetch-and-lock REST
   boundary, carrying `worker_id` + `retries`) opens an attempt; the
   task's `ExternalTaskFailed` / `ExternalTaskCompleted` event closes it
   with the outcome timestamp, the recorded error message (failures),
   the retry counter, and the envelope recorded with that outcome. Every
   attempt carries an explicit `outcome`:
   `in_flight` (locked, no outcome event yet), `completed`, `failed`, or
   `expired` — the lease elapsed without any terminating event and
   another worker re-locked the SAME task (E1-3 redelivery), so the
   re-lock closes the still-open attempt as `expired`
   (`completed_or_failed` stays `null`: no event carries a timestamp for
   it). The join is worker-aware: the REST fail event carries the
   failing `worker_id`, so when the deterministic causal sort (below)
   interleaves a same-second fail with a later re-lock, the failure is
   still attributed to its own worker's attempt — only a superseded
   attempt that NO outcome ever claims ends as `expired`.
4. **Summarize the envelope.** The attempt's outcome event carries the
   E3-1 envelope under `variables.__task_envelope`; it is parsed via
   `bpm_engine_core::ai_task` and summarized (below). The node-level
   `envelope_summary` is the LAST parsable envelope seen on the step.

**Causal order, deterministically:** history event ids are random UUIDs
(not sequence numbers) and `occurred_at` has SECONDS granularity, so
same-second ties are the norm (a lease is commonly locked and resolved
within one second). The `/steps` handler therefore sorts by
`(occurred_at, phase_rank, insertion_index)` —
`step_trace::sort_history_causally`, a stable sort on
`(occurred_at, phase_rank)` where the phase rank encodes each event
type's place in the engine lifecycle:

| rank | phase | event types |
|------|-------|-------------|
| 0 | instance starts | `ProcessStarted` |
| 10 | token arrives at the node | `TokenArrived` |
| 20 | the node's work is scheduled | `TimerScheduled`, `UserTaskCreated`, `CallActivityStarted` |
| 30 | a worker takes the lease | `ExternalTaskLocked` |
| 40 | the work resolves / the node emits output | `ExternalTaskCompleted`, `ExternalTaskFailed`, `UserTaskCompleted`, `TokenFailed`, `CallActivityCompleted`, `TimerFired`, `MessageSent`, `SignalSent` |
| 50 | the token leaves the node | `TokenCompleted` |
| 60/61 | compensation | `SagaStarted`, `SagaCompleted` |
| 70 | the instance ends | `ProcessCompleted`, `ProcessTerminated` |

Unknown/future event types get a middle default (35). This guarantees a
lock (30) always precedes its outcome (40) within the same second —
deliberately NOT an id tie-break like the display-oriented `/history`
endpoint uses (random UUIDs would randomize same-second pairs), and not
a plain insertion-order tie-break either: the memory store happens to
preserve insertion order, but a SQL history listing does not guarantee
it, and the timeline must not depend on the store.

**Honest limitation:** the phase tie-break makes the ordering
deterministic, but same-second ordering BETWEEN different nodes (and
between different tasks) remains best-effort — the true order is not
recoverable from seconds-granularity timestamps plus a type-derived
rank (the same-second fail→re-lock case above is repaired precisely
because the fail event carries the worker id). The exact fix is a
per-instance monotonic sequence in the history store (future work; the
event UUIDs cannot serve as one). `TimerFired` is written by the
engine's history handler with an EMPTY instance id (existing behavior),
so the firing instant is not part of the per-instance history; the
timer step shows the token waiting at the timer node and the flow
continuing past it.

## 3. The envelope summary

The summary is envelope **metadata only** — exactly what E6-1 needs to
make an attempt explainable:

| Field | From `AiTaskEnvelope` |
|-------|----------------------|
| `model` | `model` (provider/name/version) |
| `tools` | `tools[]` (name/version) |
| `cost` | `cost` (E6-1: currency, amount_micros, tokens_in/out, duration_ms) |
| `artifacts` | `artifacts[]` (uri/sha256/size/media_type) |
| `execution_id` | `execution_id` (`<task_id>:<attempt>`) |
| `retry_category` | `retry_category` (`transient`/`permanent`) |

Excluded on purpose: `idempotency_key`, `input_schema`/`output_schema`
(schemas can be multi-KB — they are why history redaction exists), and
`timeout_secs` (node/lease timeouts remain authoritative).

The two forms are MUTUALLY EXCLUSIVE by construction: a summary is
either the metadata fields (each optional, only what the envelope
recorded) or exactly `{"opaque": true}` — never a mix. The OpenAPI
`StepEnvelopeSummary` schema documents this in prose (the whitelisted
JSON-Schema subset has no `oneOf`); the assembler is what enforces it.

**Oversized or unknown envelopes → `{"opaque": true}`.** The one value
the assembler reads, `__task_envelope`, is engine-owned AI metadata
(E3-1), not a user variable; when its stored form was redacted by the
E2-3 history policy (oversized → `__truncated:`) or is otherwise
unparsable (e.g. a future `__var:2:`), the summary degrades to the
opaque marker and nothing of the value is surfaced.

## 4. Sensitive by default (敏感字段默认不记录)

The `/steps` response contains **no variable values at all** — the
strongest form of the issue's requirement:

- Step events carry `event_type` + `occurred_at` only — never payloads.
- `variables_touched` lists variable **names** (from the event payload
  key names of `variables`/`initial_variables`), never values.
- `error` on a failed attempt is the recorded task error message (the
  worker-supplied failure description, already persisted verbatim in
  history), not a variable value.
- The instance's (possibly decrypted) variables are never read by the
  assembler — unlike `GET /process-instances/:id`, which decrypts for
  the API caller, `/steps` does not touch variable values in any form.

## 5. The trace id

**v1 decision: `trace_id` IS the instance id** — a documented identity,
not a new identifier. Rationale: a dedicated OTel-style trace id is an
observability concern (E6-2 scope), and the instance id is already the
stable, persistent join key of everything the timeline aggregates. No
core struct or schema change is needed for it (that is deliberate: the
cheapest honest v1). Per-HTTP correlation remains on the request-id
middleware's `X-Request-Id` response header, which stamps every call.

## 6. The cost field (envelope contract)

`cost: Option<TaskCost>` is an **additive OPTIONAL field of envelope
v1** (see `docs/ai-task-envelope.md` §2 and §11):

```json
"cost": {
  "currency": "USD",          // ≤ 8 bytes, non-empty when present
  "amount_micros": 1500000,   // 1_000_000 = 1 unit of currency
  "tokens_in": 1204, "tokens_out": 37,
  "duration_ms": 980
}
```

- All fields optional (a partial report is valid); numeric fields are
  `u64` — non-negative by construction (a negative JSON number is
  rejected at parse time).
- `amount_micros` is a micro-unit INTEGER amount in `currency` — never
  a float.
- Validation (`AiTaskEnvelope::validate`, stable sub-code
  `ai_task_envelope_invalid_cost`): `currency` non-empty and ≤ 8 bytes
  when present.
- Compatibility: an envelope serialized by a pre-E6-1 writer (no `cost`
  key) parses unchanged with `cost = None`.

## 7. Related docs

- `docs/ai-task-envelope.md` — the envelope contract (incl. the new
  `cost` field and the additive-field evolution note).
- `docs/api-spec.md` — REST surface & stability; `docs/openapi.yaml` is
  the normative machine-readable contract (the CI consistency suite
  checks route parity, error codes, and schema subset).
- `docs/variables-and-artifacts.md` — the E2-3 value contract and the
  history redaction policy the opaque summaries build on.
- `docs/execution-model.md` — token lifecycle (why a token belongs to
  exactly one node).
