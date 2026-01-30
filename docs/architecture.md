# Architecture Overview

> This document describes the **runtime architecture** of the Rust BPM Engine.
> It focuses on **execution semantics**, not modeling tools or BPMN specifications.

---

## Scope

This architecture document defines:

- How workflows are executed at runtime
- How state progresses via events
- How concurrency, failure, and recovery are handled

It explicitly does **not** cover:

- BPMN XML specifications
- Visual modeling or low-code tooling
- UI concerns

---

## Core Abstractions

### Process Definition

A static graph describing the workflow structure.

- Nodes (Start, Service, User, Gateway, End)
- Sequence flows

### Process Instance

A running instance of a process definition.

- Holds variables
- Owns tokens
- Has a lifecycle (Running / Completed / Terminated)

### Token

> **Token is the unit of execution.**

A token represents execution authority at a specific node.

- Multiple tokens enable parallelism
- Tokens advance independently

---

## Execution Model

- Tokens move through the process graph
- Nodes are executed by consuming a token
- Execution produces events

All state transitions are driven by **events**, not direct calls.

See: `execution-model.md`

---

## Event-driven Core

- Engine reacts to immutable events
- Event handlers are deterministic and transactional
- Handlers may emit new events

This guarantees:

- Observability
- Replayability
- Crash safety

---

## Concurrency Model

- Concurrency is **token-scoped**
- A token can only be executed by one executor at a time
- Optimistic locking (CAS) prevents conflicts

Parallel join is protected by uniqueness constraints.

---

## Failure & Compensation

- Failures are first-class events
- Long-running consistency is achieved via Saga
- Compensation executes in reverse order

See: `saga.md`

---

## Crash Recovery

- All engine state is persisted
- Tokens can be rehydrated after crashes
- Execution always continues forward

See: `recovery.md`

---

## Design Principles

- Token over thread
- Event over call stack
- Compensation over rollback
- Persistence over memory

---

This document serves as the **contract between architecture and implementation**.
