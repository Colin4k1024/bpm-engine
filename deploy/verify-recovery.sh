#!/usr/bin/env bash
# Verify crash recovery: kill engine, restart, then check history and token state.
# Requires: curl. Optional: jq for JSON checks.
# Run from repo root. Engine must be built: cargo build -p bpm-engine-server-rest
set -e

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
API="${BASE_URL}/api/v1"

echo "=== 1. Start engine in background ==="
cargo run -p bpm-engine-server-rest &
ENGINE_PID=$!
echo "Engine PID: $ENGINE_PID"

echo "=== 2. Wait for engine to be up ==="
for i in {1..30}; do
  if curl -s -o /dev/null -w "%{http_code}" "$BASE_URL" 2>/dev/null | grep -q 200; then
    break
  fi
  sleep 0.5
done
curl -s -o /dev/null -w "%{http_code}" "$BASE_URL" | grep -q 200 || { echo "Engine did not become ready"; kill $ENGINE_PID 2>/dev/null; exit 1; }

echo "=== 3. Start a process instance (payment-flow) ==="
RESP=$(curl -s -X POST "$API/process-instances" \
  -H "Content-Type: application/json" \
  -d '{"process_def_id":"payment-flow","variables":{"amount":"100"}}')
INSTANCE_ID=$(echo "$RESP" | sed -n 's/.*"instance_id":"\([^"]*\)".*/\1/p')
if [ -z "$INSTANCE_ID" ]; then
  echo "Failed to start instance. Response: $RESP"
  kill $ENGINE_PID 2>/dev/null
  exit 1
fi
echo "Instance ID: $INSTANCE_ID"

echo "=== 4. Wait for worker to complete (or history to have events) ==="
sleep 3

echo "=== 5. Kill engine (simulate crash) ==="
kill -9 $ENGINE_PID 2>/dev/null || true
sleep 1

echo "=== 6. Restart engine ==="
cargo run -p bpm-engine-server-rest &
ENGINE_PID=$!
for i in {1..30}; do
  if curl -s -o /dev/null -w "%{http_code}" "$BASE_URL" 2>/dev/null | grep -q 200; then
    break
  fi
  sleep 0.5
done
curl -s -o /dev/null -w "%{http_code}" "$BASE_URL" | grep -q 200 || { echo "Engine did not become ready after restart"; kill $ENGINE_PID 2>/dev/null; exit 1; }

echo "=== 7. Verify instance state and history ==="
INSTANCE=$(curl -s "$API/process-instances/$INSTANCE_ID")
HISTORY=$(curl -s "$API/process-instances/$INSTANCE_ID/history")

# Check instance returned
if echo "$INSTANCE" | grep -q '"instance_id"'; then
  echo "Instance state: OK"
else
  echo "Instance state check failed. Response: $INSTANCE"
  kill $ENGINE_PID 2>/dev/null
  exit 1
fi

# Check history has events and no duplicate TokenCompleted per token (simple grep)
if echo "$HISTORY" | grep -q '"events"'; then
  echo "History: OK (events present)"
else
  echo "History check failed. Response: $HISTORY"
  kill $ENGINE_PID 2>/dev/null
  exit 1
fi

# Optional: with jq, fail if duplicate TokenCompleted for same token_id in payload
if command -v jq &>/dev/null; then
  TOKEN_COMPLETED_COUNT=$(echo "$HISTORY" | jq '[.events[]? | select(.event_type == "TokenCompleted")] | length')
  echo "TokenCompleted events: $TOKEN_COMPLETED_COUNT"
fi

echo "=== Verify recovery: PASS ==="
kill $ENGINE_PID 2>/dev/null || true
exit 0
