---
layout: default
---

# 15-Minute Quickstart

Run bpm-engine end to end: start the engine, deploy a process, start an
instance, complete its external task with a real worker, and watch the
instance reach `COMPLETED` — then a 5-minute tour of the static
validation, the step timeline, the agent tool catalog, and MCP wiring.

**Every command below was executed and its output captured while writing
this guide** (macOS, Apple Silicon, Rust 1.96). The `scripts/verify-quickstart.sh`
script re-runs the same commands with assertions in CI, so the guide
cannot drift from reality.

## Time budget (honest numbers)

| Section | Budget | Measured |
|---|---|---|
| 0. Prerequisites | 2 min | — |
| 1. Clone & build | *(before the clock)* | 27 s cold on an M-class laptop; budget 2–5 min elsewhere (first build downloads + compiles ~300 crates) |
| 2. Start the engine | 1 min | < 2 s to a healthy server |
| 3. Health check | 1 min | ~1 ms per request |
| 4. Deploy a flow via REST | 2 min | 22 ms |
| 5. Start an instance | 1 min | 41 ms |
| 6. Run the worker | 2 min | ~110 ms from worker start to task completion |
| 7. See COMPLETED + steps | 2 min | ~1 s |
| 8. Extensions | 5 min | ~1 min each |

**15 minutes after the first build.** The first `cargo build` is the only
slow step; everything after it is seconds.

---

## 0. Prerequisites (2 min)

- **Rust (stable)** — install with [rustup](https://rustup.rs):
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- `git`, `curl`, and `jq` (used in step 8) — preinstalled on most systems.
- **No Docker, no database** — the default backend is in-memory, which is
  exactly what this quickstart uses.

## 1. Clone and build *(before the 15-minute clock)*

```bash
git clone https://github.com/fanjia1024/bpm-engine.git
cd bpm-engine
cargo build
```

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 26.90s
```

## 2. Start the engine (1 min)

```bash
cargo run -p bpm-engine-server-rest
```

The binary is `api_server`; it prints its effective configuration (JSON
logs, secrets masked) and starts listening:

```
{"message":"effective configuration","server.host":"0.0.0.0","server.port":3000,
 "database.driver":"memory", ...,"auth.jwt_secret":"<not set>","auth.api_key":"<not set>", ...}
{"message":"storage backend: memory (bootstrap definitions: minimal, payment-flow)"}
{"message":"timer scheduler started","poll_interval_ms":"1000"}
```

- Reach it at **http://127.0.0.1:3000** (binds `0.0.0.0:3000` by default).
- Two **bootstrap definitions** are pre-registered in memory mode:
  `minimal` (Start → End) and `payment-flow` (Start → ExternalTask
  `payment` → End).
- **Open auth** (no credentials configured) — dev default; production
  modes are one flag away (`docs/rbac.md`).
- Custom port: `BPM__SERVER__PORT=3001 cargo run -p bpm-engine-server-rest`
  (note the **double** underscore after `BPM` — see
  [troubleshooting](troubleshooting.md) if your env var seems ignored).

Keep this terminal open; every command below runs in a **second terminal**
in the repo root.

## 3. Health check (1 min)

```bash
curl -s http://127.0.0.1:3000/health
# {"status":"ok"}

curl -s http://127.0.0.1:3000/ready
# {"status":"ok","checks":{"engine":"ok","database":"ok"}}
```

`/health` is liveness (process up); `/ready` checks the engine and the
storage backend. Both are unauthenticated, like `/capabilities`.

## 4. Deploy a flow via REST (2 min)

`POST /api/v1/process-definitions/deploy` takes a **BPMN 2.0 XML**
document. (The JSON DSL has its own REST entry points — validate and
dry-run in step 8 — and is deployed programmatically via
`bpm_engine_dsl::compile`; see `templates/flows/README.md`. One compiler,
three entries, `docs/dsl-guide.md`.)

Deploy a two-node payment flow whose service task is served by the worker
you run in step 6:

```bash
cat > quickstart-payment.bpmn <<'EOF'
<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="quickstart-payment" name="Quickstart Payment" isExecutable="true">
    <startEvent id="start"/>
    <serviceTask id="charge" name="Charge Payment">
      <extensionElements>
        <taskDefinition type="payment"/>
      </extensionElements>
    </serviceTask>
    <endEvent id="end-ok"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="charge"/>
    <sequenceFlow id="f2" sourceRef="charge" targetRef="end-ok"/>
  </process>
</definitions>
EOF

curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/deploy \
  -H "Content-Type: application/xml" \
  --data-binary @quickstart-payment.bpmn
# {"process_definition_id":"quickstart-payment"}     ← HTTP 201
```

The `<taskDefinition type="payment"/>` extension is what makes the service
task an external task of type `payment` — the "topic" workers subscribe
to. Deployments are immutable versions; deploying the same id twice
creates version 2.

## 5. Start an instance (1 min)

```bash
curl -s -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"quickstart-payment","variables":{"amount":"100"}}'
# {"instance_id":"41028a07-1197-4ee7-90b1-38f7a89a0a1d","status":"RUNNING"}

export INSTANCE_ID=41028a07-1197-4ee7-90b1-38f7a89a0a1d   # ← use YOUR id

curl -s http://127.0.0.1:3000/api/v1/process-instances/$INSTANCE_ID | jq '{status, current_nodes, variables}'
# {
#   "status": "RUNNING",
#   "current_nodes": ["charge", "start"],
#   "variables": { "amount": "100" }
# }
```

`current_nodes` lists the instance's **Waiting** tokens — note the engine
keeps the origin `start` token in `Waiting` by design (it is never
collected; see `docs/trace.md`'s token-model notes), so it appears
alongside `charge`; the order is not contractual.

The instance is `RUNNING` with a token **waiting at `charge`**: external
tasks do not run inside the engine — a worker must fetch, lock, and
complete them (lease-based, exactly-once token completion).

## 6. Run the worker (2 min)

In your second terminal:

```bash
cargo run -p bpm-engine-worker-sdk --example payment
```

```
INFO payment worker starting; poll Engine at http://127.0.0.1:3000
INFO processing payment task_id=cfecf7dd-… amount=100
INFO task finished task_id=cfecf7dd-… task_type=payment worker_id=payment-worker-1 duration_ms=102 result="Complete"
```

The worker polls `fetch-and-lock`, gets the task (its handler serves
`task_type = "payment"` — matching step 4's `<taskDefinition type="payment"/>`),
runs its idempotent handler, and completes the task with
`{"status":"PAID","amount":"100"}` merged back into the instance. It keeps
polling for more tasks; stop it with **Ctrl-C** when you are done.

(First run compiles the example: ~7 s after step 1's build.)

## 7. See COMPLETED (2 min)

```bash
curl -s http://127.0.0.1:3000/api/v1/process-instances/$INSTANCE_ID | jq '{status, variables}'
# {
#   "status": "COMPLETED",
#   "variables": { "status": "PAID", "amount": "100" }
# }
```

The execution history is one call away — the **step timeline**
(`docs/trace.md`):

```bash
curl -s http://127.0.0.1:3000/api/v1/process-instances/$INSTANCE_ID/steps \
  | jq '.steps[] | {step_no, node_id, kind, events: [.events[].event_type]}'
# {"step_no":1,"node_id":"start","kind":"node","events":["TokenArrived"]}
# {"step_no":2,"node_id":"charge","kind":"external_task",
#  "events":["TokenArrived","ExternalTaskLocked","ExternalTaskCompleted"]}
# {"step_no":3,"node_id":"end-ok","kind":"node",
#  "events":["TokenArrived"]}                    # reaching an End records
#                                                # TokenArrived; the instance
#                                                # then closes with the
#                                                # instance-level
#                                                # ProcessCompleted event
#                                                # (TokenCompleted is a
#                                                # defined, replay-replayable
#                                                # event the runtime does
#                                                # not emit in v1)
```

You have just executed a crash-safe, replayable workflow: every step above
is recorded as an immutable history event (`.../history`), and the same
execution can be deterministically replayed.

---

## 8. Five-minute extensions

### 8.1 Validate and dry-run a DSL flow (no deployment, no side effects)

The repo ships machine-readable flow templates (`templates/flows/`).
Check one before deploying it — compile failures come back as **HTTP 200
data**, not errors, so agents can read and fix them:

```bash
jq -Rs '{format: "dsl", source: .}' templates/flows/basic-gateway.json |
  curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/validate \
    -H "Content-Type: application/json" -d @- | jq '{valid, findings}'
# {"valid":true,"findings":[{"severity":"Info","code":"condition_variable_scope",…}]}

jq -Rs '{format: "dsl", source: ., variables: {status: "PAID"}}' \
  templates/flows/basic-gateway.json |
  curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/dry-run \
    -H "Content-Type: application/json" -d @- | jq '.dry_run.terminus'
# {"type":"completedAt","node_id":"end-paid"}
```

The dry-run is a side-effect-free walk with your hypothetical variables —
no instance, task, timer, or history record is created
(`docs/validation-and-dry-run.md`). Editing flows: `docs/dsl-guide.md`.

### 8.2 Run an AI flow end to end (self-contained)

```bash
cargo run --example ai_flow
```

This needs no server and no keys: it spawns a local engine, deploys the
approval **DSL** flow, runs it with the offline deterministic mock model
provider, and prints the step timeline with each step's AI envelope
(model + token cost, tools used):

```
state    : COMPLETED
steps:
  #2 propose    external_task  … envelope: "mock"/"mock-model-v1" 8 "USD" (16->8 tokens)
  #3 approve    node           ["TokenArrived", "UserTaskCompleted"]
  #4 act        external_task  … tools=[ledger_post]
```

Real provider wiring: `docs/ai-reference-flows.md`. The worker behind
those tasks is the shape of `templates/ai-worker/`.

### 8.3 Explore the machine-readable agent catalog

```bash
curl -s http://127.0.0.1:3000/capabilities | jq '.tools | length'
# 35
curl -s http://127.0.0.1:3000/capabilities | jq '.tools[] | select(.classification=="read_only") | .name'
```

One entry per REST operation, with parameter schemas, read-only/mutating
classification, and the stable error codes each operation can return —
the contract AI agents code against (`docs/agent-tools.md`, OpenAPI:
`docs/openapi.yaml`).

### 8.4 Wire an MCP client

Expose the engine's tool catalog to MCP clients (Claude Desktop/Code,
Cursor, …) via the stdio server `bpm-engine-mcp-server`. Copy
`templates/mcp-config.json` into your client and replace the minted token:

```bash
# The engine's JWT secret: set it yourself BEFORE starting the server, e.g.
#   export BPM__AUTH__JWT_SECRET=dev-secret-change-me   (note the DOUBLE
#   underscore after BPM — see docs/troubleshooting.md §6 — then restart
#   the engine). The startup log line
#   "effective configuration ... auth.jwt_secret = dev***" confirms it took.
# With it exported:
BPM_JWT_SECRET=dev-secret-change-me \
cargo run -p bpm-engine-server-rest --example mint_jwt -- \
    --sub mcp-agent --tenant acme --role operator
```

Full wiring and security notes: `templates/README.md` and
`docs/mcp-server.md`.

### 8.5 Where to go next

- **Something failed?** [troubleshooting.md](troubleshooting.md) — the
  failure modes new users actually hit (401/403, ports, Postgres, DSL
  errors, tasks stuck, DLQ, 429, MCP), each with symptom → cause → fix.
- **Copyable starting points** — [templates/](../templates/): flow JSON,
  a compiling worker crate, MCP config.
- **All runnable examples** — [examples/index.md](examples/index.md).
- **JWT identity, roles, tenants** — `docs/rbac.md` (mint tokens with the
  `mint_jwt` example, as in 8.4).
- **PostgreSQL persistence** — `docs/recovery.md` +
  `docs/database-schema.md`; crash-recovery demo: `./deploy/verify-recovery.sh`.
- **Architecture & guarantees** — `docs/architecture.md`,
  `docs/invariants.md`, `docs/execution-model.md`.
