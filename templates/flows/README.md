# Flow Templates (DSL v1 JSON)

Machine-readable process definitions in the **JSON DSL v1** format
(`docs/dsl-guide.md`; JSON Schema: `crates/dsl/schema/bpm-dsl-v1.json`).
Every file here is compiled by `bpm_engine_dsl::compile` in the workspace
test `crates/dsl/tests/flow_templates.rs` — a template that stops compiling
fails CI.

| Template | Demonstrates | Task types a worker must serve |
|---|---|---|
| [`basic-gateway.json`](basic-gateway.json) | An external task plus an exclusive gateway routing on a variable (`status == "PAID"` → paid end, otherwise the default manual-review end). The smallest flow that exercises worker execution AND branching. | `payment` |
| [`ai-approval-flow.json`](ai-approval-flow.json) | The AI approval shape: a model task drafts a proposal (`ai-propose`), a human approves via a user task form (`decision: approved \| rejected`), a gateway routes, and a side-effect task (`ai-act`) runs only on approval. Mirrors the E3-4 approval reference flow (`docs/ai-reference-flows.md`). **Route-variable caveat:** the engine does NOT merge user-task form variables into instance variables (`UserTaskCompletedHandler` drops them), so supply the routing variable `decision` (and, for tool-gated side effects, `__approval: "true"`) in the START variables — see the section below. | `ai-propose`, `ai-act` |
| [`timer-wait.json`](timer-wait.json) | A persistent timer: the instance waits `PT5S` on a `timerIntermediateCatch` node, then completes. No worker needed — the timer scheduler advances it (timers survive restarts, `docs/recovery.md`). | — |

## Using a template

**1. Validate and dry-run it over REST** (no deployment, no side effects —
`docs/validation-and-dry-run.md`). The endpoint takes the document as a
*string inside a JSON envelope*, so slurp the file with `jq -Rs`:

```bash
# against a running engine (see docs/quickstart.md)
jq -Rs '{format: "dsl", source: .}' templates/flows/basic-gateway.json |
  curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/validate \
    -H "Content-Type: application/json" -d @-

# dry-run adds a side-effect-free walk with hypothetical start variables:
jq -Rs '{format: "dsl", source: ., variables: {status: "PAID"}}' \
  templates/flows/basic-gateway.json |
  curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/dry-run \
    -H "Content-Type: application/json" -d @-
```

Compile failures come back as **HTTP 200 data** (`valid: false` +
`compile_errors` with stable snake_case `code` + JSON-pointer `path` — see
`docs/dsl-guide.md` § Error codes and paths), so agents can read and fix
them.

**2. Deploy and run it from Rust** — the DSL compiles to the same
`ProcessDefinition` as BPMN XML and the typed builder (one compiler, three
entries):

```rust
let source = std::fs::read_to_string("templates/flows/basic-gateway.json")?;
let def = bpm_engine_dsl::compile(&source)?;   // Result<ProcessDefinition, DslCompileReport>
// register `def` with your definition store and start instances
```

`POST /api/v1/process-definitions/deploy` (the REST deploy endpoint) takes
**BPMN 2.0 XML only** — DSL documents are deployed programmatically via the
compiler, as the `ai_flow` example does end to end
(`cargo run --example ai_flow`) and as `templates/ai-worker`'s companion
flow does for worker development.

**3. Run the payments with a worker** — `basic-gateway.json` uses task type
`payment`, so the repo's payment worker completes it:
`cargo run -p bpm-engine-worker-sdk --example payment` (see
`docs/sdk-rust.md`).

## The approval flow's route variable (read before running it)

`ai-approval-flow.json`'s gateway routes on the `decision` variable — and
in THIS engine, **user-task form submissions do not merge into instance
variables** (the runtime's `UserTaskCompletedHandler` moves the token but
drops the form fields; see `crates/runtime/src/user_task_completed_handler.rs`).
Starting the flow with no variables therefore always takes the default
(rejected) branch — the form records the human's choice, but the gateway
never sees it.

The E3-4 reference flows handle this by carrying the decision in the
START variables (external-task completions DO merge their variables into
the instance), so do the same when dry-running or starting the template:

```bash
# dry-run the approved branch:
jq -Rs '{format: "dsl", source: ., variables: {decision: "approved"}}' \
  templates/flows/ai-approval-flow.json |
  curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/dry-run \
    -H "Content-Type: application/json" -d @-
```

The same caveat applies to the tool-approval convention: a worker using
the E3-3 `__approval` gate reads it from the task variables, which derive
from the START variables — pass `__approval: "true"` when starting
(`templates/ai-worker`'s companion flow demonstrates this). Engine-owned
approval records (E5-2) write `__approval` for gated task types; this
plain DSL flow has no engine gating, so the caller supplies it.

## Rules to keep in mind when editing

- `schema_version` must be `1`; unknown fields are rejected
  (`unknown_field`), so typos fail loudly instead of being ignored.
- Exactly one `start`, at least one `end`; `serviceTask` requires
  `task_type`; `timerIntermediateCatch` requires `timer.duration`
  (ISO 8601, e.g. `PT5S` / `PT1H`).
- An edge is either conditional (`condition`) or `default` — never both.
- `variableEq` compares a plain string variable exactly; anything richer
  needs `expression` (the engine EL).
