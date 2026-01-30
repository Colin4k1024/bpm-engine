# bpm-core

Core semantics of the BPM engine: process, token, node types, events, and saga.

## Role

- **Pure logic, no I/O.** This crate defines data structures and state transitions only.
- ProcessDefinition, ProcessInstance, Token, NodeType (Start, End, UserTask, ServiceTask, ExternalTask, gateways).
- EngineEvent and event-driven transitions.
- Saga and compensation ordering.

## Usage

Used by `bpm-runtime`, `bpm-storage`, `bpm-bpmn`, and the REST server. Do not change casually.

## Documentation

See [docs/architecture.md](../../docs/architecture.md) and [docs/execution-model.md](../../docs/execution-model.md).
