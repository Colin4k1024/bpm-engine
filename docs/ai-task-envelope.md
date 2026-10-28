---
layout: default
---

# AI Task Envelope — Contract Reference (E3-1)

> This is the **contract reference for AI workers** (agents, LLM callers,
> tool-using services) that execute external tasks. The engine-side
> implementation is `bpm_engine_core::ai_task` (pure data + validation); the
> value format it rides on is the E2-3 variable contract
> (`docs/variables-and-artifacts.md`, ADR-007). The engine never interprets
> schema contents, never dereferences artifact URIs, and does not change
> retry behavior based on `retry_category` (v1).

---

## 1. One variable, typed metadata

Task variables remain the engine's input/output channel (§5). The AI task
envelope is **one task variable** under the reserved key `__task_envelope`
whose value is a normal E2-3 json envelope carrying the whole typed
envelope as `val`:

```text
__task_envelope = __var:1:{"t":"json","val":{"schema_version":1,"execution_id":"...",...}}
```

Everything else about task completion is unchanged: fetch-and-lock
delivers the map verbatim, `complete` merges the submitted map into the
task and the process instance, E2-3 caps and redaction apply. A worker
that ignores the envelope entirely behaves exactly as before.

## 2. Field table

JSON keys are snake_case; unknown fields are **rejected**
(`deny_unknown_fields`); optional fields and empty lists are omitted when
serialized.

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `schema_version` | u32 | yes | Must be `1`. Future versions are rejected, never guessed at (§10). |
| `execution_id` | string | yes | Identifies ONE attempt: `"<task_id>:<attempt>"` (§3). Non-empty. |
| `idempotency_key` | string | no | Worker-chosen side-effect dedup key (§4). Non-empty when present. |
| `timeout_secs` | u64 | no | Worker-side timeout hint in seconds. > 0 when present. **Informational in v1** — the lease and the node's task timeout remain authoritative. |
| `retry_category` | `"transient"` \| `"permanent"` | no | Category of the last failure (§9). Recorded only in v1. |
| `model` | `{provider, name, version?}` | no | The model that executed the task. `provider`/`name` non-empty (and `version` non-empty when present). |
| `tools` | `[{name, version?}]` | no | Tools invoked while executing. Each `name` non-empty. |
| `cost` | `{currency?, amount_micros?, tokens_in?, tokens_out?, duration_ms?}` | no | Cost of the execution (E6-1, the first additive optional field — see §11). Every field optional; the numeric fields are non-negative u64 (a negative JSON number is rejected at parse time). `currency` non-empty and ≤ 8 bytes when present (sub-code `ai_task_envelope_invalid_cost`). `amount_micros` is a micro-unit integer in `currency` (1_000_000 = 1 unit), never a float. Surfaced in the `/steps` envelope summaries (`docs/trace.md`). |
| `artifacts` | `[{uri, sha256?, size?, media_type?}]` | no | Artifacts produced/consumed (inputs and outputs). Each ref passes the E2-3 artifact URI rules (§3 of `docs/variables-and-artifacts.md`): absolute URI, no userinfo, no fragment, ≤ 2048 bytes, `sha256` = exactly 64 hex when present. The engine never dereferences. |
| `input_schema` | JSON object | no | JSON Schema fragment describing the expected **input variables** of the task. |
| `output_schema` | JSON object | no | JSON Schema fragment describing the output variables the task promises to produce. |

Validation runs at every write boundary that accepts an envelope
(`AiTaskEnvelope::validate`); every violation is a typed error with a
stable snake_case code (§10).

## 3. `execution_id` derivation

`execution_id = "<task_id>:<attempt>"` (helper: `bpm_engine_core::execution_id`).

- `task_id` is the engine's task id — **stable across retries** of the same
  task. The idempotency key (not the execution id) is the side-effect
  identity.
- `attempt` is **worker-tracked** (start at 0, increment on each delivery
  you observe). The engine's `retries` counter counts *down* from the
  node's initial value, so do not derive attempts from it.
- The execution id identifies one attempt: use it in logs, traces and the
  envelope; change it when you are redelivered and re-execute.

## 4. Idempotency key guidance

External tasks are delivered **at least once** (lease expiry ⇒ redelivery,
`docs/recovery.md` §8) and workers must make side effects idempotent.
Recommended key: a deterministic hash over **input variables + model +
tool versions** (e.g. SHA-256 of the serialized inputs, model ref, tool
refs). Properties:

- Same task redelivered with unchanged inputs ⇒ same key ⇒ your dedup
  store maps it to the already-performed side effect.
- Changed inputs (or a genuinely different model/tool mix) ⇒ different key
  ⇒ new side effect is legitimate.
- The engine **records** the key (task variables, history, instance) but
  does **not** enforce it — dedup remains the worker's job.

## 5. How input/output variables travel

- **Input**: `fetch-and-lock` returns the task's variable map verbatim.
  Parse each value per the E2-3 contract: plain string / `__var:1:{"t":"json",...}`
  / `__var:1:{"t":"artifact",...}` / `__encrypted:` ciphertext (server-side
  key; workers do not decrypt).
- **Output**: `complete` merges your submitted map into the task row and
  the process instance. Any mix of plain strings, json envelopes and
  artifact envelopes is allowed, under the E2-3 boundary checks (valid
  envelopes, valid artifact URIs, `variable_max_bytes` on the final stored
  string — which includes the serialized envelope at `__task_envelope`).
- Gateway conditions and the EL only see plain strings — route on plain
  variables, never on envelope values (E2-3 §2).

The worker SDK gives you typed access on both sides
(`bpm_engine_worker_sdk::ai_task`):

```rust,ignore
// In: typed access over the fetched task's variables.
let input = TaskInput::from_task_variables(&task.variables);
let topic: Option<&str> = input.plain("topic");
let payload: Option<serde_json::Value> = input.json("payload");
let doc: Option<ArtifactRef> = input.artifact("doc");
let upstream: Option<Result<AiTaskEnvelope, _>> = input.envelope();

// Out: builder; the envelope rides in __task_envelope (validated here,
// before side effects and before the HTTP call).
let variables = TaskOutput::new()
    .json_variable("summary", serde_json::json!({"text": "..."}))
    .artifact_variable(
        "report",
        ArtifactRef { uri: report_uri, sha256: None, size: None, media_type: None },
    )
    .envelope(new_envelope(&task.task_id, 0))
    .build()?;
client.complete(&task.task_id, worker_id, variables).await?;

// Or the one-shot convenience (identical wire bytes):
client.complete_with_envelope(&task.task_id, worker_id, variables, &envelope).await?;
```

## 6. Where the envelope is stored/visible

| Place | When | Notes |
|-------|------|-------|
| Task row variables | after `complete` (with envelope) | part of the task's merged variables (the engine's own storage; the fail path does not touch them, §8) |
| Process instance variables | after `complete` | `GET /api/v1/process-instances/:id` — merged by `ExternalTaskCompleted`, like any output variable |
| Event history `ExternalTaskCompleted` payload | after `complete` | under `variables.__task_envelope`, **redacted** per E2-3 (values over `history_variable_max_bytes`, default 1024, become `__truncated:` markers; instance variables are never truncated) |
| Next task's input variables | chained AI tasks in the same instance | the instance merge happens before the next external task's variable snapshot, so the next worker receives the previous envelope via `TaskInput::envelope()` — overwrite it with your own on complete |
| Event history `ExternalTaskFailed` payload | after `fail` (with envelope) | under `variables.__task_envelope`, redacted (§8) |

## 7. Submitting the envelope (REST)

### complete — backward compatible

`POST /api/v1/external-tasks/:task_id/complete` gains ONE optional field;
`variables` is unchanged:

```json
{
  "worker_id": "worker-1",
  "variables": { "summary": "Q3 revenue grew 12%." },
  "envelope": {
    "schema_version": 1,
    "execution_id": "<task_id>:0",
    "idempotency_key": "sha256:...",
    "timeout_secs": 120,
    "model": { "provider": "openai", "name": "gpt-4o", "version": "2024-08-06" },
    "tools": [ { "name": "web_search", "version": "2.1" } ],
    "cost": { "currency": "USD", "amount_micros": 1500000, "tokens_in": 1204, "tokens_out": 37, "duration_ms": 980 },
    "artifacts": [ { "uri": "https://storage.example.com/out/report.md", "sha256": "…64 hex…", "size": 4096, "media_type": "text/markdown" } ],
    "input_schema": { "type": "object", "required": ["topic"] },
    "output_schema": { "type": "object", "required": ["summary"] }
  }
}
```

The envelope may arrive through **exactly one** of two channels:

1. the typed `envelope` field (validated, then serialized into
   `__task_envelope` of the merged variables), or
2. already serialized at `__task_envelope` in the `variables` map — the
   worker-sdk wire format (`TaskOutput::build` /
   `EngineClient::complete_with_envelope` produce exactly these bytes; a
   hand-rolled client may do the same).

Both channels at once, or a non-envelope value at `__task_envelope`, is
rejected with 4xx `reserved_variable_key`. A well-formed but invalid
envelope (either channel) is rejected with 4xx `task_envelope_invalid`
(stable sub-code in the message, §10) **before** the store mutates the
task — the task stays locked and completable. The E2-3 caps and envelope
checks then run on the merged map, so the serialized envelope value is
size-capped like any other variable.

No `envelope` field and no `__task_envelope` variable ⇒ byte-identical
legacy behavior.

`__task_envelope` is reserved at **all** variable write boundaries: a
user-supplied value at that key on start-instance or user-task complete is
always rejected with 4xx `reserved_variable_key` (those boundaries have no
envelope semantics).

### fail — recorded, and (since E3-2) behavioral for `permanent`

`POST /api/v1/external-tasks/:task_id/fail` gains two optional fields:

```json
{
  "worker_id": "worker-1",
  "error": "model over quota",
  "retry_after_ms": 5000,
  "category": "permanent",
  "envelope": { "schema_version": 1, "execution_id": "<task_id>:0", "idempotency_key": "sha256:..." }
}
```

- `category` must be `"transient"` or `"permanent"` (else 4xx
  `invalid_retry_category`).
- `envelope` is validated exactly like on complete (4xx
  `task_envelope_invalid`); a given `category` is recorded into the
  envelope's `retry_category`.
- **E3-2 semantics: the category is BEHAVIORAL.** `permanent` skips the
  retry loop — the task goes straight to `Failed` with the DLQ +
  `TokenFailed` path (exactly like retry exhaustion) and is never
  redelivered. `transient` (or absent) retries as before. The category
  is additionally carried for observability and DLQ triage; see
  `docs/worker-context.md` §4.

## 8. The fail path (design decision, refined by E3-2)

Read the code first, then decide: the storage trait's
`ExternalTaskStore::fail(task_id, worker_id, error, retry_after,
permanent)` has no variables channel. E3-1 did not extend the store
trait; E3-2 added ONE parameter — `permanent: bool` — rather than a
`fail_permanent` method, so the guards (locked state, owner check) exist
in exactly one place per adapter. Therefore:

- **Category** ⇒ persisted as a **structured JSON prefix** on the error
  message: `{"retry_category":"permanent"}: model over quota`. This
  survives every existing persistence path — `task.error_message`, the
  `ExternalTaskFailed` history payload, the dead letter entry, and the
  `TokenFailed` reason — with zero schema changes. Parse it by splitting
  on the first `": "`. **Behavior**: the `permanent` flag routes the
  store call to the skip-retries path (task straight to `Failed`, no
  retry decrement, no requeue; DLQ + `TokenFailed` fire exactly as retry
  exhaustion does).
- **Envelope** ⇒ validated, then recorded in the `ExternalTaskFailed`
  history payload under `variables.__task_envelope`, redacted with the
  same `HistoryRedactionPolicy` the engine applies to every history
  variable. It is deliberately **not** embedded in the error message
  (schemas can be multi-KB and that path bypasses history redaction) and
  **not** merged into the task's variables (no store channel; merging
  would also leak the failed attempt's envelope into a redelivered task's
  inputs).
- The dead letter entry keeps the task's **input** variables only.

Honest limitation: after a *failure*, the envelope is audit-visible in
history but not in the task row or instance variables; the variables
channel for the envelope is `complete` (§6).

## 9. `retry_category` semantics (E3-2: behavioral)

| Value | Meaning | Effect |
|-------|---------|--------|
| `transient` | may resolve on retry (network, rate limit, timeout) | retry loop as before: decrement → requeue → at 0 `Failed` + DLQ |
| `permanent` | will not resolve on retry (bad input, policy refusal) | **retry loop skipped**: straight to `Failed` + DLQ + `TokenFailed`, no redelivery, retries not decremented |

Recorded locations: the envelope (`retry_category`), the error message
prefix (§8), history, and the DLQ entry's error message. Verified by
`crates/server/rest/tests/worker_context.rs`
(`permanent_failure_skips_retries_dlq_and_token_failed`).

## 10. Error codes (stable, REST 4xx)

| Boundary code | When | Message carries |
|---------------|------|-----------------|
| `task_envelope_invalid` | envelope failed validation (either channel, complete or fail) | the sub-code, e.g. `ai_task_envelope_invalid_artifact_ref` |
| `reserved_variable_key` | user-supplied `__task_envelope` at start-instance / user-task complete; non-envelope value at the key or dual-channel supply at external-task complete | the key name and the allowed channel |
| `invalid_retry_category` | `category` on fail is not `"transient"`/`"permanent"` | the offending value |

Envelope sub-codes (`AiTaskEnvelopeError::code`, also embedded in
`task_envelope_invalid` messages):

| Sub-code | Violation |
|----------|-----------|
| `ai_task_envelope_unsupported_version` | `schema_version` ≠ 1 |
| `ai_task_envelope_malformed` | value at `__task_envelope` does not parse into an envelope (plain string, ciphertext, unsupported `__var:` version, artifact envelope, invalid JSON, missing/unknown field) |
| `ai_task_envelope_invalid_artifact_ref` | artifact ref failed E2-3 URI/metadata validation |
| `ai_task_envelope_invalid_model_ref` | empty `model.provider`/`name`/`version` |
| `ai_task_envelope_invalid_tool_ref` | empty `tools[].name`/`version` |
| `ai_task_envelope_invalid_execution_id` | empty `execution_id` |
| `ai_task_envelope_invalid_idempotency_key` | empty `idempotency_key` |
| `ai_task_envelope_invalid_timeout` | `timeout_secs` = 0 |
| `ai_task_envelope_invalid_schema` | `input_schema`/`output_schema` not a JSON object |
| `ai_task_envelope_invalid_cost` | empty or > 8-byte `cost.currency` (numeric fields are u64 — non-negative by construction; a negative JSON number is rejected at parse time as `ai_task_envelope_malformed`) |

Error body shape is the existing `ErrorResponse` with the optional `code`
field, e.g.:

```json
{
  "error": "ai_task_envelope_invalid_artifact_ref: invalid artifact ref: artifact URI must be absolute …",
  "code": "task_envelope_invalid"
}
```

## 11. Versioning

- `schema_version` gates the envelope JSON: this build accepts **only 1**;
  any other value is `ai_task_envelope_unsupported_version`. Unknown
  fields are rejected (`deny_unknown_fields`), so a v2 envelope fails
  loudly instead of being misread.
- The envelope rides an E2-3 `__var:1:` **value** envelope, so the value
  format versioning applies as well: a future `__var:2:` at
  `__task_envelope` is treated as opaque (`Malformed` on read,
  `__opaque:` in history), never interpreted.
- Evolution is intentionally conservative: because `deny_unknown_fields`
  has been on since day one, a STRUCTURAL change (renaming/removing
  fields, changing types, making an optional field required) bumps
  `schema_version`, and this build reports the old version as
  `ai_task_envelope_unsupported_version` instead of guessing.

**Additive optional field — the one sanctioned evolution path (ADR-007
style, first exercised by E6-1's `cost`):** a NEW optional field MAY be
added to envelope v1 without bumping `schema_version`, provided

1. it is optional with a default (`cost` is `Option` — a pre-E6-1 writer
   produces envelopes without the key and they parse unchanged),
2. this build's readers accept old envelopes WITHOUT the field (they do
   — verified by the `old_envelope_without_cost_still_parses` test), and
3. the forward-compat tradeoff is accepted and documented: an OLDER
   strict reader (`deny_unknown_fields`) will REJECT a new envelope
   carrying the field as `ai_task_envelope_malformed` rather than
   misread it — lossless rejection, never silent misinterpretation.
   A worker that needs to talk to an older engine simply omits the
   field.

This mirrors the E2-3/ADR-007 value-envelope evolution: additive and
optional on the wire, strict everywhere else.
