# Agent Operation Recipes (N5-3)

Machine-readable examples for the four canonical agent workflows. Each
recipe is a sequence of REST calls (or MCP tool invocations) with expected
responses, error codes, and required permissions. Run them against a local
engine (`cargo run -p bpm-engine-server-rest`) with the appropriate JWT role.

## Recipe 1: Pre-publish checks

**Goal**: an agent proposes a definition, validates it, and publishes only
if it passes the validation gate.

**Role**: `deployer`

```bash
# 1. Validate (no side effects — checks compile + Critical findings)
curl -s -X POST $URL/api/v1/process-definitions/validate \
  -H 'content-type: application/json' \
  -d '{"format":"bpmn","source":"<bpmn-xml>"}'
# → 200 {"valid":true,"report":{"valid":true,...,"required_permissions":["worker:orders"]}}

# 2. Dry-run (simulate with hypothetical variables — no persistence)
curl -s -X POST $URL/api/v1/process-definitions/dry-run \
  -H 'content-type: application/json' \
  -d '{"format":"bpmn","source":"<bpmn-xml>","variables":{"x":"approved"}}'
# → 200 {"valid":true,"dry_run":{"steps":[...],"terminus":{"type":"completedAt"}}}

# 3. Deploy (validation gate: Critical findings → 400)
curl -s -X POST $URL/api/v1/process-definitions/deploy \
  -H 'content-type: application/xml' \
  -d '<bpmn-xml>'
# → 201 {"process_definition_id":"order-flow:1"}
# → 400 {"code":"validation_failed","report":{...}} (if Critical findings)

# 4. Audit trail
curl -s $URL/api/v1/process-definitions/order-flow:1/audit
# → 200 [{"operation":"deploy","actor":"agent@tenant",...}]
```

**Error codes**: `validation_failed` (400), `deploy_failed` (500)

## Recipe 2: Changes requiring human approval

**Goal**: an agent starts a process that requires human approval before a
side-effect tool executes.

**Role**: `operator` (start + decide), `worker` (complete approved task)

```bash
# 1. Start instance (with approval-required task type configured)
curl -s -X POST $URL/api/v1/process-instances \
  -H 'content-type: application/json' \
  -d '{"process_def_id":"approval-flow:1"}'
# → 201 {"instance_id":"...","process_def_id":"approval-flow:1"}

# 2. List pending approvals
curl -s $URL/api/v1/process-instances/$INSTANCE_ID/approvals
# → 200 [{"id":"appr-1","status":"pending","task_type":"ref-act",...}]

# 3. Decide (approve or reject — atomic, exactly once)
curl -s -X POST $URL/api/v1/approvals/appr-1/decide \
  -H 'content-type: application/json' \
  -d '{"decision":"approve","note":"risk acceptable"}'
# → 200 {"id":"appr-1","status":"approved"}
# → 409 {"code":"approval_already_decided"} (if already decided)

# 4. Worker fetches and completes the now-approved task
# (via worker SDK fetch_and_lock + complete)
```

**Error codes**: `approval_already_decided` (409), `instance_paused` (409),
`instance_cancelled` (409)

## Recipe 3: Waiting/delivery

**Goal**: an agent delivers a message to a running instance waiting at a
message catch node.

**Role**: `operator`

```bash
# 1. Deliver a message to a specific instance
curl -s -X POST $URL/api/v1/process-instances/$INSTANCE_ID/messages \
  -H 'content-type: application/json' \
  -d '{"message_name":"order-ready","payload":{"order_id":"123"}}'
# → 202 {"delivery_id":"del-1","status":"pending"}

# 2. Check pending deliveries
curl -s $URL/api/v1/process-instances/$INSTANCE_ID/deliveries
# → 200 [{"id":"del-1","status":"pending","name":"order-ready",...}]

# 3. Deliver by correlation (no target instance — finds by variable match)
curl -s -X POST $URL/api/v1/messages \
  -H 'content-type: application/json' \
  -d '{"message_name":"order-ready","correlation":{"order_id":"123"}}'
# → 202 {"delivery_id":"del-2","status":"pending"}
# → 409 {"code":"message_correlation_ambiguous"} (if multiple match)

# 4. Broadcast a signal
curl -s -X POST $URL/api/v1/signals \
  -H 'content-type: application/json' \
  -d '{"signal_name":"shutdown","payload":{}}'
# → 202 {"delivery_id":"sig-1","status":"pending"}
```

**Error codes**: `message_correlation_ambiguous` (409)

## Recipe 4: Fault recovery

**Goal**: an agent or operator triggers compensation for a failed process
and manages the recovery.

**Role**: `operator`

```bash
# 1. Trigger compensation (creates external tasks for Pending records)
curl -s -X POST $URL/api/v1/process-instances/$INSTANCE_ID/compensation/trigger
# → 200 {"triggered":2,"records":[...]}

# 2. View compensation progress (blocking reason + available actions)
curl -s $URL/api/v1/process-instances/$INSTANCE_ID/compensation
# → 200 {"instance_state":"Terminated","summary":{"total":2,"failed":1,...},
#         "blocking_reason":"1 compensation step(s) failed",
#         "available_actions":["retry","skip"],"records":[...]}

# 3. Retry a failed compensation step
curl -s -X POST $URL/api/v1/process-instances/$INSTANCE_ID/compensation/retry/$RECORD_ID?reason=transient
# → 200

# 4. Skip a compensation step (terminal)
curl -s -X POST $URL/api/v1/process-instances/$INSTANCE_ID/compensation/skip/$RECORD_ID
# → 200

# 5. Cancel the instance (if not already terminal)
curl -s -X POST $URL/api/v1/process-instances/$INSTANCE_ID/cancel
# → 200
```

**Error codes**: `instance_cancelled` (409), `instance_not_pausable` (409)

## CI verification

The recipe test (`tests/agent_recipes.rs`) exercises the four recipe groups
against a real engine (memory mode) and asserts the expected status codes,
error codes, and permission enforcement.
