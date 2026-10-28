---
layout: default
---

# Approval, Pause & Cancel Semantics (E5-2, issue #24)

> The user-side reference for engine-owned approvals, instance-level
> pause/resume, and cancel. The normative design record is
> **[ADR-008](adr/ADR-008-approval-pause-cancel-semantics.md)** — this
> document mirrors its decisions for operators and worker/agent authors.
> Contract surfaces: `docs/openapi.yaml` (paths, error codes) and the agent
> tool catalog (`GET /capabilities`), both locked by the CI consistency
> suite.

Related: `docs/tool-security.md` (the worker-side approval gate that reads
the engine-written truth), `docs/rbac.md` (roles), `docs/recovery.md`
(lease semantics during pause).

---

## 1. Which steps require approval (v1)

The DSL/BPMN has no approval flag yet, so v1 configures the gate by task
type:

```toml
[engine]
approval_required_task_types = ["payment_charge", "audit"]   # TOML array
```

or equivalently as a comma-separated string (the env form):

```sh
BPM_ENGINE__APPROVAL_REQUIRED_TASK_TYPES=payment_charge,audit
```

Default: **empty** — no gate, byte-identical pre-E5-2 behavior (this is
why every existing suite stays green).

A step is approval-required when its external task's `task_type` is in
that set, **or** an approval record already exists for the token's visit
of the node (the recovery/re-delivery case — the record is the state, a
re-arrival never creates a second requirement).

## 2. Approval happens BEFORE the task exists

Task-creation-after-approval (ADR-008 §2): when a token arrives at an
approval-required node, the engine persists a **Pending `ApprovalRecord`**
and the token WAITS (the user-task waiting pattern). **No external task
row exists** until an operator approves — workers literally cannot see
unapproved work; `fetch-and-lock` has no approval filtering because there
is nothing to filter.

| Arrival state of the record | Engine behavior |
|---|---|
| none (first arrival, gated type) | create Pending record; token waits; no task; `ApprovalCreated` history |
| `PENDING` | token keeps waiting |
| `APPROVED` | (re-)arrival creates the task with `__approval=true` |
| `REJECTED` | never creates a task (the reject path already failed the token) |

### Endpoints

| Endpoint | Role | Effect |
|---|---|---|
| `GET /api/v1/process-instances/:id/approvals` | all (read) | list the instance's approval records (discover pending ids here) |
| `POST /api/v1/approvals/:approval_id/decide` `{decision: "approve"\|"reject", note?}` | operator | decide a Pending record |

`approve` **creates the fetchable external task**, with the task variable
`__approval="true"` **written by the engine** (see §3). `reject` dead
letters the step and fails the token (see §4). Both append a history event
carrying the verified actor (`sub` claim) and the optional note.

### Decision vs. effect: the re-drivable unit

The decision and its effect are not one transaction, but a recoverable
unit (ADR-008 §7.1):

- `decide` commits the **decision durably** first (the record flips to
  `APPROVED`/`REJECTED`; an internal effect flag stays *pending*), then
  applies the effect (task creation on approve; dead letter + token
  termination on reject) and confirms it.
- If applying the effect **fails** (e.g. a storage error), the route
  answers **500 `internal`** naming the approval id: the decision is
  recorded and its effect is pending — **retry the SAME decide call**,
  which re-drives the effect.
- A retried same-decision call replays **200** and, if the effect was
  still pending (a crash between the decision and its application), the
  replay **re-drives the effect before answering** — self-healing.
- On server start a sweep re-drives every decision whose effect is still
  pending (meaningful for the PostgreSQL driver; the memory store is
  process-lifetime dev/test state that starts empty).
- The effect is **idempotent**: a re-drive never creates a second task,
  never dead-letters twice, and heals a missing decision history event.

## 3. `__approval` is engine-written truth on gated steps

The E3-3 worker-side gate (`bpm_engine_worker_sdk::tools`,
`docs/tool-security.md` §3.1) reads the task variable `__approval`. From
E5-2 on, for approval-required task types, that value is what the ENGINE
injected at task creation — the variable channel cannot forge it:

- a client-supplied `__approval` in start/user-task variables is still
  accepted (backward compatibility — the ai-reference flows rely on it),
  but the engine **overwrites** it with its own truth when the gated task
  is created (engine truth wins);
- un-gated task types keep the E3-3 worker-side convention unchanged
  (upgrade a type to the config set to move it behind the engine gate).

## 4. Reject semantics

The first `decide(reject)`:

1. appends `ApprovalRejected` history (actor + note);
2. inserts a **dead letter entry** — `error_message` =
   `approval rejected by <actor>: <note>`, `task_id` carrying the approval
   record id (this step never had an external task row);
3. terminates the token (`TokenFailed` is recorded; operator-forced
   transition in the core state machine).

The **instance keeps running** its remaining branches — exactly the E3-2
permanent-failure pattern; the DLQ entry is the operator's lever. A DLQ
requeue is an explicit operator override of the approval gate; the
requeued task does **not** carry `__approval`, so the worker-side gate
stays fail-closed.

A rejection racing (or following) an instance **cancel** is safe: the
reject effect runs under the instance lifecycle lock and reloads the
instance inside it — a cancelled (`TERMINATED`) instance is never
resurrected to `RUNNING`; the rejection is recorded (one dead letter +
the `ApprovalRejected` history entry) with no instance mutation, and the
token is terminated exactly once.

## 5. Pause / resume (instance level)

- `POST /api/v1/process-instances/:id/pause` (operator): every
  non-terminal token → `Suspended` (version-bumped). The instance state
  stays `RUNNING`; the response's `paused: true` is derived from the token
  states. `InstancePaused` history event with the actor.
- `POST /api/v1/process-instances/:id/resume` (operator): restores the
  **pre-pause semantics** per node kind — tokens at Waiting-steady-state
  nodes (timer/message/signal intermediate catch, user task, external
  task, boundary event, call activity) return to `Waiting`; every other
  token returns to `Ready`. `InstanceResumed` history event.

**Honest pause semantics** (the in-flight-lock decision, ADR-008 §5):

- **Leases continue.** Pause does not revoke or freeze external-task
  leases; workers keep their locks and may `extend-lock`.
- **Transitions are blocked.** `complete`/`fail` on external tasks and
  user-task `complete` are refused with **409 `instance_paused`** while
  any token of the instance is Suspended; the task stays Locked. After
  resume the worker retries the completion (or, if the lease expired
  first, the task is re-delivered — the existing at-least-once semantics).
- **Timers that fire during the pause are RESCHEDULED, not consumed.**
  The timer record is re-inserted with `due_at = now + ~1s` and stays
  `Scheduled` until the token is un-suspended; after resume the catch
  token is back to `Waiting` and the timer fires normally. **The deadline
  survives the pause** — pausing postpones a timer, it never drops it.
- **Messages/signals delivered while paused are DROPPED (v1).** A message
  or signal whose catch token is `Suspended` is a transient event with no
  buffer in v1: the delivery is not consumed and not queued — the sender
  retries (fires the message/signal again), or the operator resumes
  first and the next delivery is consumed. Pinned by tests
  (delivered-while-paused → not consumed; delivered after resume →
  consumed). A delivery buffer is a recorded v2 item (ADR-008 §5).

## 6. Cancel (instance level)

`POST /api/v1/process-instances/:id/cancel` (operator): reuses the
TerminateEnd semantics — every non-terminal token → `Terminated`,
instance → `Terminated`, a `ProcessTerminated` engine event and an
`InstanceCancelled` history event (with the actor).

After cancellation, task operations on the instance are rejected with
**409 `instance_cancelled`**: external-task `complete`/`fail`/
`extend-lock` and user-task `complete`. Honest boundary: `fetch-and-lock`
does not filter tasks of cancelled instances in v1 — a worker may fetch
such a task but cannot complete it; the work is discarded (idempotent
side effects remain the tool author's responsibility).

## 7. Only-once and recovery (the key acceptance)

| Operation | Race / retry | Result |
|---|---|---|
| approve | concurrent or retried decide | the store's `decide` flips the record exactly once; same-decision retry → **200 replay** with the recorded outcome (original note/actor) — re-driving a still-pending effect first; conflicting decision → **409 `approval_already_decided`**; ONE task created |
| reject | concurrent or retried decide | same contract; the token terminates exactly once |
| reject vs. cancel | concurrent cancel + reject | both serialize on the instance lifecycle lock: the instance ends `TERMINATED` (never resurrected), the rejection is recorded exactly once, the token terminates exactly once |
| effect failure | decide commits, effect application fails | **500 `internal`** naming the approval id; the decision stays durable with a pending effect — retry the SAME decide call (re-drive), or restart (the startup sweep re-drives) |
| pause | double / concurrent | 200 replay; one `InstancePaused` event |
| resume | double | 200 no-op replay |
| cancel | double / concurrent | 200 replay with the terminal state; exactly one `InstanceCancelled` and one `ProcessTerminated` event |

Mechanism: `ApprovalStore::decide` is atomic (memory: one write-lock
critical section; Postgres: `UPDATE ... WHERE status = 'Pending'`), and
the effect application, pause/resume/cancel check-then-act windows run
under a process-wide async mutex (`AppState::instance_lifecycle_lock`) —
single-service only-once. **Multi-replica deployment needs DB-level
conditional updates; explicitly out of scope for v1** (ADR-008 §7).

Approval persistence across retries: a record is subject-unique on
`(instance, node, token)` — transient task retries (requeue → re-fetch)
do not re-enter the arrival gate, so **no re-approval** is needed; the
approval is consumed at first task creation and the record stays
`APPROVED` for audit. The decision/effect re-drivable unit (§2) covers
the crash window between the durable decision and its application:
a retried decide re-drives, the startup sweep re-drives, and the effect
is idempotent (never a second task or dead letter).

## 8. History explains everything

Route-layer history events (same mechanism as `ExternalTaskLocked`):
`ApprovalCreated` (engine gate), `ApprovalGranted` /
`ApprovalRejected` (decide, with actor + note), `InstancePaused` /
`InstanceResumed` / `InstanceCancelled` (lifecycle, with actor). The
`GET .../history` categories map them to `approval` / `instance`.

These are NOT `EngineEvent`s (deliberately — see ADR-008 §8), so replay
sessions do not replay approval decisions: an approval-required step
replays as immediate task creation in the simulation.

## 9. Error codes

| Code | HTTP | When |
|---|---|---|
| `approval_not_found` | 404 | unknown approval id (cross-tenant access answers identically — no existence leak) |
| `approval_already_decided` | 409 | a CONFLICTING decide on a decided record (same-decision retry is a 200 replay) |
| `instance_paused` | 409 | external-task complete/fail or user-task complete while paused |
| `instance_cancelled` | 409 | task operations on a cancelled instance |
| `instance_not_pausable` | 409 | pause/resume on a terminal instance |

## 10. Configuration reference

| Setting | Default | Effect |
|---|---|---|
| `engine.approval_required_task_types` | `[]` | task types behind the engine approval gate; TOML array or comma string; env `BPM_ENGINE__APPROVAL_REQUIRED_TASK_TYPES` |

## 11. Out of scope (v1)

Per-DSL-node approval flags, multi-approver/quorum approvals,
fetch-filtering of cancelled instances' tasks, boundary timers on
approval-waiting steps, pause lease freezing, **a buffer for
messages/signals delivered while paused** (v1 drops them — see §5; a
buffer is the recorded v2 item), and distributed (multi-replica)
only-once. See ADR-008 "非目标".
