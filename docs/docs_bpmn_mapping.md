# BPMN 2.0 compatibility mapping (plan v2.0 C.1)

This document describes how BPMN 2.0 elements map to the engine's internal model.

## Supported elements

| BPMN 2.0 element       | Engine node type   | Notes |
|------------------------|--------------------|--------|
| StartEvent             | Start              | Single start node per process. |
| EndEvent               | End                | |
| UserTask               | UserTask           | |
| ServiceTask            | ServiceTask        | Requires `handler_ref` or extension to map to registered handler. |
| ExclusiveGateway       | ExclusiveGateway   | Outgoing sequence flows with conditions (VariableEq / Expression / Default). |
| ParallelGateway (fork)  | ParallelFork       | One incoming, N outgoing. |
| ParallelGateway (join) | ParallelJoin       | N incoming, one outgoing; `expected` = number of incoming flows. |

## Not supported (v2.0)

- SubProcess, CallActivity
- Event subprocesses, boundary events
- Timer events, message events (engine has separate Timer mechanism)
- Multi-instance (loop)
- ScriptTask (use ServiceTask + handler instead)

## JSON structure (minimal BPMN-like)

The engine accepts a BPMN-like JSON with:

- `id`, `start` (start node id), `nodes`
- Each node: `id`, `type` (e.g. "StartEvent", "ServiceTask"), optional `handler_ref` for ServiceTask, `outgoing` (list of flow ids or target node ids)
- Flows: either inline on nodes as `outgoing_edges` with `target` + `condition`, or separate `flows` array

Conversion: BPMN JSON → internal DSL or directly to ProcessDefinition via [dsl::to_process_definition](src/dsl/convert.rs) after mapping element types and flows.
