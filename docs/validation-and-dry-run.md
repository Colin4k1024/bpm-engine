# Static Validation & Dry-Run (E2-2)

Pre-deploy safety gate for AI-agent-generated process definitions (BPMN 2.0
XML or the E2-1 JSON DSL): check the definition **before** deploying it, and
simulate it **without side effects**.

- Rust API: `bpm_engine_bpmn::validate_definition` / `bpm_engine_bpmn::dry_run`
  (pure functions over a compiled `ProcessDefinition`).
- REST API: `POST /api/v1/process-definitions/validate` and
  `POST /api/v1/process-definitions/dry-run`.

Recommended agent workflow: **validate → dry-run → deploy**.

```
1. POST /process-definitions/validate   { format, source }            → fix Critical findings / compile errors
2. POST /process-definitions/dry-run    { format, source, variables } → check the walk & terminus
3. POST /process-definitions/deploy     (BPMN XML body, unchanged)     → only when 1+2 look right
```

## Endpoints

### `POST /api/v1/process-definitions/validate`

Request:

```json
{
  "format": "bpmn",          // "bpmn" (BPMN 2.0 XML) or "dsl" (JSON DSL, E2-1)
  "source": "<the full document as a string>"
}
```

Responses (always HTTP 200 when the envelope is well-formed):

- Compile failure — compile errors are **data, not HTTP errors** (agents need
  to read them):

  ```json
  {
    "valid": false,
    "compile_errors": [ { "code": "NoStartEvent", "message": "..." } ]
  }
  ```

  `compile_errors` reuses each compiler's public DTO verbatim:
  - BPMN parse error: `{ "error": "parse error: invalid XML: ..." }`
  - BPMN semantic errors: array of `CompilerError` (PascalCase `code` such
    as `NoEndEvent`, plus `message`/`node_id`/`flow_id`/`hint` — same shape
    as the deploy endpoint returns on 400).
  - DSL errors: array of `DslError` (snake_case `code` like `unknown_field`,
    JSON-pointer `path`, `message`, optional `hint`/`sourceCode` — see
    `docs/dsl-guide.md`).

- Compile success — static findings:

  ```json
  {
    "valid": true,
    "findings": [
      { "severity": "Info", "code": "condition_variable_scope", "message": "..." }
    ]
  }
  ```

  `valid` is `true` iff **no finding is `Critical`**; warnings and info are
  included in `findings` but do not invalidate. Findings are deterministically
  ordered (severity, then code, then node id).

### `POST /api/v1/process-definitions/dry-run`

Same request shape, plus optional start variables:

```json
{
  "format": "bpmn",
  "source": "<the full document as a string>",
  "variables": { "x": "a" }
}
```

Response: the validate response plus the walk report (skipped when
compilation failed):

```json
{
  "valid": false,
  "findings": [ ... ],
  "dry_run": {
    "steps": [
      { "node_id": "start", "node_type": "start" },
      { "node_id": "gw", "node_type": "exclusiveGateway" },
      { "node_id": "task_a", "node_type": "externalTask", "detail": "work-a" },
      { "node_id": "end", "node_type": "end" }
    ],
    "terminus": { "type": "completedAt", "node_id": "end" },
    "unresolved_conditions": [],
    "max_steps": 100
  }
}
```

**Dry-run must not deploy and must not create any instance, task, timer, or
history record** (acceptance: dry-run 不产生实例). Two different kinds of
proof back this:

- **Timers and history: side-effect-free by construction.** The validate and
  dry-run handlers take no application state at all — no store handles, no
  `AppState` — and the simulator is a pure function over the compiled
  definition. No code path exists that could schedule a timer or append a
  history record, so this needs no test to hold.
- **Instances, tasks, and definitions: proven by test assertions.**
  `crates/server/rest/tests/validation_dry_run.rs`
  (`validate_and_dry_run_have_no_side_effects`) exercises both endpoints
  with both formats (success and failure cases) and then asserts that no
  process instance exists, no task exists, and no definition was deployed
  (only the bootstrap definitions remain) — plus a positive control proving
  a real deploy + start does create an instance.

### Malformed envelopes (4xx)

Only malformed request *envelopes* are 4xx, with a stable `code`:

| HTTP | `code` | Condition |
|------|--------|-----------|
| 400 | `invalid_request_body` | Body is not valid JSON for the request DTO |
| 400 | `invalid_format` | `format` is not `"bpmn"` or `"dsl"` |
| 400 | `empty_source` | `source` is empty / whitespace |

## Static finding codes

| Code | Severity | Meaning |
|------|----------|---------|
| `unreachable_node` | Critical | Node not reachable from the start event via outgoing edges (BFS). **This is GRAPH reachability: edge conditions are ignored** — a node behind an always-false condition still counts as reachable (the condition-aware checks are `missing_default_branch` and `dead_branch_after_unconditional`). Boundary events count as reachable through their host activity. |
| `missing_default_branch` | Critical | Exclusive gateway whose outgoing edges are all conditional, with neither a `default` nor an unconditional edge — a token gets stuck when every condition is false. **Exempt:** an exhaustive complement pair (`x == "a"` / `x != "a"` on the same variable) can never both be false, so it is not flagged. |
| `dead_branch_after_unconditional` | Warning | Conditional (or `default`) edge of an exclusive gateway listed **after** an unconditional edge in source flow order: the runtime evaluator takes the first unconditional edge immediately (`gateway.rs`: `None => return vec![edge.target]`), so the later edge can never be taken. `node_id` is the dead edge's target. Edge order is the source document's sequence-flow order, preserved by the parser and compiler — the same order the runtime evaluates. |
| `join_in_cycle` | Warning | Parallel join inside a cycle (SCC detection). Joins **re-arm per iteration** — each fork execution creates a fresh join group that fires once, and the join continuation leaves the group — so a clean fork → branches → join → loop-back flow is valid. The hazard is uneven arrivals within one iteration: a branch that reaches the join without its sibling (e.g. one path re-entering alone) leaves that group below its expected count and starves the join. |
| `cycle_detected` | Warning | Any cycle in the flow (SCC of ≥ 2 nodes); loops are legal but agents should know — the message lists the cycle's node ids. |
| `invalid_task_config` | Critical | External task with an empty/whitespace `task_type` (workers subscribe by task type) or negative `retries`. |
| `task_timeout_zero` | Warning | External task with `timeout_secs == 0` — the lease expires immediately. |
| `duplicate_form_field` | Warning | User task form fields with duplicate ids. |
| `condition_variable_scope` | Info | Input contract: variable referenced by edge conditions; the message says how many edges reference it and that it should be provided at start or via a task output. Production by task outputs cannot be proven statically, so this is informational, **not** an error. |

These are deeper than the compile-time checks (which reject malformed
documents outright): e.g. a two-node island (`a → b → a`) passes the compiler
— every node has incoming and outgoing flows — but is flagged
`unreachable_node` + `cycle_detected` here.

## Dry-run semantics

`dry_run(def, variables, max_steps)` walks the compiled definition from its
start event with the given variable values:

- **Exclusive gateway**: edges are evaluated in order (first-match, mirroring
  the runtime's `ExclusiveGatewayEvaluator`). `VariableEq` is an exact string
  match; expressions go through the lightweight evaluator (below). First true
  edge routes the token; otherwise the `default` edge; otherwise:
  - all-false + no default + no unconditional edge → `stuckAt`
  - unevaluable expression and no resolution → `unresolvedBranch`
- **Parallel fork**: every branch is explored; the join records one step per
  arriving branch and continues once all `expected` branches arrived.
- **External tasks / service tasks / call activities / message & signal
  throw events**: the step is recorded (`detail` = task type / called process
  key / message or signal name) and the walk **assumes completion and
  continues** — failures, retries and lease behavior are out of scope.
- **User tasks / timer / message / signal catch events**: the step is
  recorded and the token parks → `waitingAt` with a reason.
- **End / terminate end**: the path completes; in parallel flows the walk
  drains remaining branches and the terminus records the last end reached.
- **Loops**: the walk is bounded by `max_steps` (REST uses the default 100)
  → `maxStepsExceeded` naming the node the walk was about to visit.

### Terminus variants

| `terminus.type` | Meaning |
|-----------------|---------|
| `completedAt` | An end event was reached (`node_id` = the end node; last one in parallel flows). |
| `waitingAt` | Execution would pause at `node_id` (user task / timer / message / signal catch, or a parallel join still waiting for remaining branches); `reason` explains why. |
| `stuckAt` | Exclusive gateway at `node_id` with every condition false and no default/unconditional edge; `reason` explains. |
| `unresolvedBranch` | Gateway routing could not be decided because a condition is outside the evaluator subset; the expressions are listed in `unresolved_conditions`. |
| `maxStepsExceeded` | Step budget exhausted (typical for loops). |

`unresolved_conditions` lists every expression the evaluator could not
decide; it is advisory and also non-empty when a default branch nevertheless
resolved the routing.

**Reporting is path-interleaved, not a schedule**: parallel branch steps are
emitted in breadth-first order. The `steps` array is a report of what the
walk visited, not a runtime execution plan.

Boundary events are not explored by the walk (they are exceptional paths
fired by the runtime, not sequence-flow successors).

## Condition evaluation: honest subset

The runtime's full EL (`bpm_engine_runtime::el::eval_condition`) cannot be
reused in the BPMN crate (the runtime depends on the BPMN compiler), so the
dry-run uses a deliberately small evaluator (`crates/bpmn/src/condition_lite.rs`)
that mirrors the runtime's semantics exactly for:

- `lhs == "literal"` / `lhs != "literal"` (single spaced comparison,
  single-token left operand, quoted or single-token right operand) — the
  canonical form DSL `variableEq` conditions lower to; a quoted literal is
  one operand even when it contains spaces;
- bare-variable truthiness for no-space operands (the runtime checks
  `variables[expr]` non-emptiness whenever the expression contains no space,
  before looking for operators).

Scanning is **quote-aware**, mirroring the runtime's `split_outside_quotes`
(copied into `condition_lite.rs` because the BPMN crate cannot depend on the
runtime): ` and `, ` or `, `==` and `!=` text inside a quoted literal never
splits the expression, so `x == "salt and pepper"` and `x != "a == b"` parse
as the single comparisons they are (only TOP-LEVEL separators and operators
count).

Everything else — `and`/`or` composition, ordering comparisons (`>`, `<`,
`>=`, `<=`), multi-token operands — is **not** evaluated: the branch is
reported as unresolved instead of guessed. Full EL parity is intentionally
not implemented: dry-run is advisory, so "cannot decide" is a more useful
answer than a wrong route (at runtime the full EL handles all of these; an
expression the full EL cannot parse is treated as `false` by the gateway
evaluator).

The `condition_variable_scope` finding uses a simple tokenizer (operands
outside quotes, numeric literals and `and`/`or`/`not`/`true`/`false`
filtered) to build the input contract; for `x == "y"` only `x` is reported,
for `x == y` both.

## Design decisions

- **Deploy behavior is unchanged.** Validation and dry-run are separate
  pre-deploy calls; `POST /process-definitions/deploy` still compiles and
  persists exactly as before. Nothing is validated implicitly at deploy time
  — agents (and any caller) opt in by calling validate/dry-run first.
- **Compile errors are data (HTTP 200), not HTTP errors**, so agents can
  iterate on a generated document without distinguishing "my document is
  bad" (expected, fixable) from "my request was malformed" (4xx, envelope
  problem).
- **No changes to `core`, `runtime`, or `dsl` crates.** The analysis lives in
  the BPMN crate over the compiled `ProcessDefinition`, which both entry
  points (BPMN XML, DSL JSON) produce.
- The REST server gained a dependency on `bpm-engine-dsl` to compile DSL
  sources for validation/dry-run (the first REST usage of the DSL crate).
