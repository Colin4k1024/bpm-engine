#!/usr/bin/env bash
# verify-quickstart.sh — run the 15-minute quickstart (docs/quickstart.md)
# end to end, with assertions, in a fresh checkout.
#
# What it does (the quickstart's commands, verbatim, plus checks):
#   1. builds the engine and the payment worker example
#   2. starts the REST server (memory mode, open auth) on port 3000
#   3. health + readiness checks
#   4. deploys the quickstart payment flow via REST (BPMN XML)
#   5. starts a process instance
#   6. runs the payment worker until the task completes
#   7. asserts the instance is COMPLETED and the step timeline shows the
#      external-task lifecycle
#   8. validates + dry-runs a DSL flow template (templates/flows)
#   9. checks the /capabilities agent tool catalog
#
# Usage: bash scripts/verify-quickstart.sh
#
# Port: the quickstart (and the payment worker example, which hardcodes
# `http://127.0.0.1:3000`) use port 3000. The script starts its own engine
# on 3000 when it is free; if a healthy bpm-engine already listens there,
# it is REUSED (deploy + instance + worker run against it); anything else
# on the port fails fast with a clear message.
#
# Idempotent-safe: every process this script starts is killed by an EXIT
# trap; no state persists (the memory backend dies with the server).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

# The worker's HTTP client (reqwest) follows system proxies, which can
# break loopback traffic; force direct connections to the engine.
export no_proxy="127.0.0.1,localhost"
export NO_PROXY="$no_proxy"

PORT=3000
BASE_URL="http://127.0.0.1:$PORT"
API="$BASE_URL/api/v1"

log() { printf '\n=== %s ===\n' "$*"; }
pass() { printf 'PASS: %s\n' "$*"; }
die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# --- dependencies ----------------------------------------------------------
for cmd in cargo curl jq; do
  command -v "$cmd" >/dev/null 2>&1 || die "required command not found: $cmd"
done

SERVER_PID=""
WORKER_PID=""
TMP_DIR="$(mktemp -d)"

cleanup() {
  [[ -n "$WORKER_PID" ]] && kill "$WORKER_PID" 2>/dev/null || true
  [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

# --- 1. build (reported separately: it is outside the quickstart clock) ----
log "Build engine + worker example (first build is the slow part)"
BUILD_START=$(date +%s)
cargo build -p bpm-engine-server-rest
cargo build -p bpm-engine-worker-sdk --example payment
BUILD_SECS=$(( $(date +%s) - BUILD_START ))
echo "build took ${BUILD_SECS}s (the quickstart's 15 minutes start AFTER this)"

# --- 2. start the server (quickstart step 2) -------------------------------
log "Start the engine on $BASE_URL"
# Is something already listening on the port?
if curl -s -m 2 "$BASE_URL/health" >/dev/null 2>&1; then
  HEALTH="$(curl -s -m 2 "$BASE_URL/health" || true)"
  if [[ "$(echo "$HEALTH" | jq -r '.status' 2>/dev/null)" == "ok" ]]; then
    echo "a healthy engine is already running on $BASE_URL — reusing it"
  else
    die "port $PORT is in use by something that is not a bpm-engine (/health: $HEALTH). Stop it or free the port."
  fi
else
  # BPM__SERVER__PORT: the working env-override form (docs/troubleshooting.md #6).
  # The rate limit is raised so the worker's 1 Hz polling cannot trip the
  # default 100 req/min window (docs/troubleshooting.md #12).
  BPM__SERVER__PORT="$PORT" \
  BPM__RATE_LIMIT__REQUESTS_PER_MINUTE=1000000 \
    cargo run -q -p bpm-engine-server-rest >"$TMP_DIR/server.log" 2>&1 &
  SERVER_PID=$!
fi

# --- 3. health check (quickstart step 3) -----------------------------------
log "Wait for /health"
HEALTH=""
for _ in $(seq 1 60); do
  if HEALTH="$(curl -s "$BASE_URL/health" 2>/dev/null)"; then
    [[ "$(echo "$HEALTH" | jq -r '.status')" == "ok" ]] && break
  fi
  sleep 1
done
[[ "$(echo "$HEALTH" | jq -r '.status' 2>/dev/null)" == "ok" ]] \
  || { tail -20 "$TMP_DIR/server.log" 2>/dev/null; die "server did not become healthy on $BASE_URL"; }
pass "GET /health -> $HEALTH"

READY="$(curl -s "$BASE_URL/ready")"
[[ "$(echo "$READY" | jq -r '.status')" == "ok" ]] || die "GET /ready -> $READY"
pass "GET /ready -> $READY"

# --- 4. deploy the quickstart flow (quickstart step 4) ---------------------
log "Deploy quickstart-payment via REST"
cat >"$TMP_DIR/quickstart-payment.bpmn" <<'EOF'
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
DEPLOY_HTTP="$(curl -s -o "$TMP_DIR/deploy.json" -w '%{http_code}' \
  -X POST "$API/process-definitions/deploy" \
  -H "Content-Type: application/xml" \
  --data-binary @"$TMP_DIR/quickstart-payment.bpmn")"
[[ "$DEPLOY_HTTP" == "201" ]] || die "deploy returned HTTP $DEPLOY_HTTP: $(cat "$TMP_DIR/deploy.json")"
DEPLOYED_ID="$(jq -r '.process_definition_id' "$TMP_DIR/deploy.json")"
[[ "$DEPLOYED_ID" == "quickstart-payment" ]] || die "unexpected deployed id: $DEPLOYED_ID"
pass "deploy -> 201 $DEPLOYED_ID"

# --- 5. start an instance (quickstart step 5) ------------------------------
log "Start a process instance"
START_HTTP="$(curl -s -o "$TMP_DIR/start.json" -w '%{http_code}' \
  -X POST "$API/process-instances" \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"quickstart-payment","variables":{"amount":"100"}}')"
[[ "$START_HTTP" == "201" ]] || die "start returned HTTP $START_HTTP: $(cat "$TMP_DIR/start.json")"
INSTANCE_ID="$(jq -r '.instance_id' "$TMP_DIR/start.json")"
STATUS="$(jq -r '.status' "$TMP_DIR/start.json")"
echo "instance $INSTANCE_ID ($STATUS)"
pass "start instance -> $INSTANCE_ID"

# The waiting instance shows exactly the external-task node (the start
# token auto-forwards; docs/quickstart.md §5 pins this sample).
CURRENT_NODES="$(curl -s "$API/process-instances/$INSTANCE_ID" | jq -r '.current_nodes | sort | join(",")')"
[[ "${CURRENT_NODES}" == "charge,start" ]] \
  || die "current_nodes is '${CURRENT_NODES}', expected the set {charge, start} (docs/quickstart.md §5: the origin start token stays Waiting by design; order is not contractual)"
pass "current_nodes == {charge, start} (waiting at the external task)"

# --- 6. run the worker (quickstart step 6) ---------------------------------
log "Run the payment worker (killed as soon as the instance completes)"
RUST_LOG=info cargo run -q -p bpm-engine-worker-sdk --example payment \
  >"$TMP_DIR/worker.log" 2>&1 &
WORKER_PID=$!

# --- 7. see COMPLETED (quickstart step 7) ----------------------------------
log "Poll the instance until COMPLETED (max 60s)"
STATUS=""
for _ in $(seq 1 60); do
  STATUS="$(curl -s "$API/process-instances/$INSTANCE_ID" | jq -r '.status')"
  [[ "$STATUS" == "COMPLETED" ]] && break
  sleep 1
done
if [[ "$STATUS" != "COMPLETED" ]]; then
  { echo "--- server log (tail) ---"; tail -20 "$TMP_DIR/server.log" 2>/dev/null;
    echo "--- worker log (tail) ---"; tail -20 "$TMP_DIR/worker.log" 2>/dev/null; } >&2
  die "instance $INSTANCE_ID is $STATUS, expected COMPLETED"
fi
VARIABLES="$(curl -s "$API/process-instances/$INSTANCE_ID" | jq -c '.variables')"
[[ "$VARIABLES" == *'"status":"PAID"'* ]] || die "variables after completion: $VARIABLES"
pass "instance COMPLETED with variables $VARIABLES"

kill "$WORKER_PID" 2>/dev/null || true
wait "$WORKER_PID" 2>/dev/null || true
WORKER_PID=""
grep -q 'task finished' "$TMP_DIR/worker.log" \
  || die "worker log does not show task completion: $(cat "$TMP_DIR/worker.log")"
pass "worker completed the payment task (see worker.log)"

# --- 8. step timeline (quickstart step 7) ----------------------------------
log "Check the step timeline"
STEPS="$(curl -s "$API/process-instances/$INSTANCE_ID/steps")"
echo "$STEPS" | jq -c '.steps[] | {step_no, node_id, kind}' || true
echo "$STEPS" | jq -e '.steps[] | select(.kind=="external_task")
  | .events[].event_type == "ExternalTaskCompleted"' >/dev/null \
  || die "no ExternalTaskCompleted in the step timeline"
pass "step timeline shows the external-task lifecycle"

# --- 9. validate + dry-run a DSL flow template (quickstart step 8.1) -------
log "Validate and dry-run templates/flows/basic-gateway.json"
jq -Rs '{format: "dsl", source: .}' templates/flows/basic-gateway.json \
  | curl -s -X POST "$API/process-definitions/validate" \
      -H "Content-Type: application/json" -d @- >"$TMP_DIR/validate.json"
jq -e '.valid == true' "$TMP_DIR/validate.json" >/dev/null \
  || die "basic-gateway.json failed validation: $(cat "$TMP_DIR/validate.json")"
pass "validate basic-gateway.json -> valid"

jq -Rs '{format: "dsl", source: ., variables: {status: "PAID"}}' \
  templates/flows/basic-gateway.json \
  | curl -s -X POST "$API/process-definitions/dry-run" \
      -H "Content-Type: application/json" -d @- >"$TMP_DIR/dryrun.json"
jq -e '.dry_run.terminus.node_id == "end-paid"' "$TMP_DIR/dryrun.json" >/dev/null \
  || die "dry-run terminus unexpected: $(cat "$TMP_DIR/dryrun.json")"
pass "dry-run basic-gateway.json -> terminus end-paid"

# --- 10. the agent tool catalog (quickstart step 8.3) ----------------------
log "Check /capabilities"
TOOL_COUNT="$(curl -s "$BASE_URL/capabilities" | jq '.tools | length')"
EXPECTED_TOOLS="$(jq '.tools | length' crates/server/rest/capabilities/agent-tools.json)"
[[ "${TOOL_COUNT}" -eq "${EXPECTED_TOOLS}" ]] \
  || die "capabilities lists ${TOOL_COUNT} tools but the committed catalog has ${EXPECTED_TOOLS} — regenerate or fix the drift"
pass "GET /capabilities -> ${TOOL_COUNT} tools (== committed catalog)"


log "QUICKSTART VERIFIED"
echo "All quickstart steps passed (build ${BUILD_SECS}s + a few seconds of runtime)."
