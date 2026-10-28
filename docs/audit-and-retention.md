# Audit & Data Retention (E5-3, issue #25)

> 交付与验收：操作人、定义版本、决策和产物引用可追溯；配置保留期/删除与脱敏，回放不泄露密钥。

This document describes the audit model of the BPM engine: **what is
traceable**, the **audit endpoint** contract, the **retention** policy and
its deliberate scope, and the **secrecy guarantees** (audit and replay never
expose secrets). It is the honest reference for what v1 does and does not
record.

Related: `docs/rbac.md` (who may call what), `docs/variables-and-artifacts.md`
(E2-3 envelopes and encryption), `docs/approval-semantics.md` (E5-2 decisions),
`docs/trace.md` (the step timeline), `docs/recovery.md` (replay/crash recovery).

## 1. The audit model: who did what

The engine's own state transitions (`TokenArrived`, `TokenCompleted`,
`TimerFired`, …) are performed by the engine and have **no human actor by
design** — `crates/core` stays actorless. The **verified identity**
(E5-1: the JWT `sub` claim under JWT auth; `"api-key"` / `"anonymous"` in the
other modes) is persisted at the *boundary*: the REST route layer writes
dedicated **boundary events** into the instance's history, each carrying an
`actor` field.

| Boundary event | Written by | Actor | Extra context |
|---|---|---|---|
| `InstanceStarted` | `POST /process-instances` | the starter | `process_def_id` |
| `TaskCompleted` | `POST /tasks/:task_id/complete`, `POST /external-tasks/:task_id/complete` | the completer | `task_kind` (`user`\|`external`), `task_id`, `node_id`, `worker_id` (external) |
| `ExternalTaskLocked` | `POST /external-tasks/fetch-and-lock` | the verified principal | `worker_id` (the client-claimed lease owner) |
| `ExternalTaskFailed` | `POST /external-tasks/:task_id/fail` | the verified principal | `worker_id`, `error_message` |
| `InstancePaused` / `InstanceResumed` / `InstanceCancelled` | lifecycle routes (E5-2) | the operator | suspended/resumed/terminated token counts |
| `ApprovalGranted` / `ApprovalRejected` | the decide route (E5-2) | the decider (the durable record's `decided_by`) | `approval_id`, `node_id`, `note` |
| `DeadLetterRequeued` / `DeadLetterDeleted` | DLQ routes | the operator | `dead_letter_id`, `task_id` |

Boundary events are plain history rows: they show up in
`GET /process-instances/:id/history` (with their category), are **not**
replayable event types (replay is unaffected), and are joined into
`/process-instances/:id/audit`. The `actor` is the *verified* principal —
for workers, `worker_id` (client-claimed) and `actor` (verified) are recorded
side by side and may differ.

**Definition lifecycle actions** (`deploy`, `activate`, `deprecate`) carry
**no instance-scoped history rows** — history is instance-scoped (the
Postgres schema enforces a foreign key on `instance_id`), and those actions
have no instance. Their traceability is the structured server log
(`process definition deployed/activated/deprecated` with `definition_id`
and `actor`). A definition-level audit table is future work.

### What is traceable (v1)

- **操作人 (actor)**: on every boundary event above — start, task
  completions, locks/fails, pause/resume/cancel, approval decisions, DLQ
  requeue/delete.
- **定义版本 (definition version)**: the instance's `process_def_id` IS the
  versioned id (`key:version`) — inherent, immutable, and joined with the
  definition record's current status by the audit endpoint.
- **决策 (decisions)**: approval decisions carry actor + note (the E5-2
  durable record); the audit endpoint surfaces them in a `decisions` array.
- **产物引用 (artifact references)**: `__var:1:` artifact envelopes in the
  instance's history/variables are extracted read-only (uri, sha256, size,
  media_type) into the audit `artifacts` array. The E3-1 AI task envelope's
  artifacts are surfaced by `/steps` (its envelope summaries).
- **Costs**: via `/steps` (E6-1 envelope summaries) — not part of the audit
  endpoint's response.

### What is NOT traceable (honest non-goals)

- Engine-internal transitions have no actor (the initiating actor is on the
  boundary event that caused them).
- Deploy/activate/deprecate have no history rows (log-only, see above).
- `POST /external-tasks/:task_id/extend-lock` has no actor-carrying history
  event: it is lease **maintenance** (renewing an existing lock whose owner
  was already attributed at fetch-and-lock in `ExternalTaskLocked`), never
  advances a token, and is actor-logged server-side only.
- Variables in audit output are **redacted per E2-3**: oversized values are
  `__truncated:` markers, unknown envelope versions are `__opaque:` — the
  audit shows markers, not values, **by design**.
- Definition-record status is the *current* status (a version deprecated
  after the instance ran shows `deprecated` — the historical status at
  execution time is not snapshot per event; the versioned id is).

## 2. The audit endpoint

```
GET /api/v1/process-instances/:id/audit
```

Read operation — every authenticated role may call it (`auditor` is the
primary consumer; see `docs/rbac.md`). Cross-tenant access is rejected as
404 `not_found` (the same no-existence-leak gate as every instance-scoped
read); an unknown instance answers 404 `not_found`; storage failures answer
500 `internal`.

Response (see `docs/openapi.yaml` → `AuditResponse`):

```jsonc
{
  "instance_id": "…",
  "instance": { "state": "COMPLETED", "tenant_id": "acme" },
  "definition": { "id": "order-flow:3", "key": "order-flow",
                  "version": 3, "status": "active" },
  "events": [
    { "sequence": 0, "id": "…", "event_type": "ProcessStarted",
      "category": "instance", "occurred_at": "1770000000",
      "payload": { "…" } },                       // no actor: engine-internal
    { "sequence": 1, "id": "…", "event_type": "InstanceStarted",
      "category": "instance", "occurred_at": "1770000000",
      "actor": "alice",                            // boundary: the starter
      "payload": { "instance_id": "…", "process_def_id": "order-flow:3",
                   "actor": "alice" } }
    // …
  ],
  "decisions": [
    { "approval_id": "…", "decision": "approved", "node_id": "charge",
      "actor": "bob", "note": "risk checked", "occurred_at": "1770000001" }
  ],
  "artifacts": [
    { "variable": "report", "uri": "https://…/r1.md",
      "sha256": "…", "size": 4096, "media_type": "text/markdown",
      "source": "history" }
  ]
}
```

Notes:

- **Events** are ordered with the same deterministic causal key as `/steps`
  (`(occurred_at, phase_rank, listing order)`), not the display-oriented
  `(occurred_at, id)` of `/history`.
- **payload is verbatim history**: already redacted at write time, never
  decrypted. `__encrypted:` values appear as ciphertext.
- **artifacts** dedup by `(uri, variable)`; `source` says whether the
  reference was seen in an event payload (`history`) or the current
  variables (`instance`). Redacted envelope values no longer parse and
  therefore do not appear.
- `definition.status` is absent when the definition record is gone (the
  instance may outlive its definition); the version is still reported from
  the id.

## 3. Retention (配置保留期/删除)

### Configuration

```toml
[audit]
retention_days = 0        # 0 (default) = keep everything — retention OFF
sweep_interval_secs = 3600
```

`retention_days` is the only knob that matters for behavior;
`sweep_interval_secs` bounds the periodic sweep cadence. Env overrides:
`BPM_AUDIT__RETENTION_DAYS`, `BPM_AUDIT__SWEEP_INTERVAL_SECS`.

**Default is OFF (`0`)** — the safe direction: deletion is irreversible and
can break replay/crash-recovery for old instances, so nothing is deleted
unless an operator opts in.

### What the sweep covers

| Store | Deleted by | Column |
|---|---|---|
| History events | `HistoryRepo::delete_older_than` | `occurred_at` |
| Dead letters | `DeadLetterStore::delete_older_than` | `created_at` |

- A sweep runs **at startup** (before serving) and then every
  `sweep_interval_secs` (a plain tokio interval task in `build_app`).
- Rows **strictly older** than `now - retention_days` are deleted. The
  timestamp comparison is **numeric** (both adapters cast/parse the
  unix-seconds TEXT column — a text compare would misorder `"999"` vs
  `"1000"`). Rows whose timestamp is not a unix-seconds number are **kept**
  (never delete what cannot be age-assessed). All SQL is parameter-bound.
- Retention is **global** (all tenants): it is a data-lifecycle policy, not
  a per-tenant query.
- **Idempotent**: a re-run deletes nothing new. Single-service honesty: with
  several engine processes over one database, each runs its own sweep —
  redundant, not harmful.
- `/ready` reports `checks.audit_retention = "ok"` when retention is
  enabled; the key is absent when disabled (the default needs no signal).
  Each sweep logs `retention sweep completed` with the cutoff and the
  deleted counts.
- **No admin endpoint in v1** — the sweep is automatic only; manual
  intervention is a database operation. (Documented decision, see the
  non-goals below.)

### What retention deliberately does NOT cover

- **Instances, tokens, definitions** are OPERATIONAL state — deleting a
  running instance is destructive (breaks crash recovery, in-flight leases,
  pending approvals). Out of scope by design.
- **Replay sessions** are in-memory and die with the process — nothing to
  sweep.
- **Approval records** follow their instance (operational state); their
  *decision* is already part of history (swept with it).
- Retention never rewrites rows — it only deletes whole aged history rows.
  Redaction (below) applied at write time is therefore the only
  content-level control.

## 4. Redaction & secrecy (脱敏 / 回放不泄露密钥)

Three layers, in the order data flows:

1. **At the write boundary (E2-3)** — `sensitive` variables must be
   `encrypted`; encryption happens before the engine sees the value, so
   history, DLQ, logs, and stores only ever hold ciphertext
   (`__encrypted:<nonce>:<ciphertext>`). Oversized values are replaced with
   `__truncated:<len>:<sha256-12>:` markers by the history redaction policy
   (`engine.history_variable_max_bytes`, default 1024); unknown envelope
   versions become `__opaque:`.

2. **The audit endpoint never decrypts** — it reads history and parses
   envelopes only. Encrypted values surface as ciphertext; oversized values
   as markers; redacted envelopes do not parse (so their artifact refs are
   absent). `GET /process-instances/:id` (and `/trace`) DO decrypt for the
   owner — the audit view deliberately does not.

3. **Replay (回放) exposes no variable material at all.** The replay
   endpoints answer with event types, timestamps, token/node ids and token
   states only — never payloads or variables (verified end-to-end by the
   `replay_snapshot_and_steps_never_expose_secrets` test of
   `crates/server/rest/tests/audit_retention.rs`, which asserts the RAW
   response bodies of create/step/seek/snapshot contain neither plaintext
   secrets nor even ciphertext). Replaying also cannot leak into real
   history: each step re-applies events to a throwaway shadow store.

In short: the only endpoints that ever reveal an encrypted variable's
plaintext are the owner-facing `GET /process-instances/:id` and `/trace`
(with `BPM_VARIABLE_ENCRYPTION_KEY` configured); audit and replay never do.

## 5. Verification

- `crates/server/rest/tests/audit_retention.rs` (memory mode, CI): the
  actor/definition/decision/artifact joins, the redaction markers, the
  replay-secrecy raw-body assertions, the retention sweep (aged-only
  deletion, idempotence, disabled default, `/ready`).
- `crates/adapters/memory` unit tests + `crates/adapters/postgres/tests/
  integration.rs::retention_delete_older_than_history_and_dead_letters`
  (Docker-gated): the store-level numeric deletes.
- `crates/server/rest/tests/openapi_contract.rs` + the regenerated
  capabilities catalog keep `docs/openapi.yaml` in lockstep with the
  router.
