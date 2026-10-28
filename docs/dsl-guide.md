---
layout: default
---

# BPM DSL Guide (v1)

> The DSL is the JSON entry point for defining processes — aimed at AI agents
> and generators that should not hand-write BPMN 2.0 XML. It compiles through
> the **same** intermediate model and compiler as BPMN XML, so both entries
> share one validation and execution semantics. Design decisions: [ADR-006](adr/ADR-006-minimal-declarative-dsl.md).

---

## JSON Schema 的边界

`crates/dsl/schema/bpm-dsl-v1.json` 是 **shape 层**校验：字段名、类型、必填根字段、
未知字段拒绝（`additionalProperties: false`）。跨字段规则（节点 id 唯一、边引用存在、
`serviceTask` 必须带 `task_type`、`timerIntermediateCatch` 必须带 `timer`、
`condition` 与 `default` 互斥等）**不在 schema 中表达**，由编译器的结构层校验负责，
错误以带 `path` 的 `DslError` 返回。生成定义的 Agent 应预期：通过 schema 的文档
仍可能被编译器以结构层错误拒绝。

## What DSL v1 covers

| Node type | Required fields | Optional fields |
|-----------|-----------------|-----------------|
| `start` | `id`, `type` | `name` (exactly one start per process) |
| `end` | `id`, `type` | `name` (at least one end per process) |
| `serviceTask` | `id`, `type`, `task_type` | `name`, `retries` (default 3), `timeout_secs` (default 60) |
| `userTask` | `id`, `type` | `name`, `form_key`, `form_fields` |
| `exclusiveGateway` | `id`, `type` | `name` |
| `parallelGateway` | `id`, `type` | `name` (fork: 1 in / 2+ out, join: 2+ in / 1 out) |
| `timerIntermediateCatch` | `id`, `type`, `timer.duration` | `name` |

Not in v1 (rejected with `unknown_field`): boundary events, call activities,
subprocesses, message/signal events, terminate ends. Future node types are
gated by `schema_version`.

Edges connect node ids and may carry a condition or a `default` flag (never
both):

```json
{ "source": "xor", "target": "end-yes",
  "condition": { "kind": "variableEq", "variable": "approved", "value": "true" } }
{ "source": "xor", "target": "end-no", "default": true }
{ "source": "xor", "target": "end-big",
  "condition": { "kind": "expression", "expression": "amount > 100" } }
```

- `variableEq` compares a variable for exact string equality. `variable` must
  not contain double quotes, `=`, or whitespace; `value` must not contain
  double quotes — use `expression` for those cases.
- `expression` is the engine's EL (supports `==`, `!=`, `>`, `<`, `>=`, `<=`,
  `and`, `or`); it passes through verbatim like a BPMN `conditionExpression`
  (an optional `${...}` wrapper is stripped).
- `default: true` marks the flow taken when no condition matches — the same
  effect as a BPMN `exclusiveGateway default` attribute.
- Edge `id` is optional (auto-generated as `edge_<index>`); duplicates are
  rejected.

`timer.duration` is an ISO 8601 duration string passed through unchanged. The
runtime parses `PT{N}S`, `PT{N}M`, `PT{N}H` (e.g. `"PT30S"`, `"PT1H"`);
unsupported formats fall back to 60 seconds.

`process.id` becomes the compiled definition id — the same behavior as the
BPMN `process id`.

## A full example

```json
{
  "schema_version": 1,
  "process": { "id": "order-flow", "name": "Order Flow" },
  "nodes": [
    { "id": "start", "type": "start" },
    { "id": "charge", "name": "Charge Payment", "type": "serviceTask",
      "task_type": "payment", "retries": 5, "timeout_secs": 120 },
    { "id": "decide", "type": "exclusiveGateway" },
    { "id": "review", "type": "userTask", "form_key": "review-form",
      "form_fields": [
        { "id": "reason", "label": "Reason", "type": "string", "required": true }
      ] },
    { "id": "wait", "type": "timerIntermediateCatch", "timer": { "duration": "PT1H" } },
    { "id": "end-ok", "type": "end" },
    { "id": "end-review", "type": "end" }
  ],
  "edges": [
    { "source": "start", "target": "charge" },
    { "source": "charge", "target": "decide" },
    { "source": "decide", "target": "end-ok",
      "condition": { "kind": "variableEq", "variable": "approved", "value": "true" } },
    { "source": "decide", "target": "review", "default": true },
    { "source": "review", "target": "wait" },
    { "source": "wait", "target": "end-review" }
  ]
}
```

Form fields lower to the engine's `FormField`: `label` defaults to `id`,
`type` is one of `string` / `number` / `boolean` / `enum` (default `string`),
`required` defaults to `false`, and `options` lists enum values.

## Compiling from Rust

```rust
use bpm_engine_dsl::compile;

match compile(json) {
    Ok(def) => { /* bpm_engine_core::ProcessDefinition, register and run */ }
    Err(report) => {
        for e in &report.errors {
            println!("[{}] {}: {}", e.code, e.path, e.message);
        }
    }
}
```

`bpm_engine_dsl::schema_json()` returns the JSON Schema (draft 2020-12) for
the document format; the committed copy is
[`crates/dsl/schema/bpm-dsl-v1.json`](../crates/dsl/schema/bpm-dsl-v1.json)
(regenerate with `cargo run -p bpm-engine-dsl --example dump_schema`). A
lockstep test fails the build if the file drifts from the types.

## Error codes and paths

Every error carries a stable snake_case code and a JSON-pointer-style `path`
pointing into your document. All structural and semantic errors are collected
(not fail-fast); the JSON-shape layer reports at most one error because
deserialization stops at the first syntax/data problem.

| Code | Path example | Meaning |
|------|--------------|---------|
| `json_parse` | `/` | Input is not valid JSON |
| `schema_version_unsupported` | `/schema_version` | `schema_version` is not `1` |
| `unknown_field` | `/nodes/0/taskTypo` | Field not part of the schema (typos) |
| `missing_field` | `/nodes/1/task_type` | Required field absent (incl. per-type requirements) |
| `invalid_type` | `/nodes/0/retries` | Wrong field type, unknown node type, empty id, field not valid for the node type, unsafe condition value, `condition` + `default` conflict |
| `duplicate_node_id` | `/nodes/2/id` | Two nodes share an id (first occurrence noted) |
| `duplicate_edge_id` | `/edges/1/id` | Two edges resolve to the same id |
| `unknown_node_ref` | `/edges/1/source` | Edge references a node id that does not exist |
| `empty_node_list` | `/nodes` | `nodes` is empty |
| `compile` | `/nodes/3` | Semantic error from the shared BPMN compiler (start/end counts, orphan nodes, gateway shapes, dead ends); `sourceCode` carries the upstream code such as `NoEndEvent` |

## Relationship to the BPMN entry

- `bpm_engine_dsl::compile(json)` lowers the document onto the same
  `BpmnProcess` intermediate model the BPMN XML parser produces and calls
  `bpm_engine_bpmn::compile` verbatim — one semantics, one validator.
- An equivalent BPMN document compiles to a structurally identical
  `ProcessDefinition` (enforced by the parity test
  `crates/dsl/tests/parity_bpmn.rs`).
- Defaults match the BPMN parser: `retries = 3`, `timeout_secs = 60`,
  form-field `label = id`, `type = string`, `required = false`.
- The DSL is stricter where BPMN is silent (e.g. `task_type` is required
  instead of defaulting to `"default"`, an edge cannot be both conditional
  and default) so generators get precise feedback instead of silent
  surprises.

REST deployment of DSL documents lands with the API-contract issues
(see [ADR-006](adr/ADR-006-minimal-declarative-dsl.md)); pre-deploy static
validation and dry-run of DSL documents is available now via
`POST /api/v1/process-definitions/validate` and `/dry-run` with
`format: "dsl"` (see [validation-and-dry-run.md](validation-and-dry-run.md)).
