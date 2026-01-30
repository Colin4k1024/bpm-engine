# bpm-bpmn

BPMN 2.0 XML parser and compiler to the engine’s ProcessDefinition.

## Role

- Parse BPMN 2.0 XML into an internal model.
- Compile to `bpm_core::ProcessDefinition` (nodes, flows, start node).
- Supports a **subset** of BPMN: StartEvent, EndEvent, UserTask, ServiceTask, ExclusiveGateway, ParallelGateway (fork/join). Not supported: SubProcess, CallActivity, boundary events, multi-instance, ScriptTask, etc.

See docs for the full mapping.

## Usage

Used by the REST server’s deploy endpoint and by examples that load BPMN. The engine runtime is BPMN-agnostic and only consumes ProcessDefinition.

## Documentation

See [docs/bpmn-spec-mapping.md](../../docs/bpmn-spec-mapping.md) and [docs/architecture.md](../../docs/architecture.md).
