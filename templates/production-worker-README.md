# Production AI Worker Template (N3-4)

A complete, copyable template that ties together every reliability and
safety component an AI-driven worker needs:

| Component | Where | Issue |
|---|---|---|
| Worker SDK poll loop | `ai-worker/src/main.rs` | E4-4 |
| Tool security boundary (allowlist + approval gate + credentials) | `ai-worker/src/main.rs` | E3-3 |
| AI task envelope (model/tool/cost/artifact) | `ai-worker/src/main.rs` | E3-1 |
| **Structured output validation** (JSON Schema + field mapping) | `ai-worker/src/main.rs` | N3-2 |
| **Budget enforcement** (max cost/tokens/attempts) | `ai-worker/src/main.rs` | N3-3 |
| **Idempotency** (per-task dedup key) | worker-sdk `IdempotencyGuard` | E3-2 |
| **Artifact references** (URI + sha256 for large outputs) | envelope `ArtifactRef` | E3-1 |
| **Approval gate** (`__approval` engine-written) | engine ADR-008 | E5-2 |

## Flow: `production-worker-flow.json`

```
start → retrieve → infer → gateway ──(verdict=approved)──→ act → done
                                    └─(default)────────→ reject-end
```

- **retrieve** (`prod-retrieve`): fetches input materials (deterministic).
- **infer** (`prod-infer`): model call with structured output validation
  (`OutputContract` validates the model's JSON against a schema; only
  declared fields map to variables/gateways).
- **gateway**: routes on `verdict` (model output, validated by schema).
- **act** (`prod-act`): side-effect tool behind the approval gate
  (`__approval` must be `"true"`, set by the engine after human approval).
  Budget-checked: over-budget stops the call before it executes.

## Empty-env quickstart (mock default, offline)

No API keys, no network — the mock provider is the default:

```bash
# 1. Build the engine
cargo build -p bpm-engine-server-rest

# 2. Start the engine (memory mode, open auth)
cargo run -p bpm-engine-server-rest &
sleep 2

# 3. Validate the production flow (DSL compiles + passes validation gate)
curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/validate \
  -H 'content-type: application/json' \
  -d "{\"format\":\"dsl\",\"source\":$(cat templates/flows/production-worker-flow.json | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')}"
# → {"valid":true,...}

# 4. Deploy via BPMN XML (the deploy endpoint accepts BPMN XML;
#    for DSL, compile with cargo run -p bpm-engine-dsl --example compile_flow
#    or use the BPMN XML from templates/flows/ai-approval-flow.json as a starting point)
curl -s -X POST http://127.0.0.1:3000/api/v1/process-definitions/deploy \
  -H 'content-type: application/xml' \
  -d '<your-bpmn-xml-here>'

# 5. Start an instance
curl -s -X POST http://127.0.0.1:3000/api/v1/process-instances \
  -H 'content-type: application/json' \
  -d '{"process_def_id": "production-worker:1"}'

# 6. Run the ai-worker template (from templates/ai-worker/)
cd templates/ai-worker
BPM_BASE_URL=http://127.0.0.1:3000 cargo run
```

## Real provider via environment

Set `OPENAI_API_KEY` (and optionally `OPENAI_BASE_URL`, `OPENAI_MODEL`)
to use a real OpenAI-compatible provider instead of the mock:

```bash
OPENAI_API_KEY=sk-... BPM_BASE_URL=http://127.0.0.1:3000 cargo run -p ai-worker-template
```

The worker reads provider config from env once at startup. No keys are
stored in task variables or history — credentials are env-only
(`docs/tool-security.md`).

## CI verification

`scripts/verify-production-template.sh` runs the empty-env quickstart
end-to-end with assertions. It is executed in CI on every push.
