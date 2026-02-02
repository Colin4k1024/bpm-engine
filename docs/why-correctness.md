# Why correctness first?

bpm-engine is a **correctness-first workflow execution kernel**: it prioritizes execution semantics, persistence, replay, and formal invariants over feature breadth or low-code UX.

This document contrasts bpm-engine with other workflow systems **on semantics and guarantees**, not on feature lists.

---

## Positioning

| Focus | bpm-engine | Camunda | Temporal | AWS Step Functions |
|-------|------------|---------|----------|---------------------|
| **Primary strength** | Execution semantics, invariants, replay | BPMN modeling, forms, Cockpit | Durable execution, SDKs | Serverless DAG, AWS integration |
| **Execution model** | Token state machine, persisted | Token/activity, persisted | Event-sourced workflow history | State machine, managed |
| **Replay / audit** | First-class: history + replay API | Audit log, Cockpit | Event history, replay in SDK | Limited |
| **Invariants** | Formal (token, join, external task), tested | Implicit in runtime | Implicit in runtime | Implicit |
| **External work** | Lease-based fetch-and-lock | Human / external tasks | Activities, heartbeats | Task token, Lambda |

---

## Semantic comparison (short)

- **Token semantics**: bpm-engine models execution as **tokens** with explicit lifecycle (Ready → Executing → Waiting / Completed / Terminated). State is persisted; every transition is recorded. Replay applies the same event sequence to reproduce state.

- **Join semantics**: Parallel join produces one token only when **all** incoming branches have arrived; count is fixed at compile time. Invariants and tests assert no partial join.

- **External task semantics**: One owner per task (lease); lock expiry returns the task to the pool. Retries are monotonic. History records Locked / Failed / Completed for trace and audit.

- **Crash recovery**: Progress is driven by persisted state (instance, tokens, history, timers). After a crash, the engine re-reads from storage and continues; replay can verify that replayed state matches.

---

## When to choose bpm-engine

Choose bpm-engine when:

- You need **clear, auditable execution semantics** and the ability to answer “why did this instance end up here?”
- You want **replay** (step-through or full) and **trace** (token timelines, external task history) as first-class APIs.
- You care about **formal invariants** (token exactly-once completion, join all-branches-required, external task single-owner) and want them tested in CI.

Choose Camunda when you need rich BPMN modeling, forms, and operational UIs. Choose Temporal when you want durable workflows with strong SDKs and less focus on BPMN. Choose Step Functions when you want a managed, serverless DAG on AWS.

---

## Summary

bpm-engine does not try to be “another full BPM suite.” It aims to be a **correctness-first execution kernel**: small, explicit, and built so that execution semantics are visible, replayable, and invariant-checked.
