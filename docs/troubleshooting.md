---
layout: default
---

# Troubleshooting — Common Failures

The failure modes a new user actually hits, each with **symptom → cause →
fix**. Every error code, message, and behavior below exists in the repo
today: the bodies marked `actual` were captured by running the commands in
[quickstart.md](quickstart.md) and triggering each failure. The stable
error-code table lives in [api-spec.md](api-spec.md) §5 (single source:
`crates/server/rest/src/routes.rs::error_codes`).

Quick triage:

| You see | Go to |
|---|---|
| `401` with `unauthorized` / `invalid_token` | [1](#1-auth-401), [2](#2-jwt-401) |
| `403 forbidden` | [3](#3-403-forbidden-rbac-or-tenant-header-conflict) |
| `404` for an instance you just created | [4](#4-404-for-an-instance-that-exists-jwt-tenant-scoping) |
| Server exits / nothing listens | [5](#5-port-already-in-use), [6](#6-env-var-overrides-ignored) |
| Postgres startup failures | [7](#7-postgres-driver-and-migrations) |
| DSL rejected / compile errors | [8](#8-dsl-validation-errors) |
| Instance stuck, task never completes | [9](#9-task-never-completes) |
| `400 variable_too_large` | [10](#10-variables-too-large) |
| Task went to Failed, dead letters | [11](#11-dead-letter-queue-dlq-behavior) |
| `429 rate_limited` | [12](#12-429-rate-limited) |
| MCP tools missing / server won't start | [13](#13-mcp-wiring) |

---

## 1. Auth 401 (`unauthorized`)

**Symptom** (actual, API-key mode):

```
$ curl http://127.0.0.1:3000/api/v1/process-instances
{"error":"missing or invalid API key","code":"unauthorized"}   [HTTP 401]
```

**Cause** — the server was started with a **bootstrap API key**
(`BPM_API_KEY`, or `auth.api_key` / `BPM_AUTH__API_KEY` in config) and the
request carries no `X-API-Key` header, or a wrong one.

**Fix** — send the configured key:

```bash
curl -H "X-API-Key: $BPM_API_KEY" http://127.0.0.1:3000/api/v1/process-instances
```

The API key is the full-access bootstrap credential (`docs/rbac.md` §
Runtime modes). `/health`, `/ready`, and `/capabilities` never require it.
If neither secret is configured the server is in **open mode** and accepts
every request — dev only.

## 2. JWT 401 (`invalid_token`)

**Symptom** (actual, JWT mode — `BPM_JWT_SECRET` set):

```
$ curl http://127.0.0.1:3000/api/v1/process-instances
{"error":"invalid or missing JWT: missing bearer token (JWT auth is configured; send Authorization: Bearer <jwt>)","code":"invalid_token"}   [HTTP 401]
```

**Cause** — JWT auth is configured (server-side `BPM_JWT_SECRET` /
`auth.jwt_secret`) but the request has no `Authorization: Bearer <jwt>`
header, or the token is malformed, signed with a different secret, or
expired (expiry is enforced with **zero leeway**).

**Fix** — mint a token with the SAME secret the server verifies, and send
it as a bearer header:

```bash
TOKEN=$(BPM_JWT_SECRET=$BPM_JWT_SECRET \
  cargo run -q -p bpm-engine-server-rest --example mint_jwt -- \
    --sub alice --tenant acme --role operator)

curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3000/api/v1/process-instances
```

Claims contract: `sub` (actor), `tenant` (non-empty; scopes every query),
`role` (`deployer|operator|worker|auditor`), `exp` (unix seconds; default
1 h via `mint_jwt`, `--exp-in <secs>` to override) — `docs/rbac.md`. A
presented bearer token always wins over the API key; with no bearer, the
API key (if configured) is the fallback.

## 3. 403 `forbidden` (RBAC or tenant-header conflict)

**Symptom** — `{"error":"…","code":"forbidden"}` [HTTP 403] on a
`/api/v1` operation although the token itself is valid.

**Cause** — one of:

1. **Role lacks the permission** (JWT mode): the role→permission matrix is
   enforced before handlers (`docs/rbac.md` § Role → permission matrix).
   e.g. only `deployer` (or the admin API key) may `POST
   /process-definitions/deploy`; only `operator` may start instances,
   complete user tasks, or requeue/delete dead letters; `worker` owns the
   external-task lease lifecycle; `auditor` is read-only.
2. **`X-Tenant-Id` header conflicts with the token's tenant claim** — in
   JWT mode the tenant comes FROM the claim; a disagreeing header is
   rejected (`docs/rbac.md` § X-Tenant-Id deprecation).

**Fix** — mint the token with the role that matches what you are doing,
and drop the `X-Tenant-Id` header in JWT mode (an absent or agreeing
header is fine). The admin API key bypasses role checks (bootstrap path).

## 4. 404 for an instance that exists (JWT tenant scoping)

**Symptom** (actual — same instance, different tenant's token):

```
$ curl -H "Authorization: Bearer $TENANT_ACME_TOKEN" …/process-instances/$ID → 200
$ curl -H "Authorization: Bearer $TENANT_OTHER_TOKEN" …/process-instances/$ID
{"error":"process instance not found: 84628093-…","code":"not_found"}   [HTTP 404]
```

**Cause** — tenant isolation (E5-1): under a verified JWT, an id belonging
to ANOTHER tenant answers exactly like an unknown id — byte-identical
404s, no existence leak (`docs/api-spec.md` §5, `not_found`). The same
applies to tasks, dead letters, and replay sessions; cross-tenant
external-task complete/fail answer 400 `task_not_found` before any state
changes.

**Fix** — query with a token whose `tenant` claim matches the instance's
tenant (the one that created it). In open/API-key mode there is no such
scoping (`X-Tenant-Id` is honored as untrusted input there).

## 5. Port already in use

**Symptom** (actual — starting a second server on port 3000):

```
Error: Address already in use (os error 48)
(the trailing os-error number is OS-specific: 48 on macOS, 98 on Linux)
```

**Cause** — another `api_server` (or any process) already listens on the
configured port. Note that `0.0.0.0:3000` catches `127.0.0.1:3000` and
`localhost:3000` alike.

**Fix** — stop the other process, or run on another port:

```bash
BPM__SERVER__PORT=3001 cargo run -p bpm-engine-server-rest
```

(or set `server.port` in a `bpm.toml`, or `BPM_CONFIG_PATH` to point at
your file — `crates/server/rest/src/bpm_config.rs`).

## 6. Env var overrides ignored

**Symptom** — you start the server with `BPM_SERVER__PORT=3001` (single
underscore after `BPM`) and the effective-configuration log still says
`server.port: 3000`; the variable is silently ignored. Same for
`BPM_LOG__LEVEL`, `BPM_AUTH__JWT_SECRET`-style overrides of *config*
fields.

**Cause** — a quirk of how the server's config layer reads the
environment (`config` crate, prefix `BPM` with `__` key separator): the
prefix is stripped as `BPM__` (double underscore), so `BPM_SERVER__PORT`
does not match any layer. **Exception:** `BPM_JWT_SECRET` and
`BPM_API_KEY` are read directly by the auth middleware and work in the
single-underscore form.

**Fix** — use the double-underscore form for config fields (verified
working against the real binary):

```bash
BPM__SERVER__PORT=3001 \
BPM__RATE_LIMIT__REQUESTS_PER_MINUTE=1000 \
BPM__ENGINE__VARIABLE_MAX_BYTES=131072 \
cargo run -p bpm-engine-server-rest
```

Keep `BPM_JWT_SECRET` / `BPM_API_KEY` single-underscore (they are also the
forms the middleware, `mint_jwt`, and `docs/rbac.md` document). The
effective-configuration log line at startup (secrets masked) tells you
immediately whether an override landed.

## 7. Postgres driver and migrations

**Symptoms** (actual):

```
# driver = postgres, no URL:
Error: database.url must not be empty when database.driver = "postgres"

# unsupported driver:
Error: unsupported database.driver "postgress": expected "memory" or "postgres"
```

or, with `migrate_on_start = false`, startup fails listing pending
migrations.

**Cause/fix** — the postgres driver needs `database.url` and the `postgres`
feature (default-on for `bpm-engine-server-rest`):

```bash
BPM__DATABASE__DRIVER=postgres \
BPM__DATABASE__URL=postgres://bpm:bpm@localhost:5432/bpm \
cargo run -p bpm-engine-server-rest
```

- `database.migrate_on_start` (default **true**) applies the versioned SQL
  migrations on startup. In change-managed environments set it to `false`:
  the server then VALIDATES the schema and fails startup if any migration
  is pending (apply them out of band — `docs/database-schema.md`,
  `crates/adapters/postgres/migrations/`).
- `migrations` and `postgres-recovery` CI jobs cover exactly this wiring;
  locally, `BPM_TEST_DATABASE_URL` gates the `-- --ignored` suites.
- In **memory mode** (default) none of this exists — definitions `minimal`
  and `payment-flow` are bootstrap-registered; in postgres mode definitions
  must be deployed via the REST API (programmatic definitions cannot be
  persisted there).

## 8. DSL validation errors

**Symptom** — you POST a DSL document to
`POST /api/v1/process-definitions/validate` and get **HTTP 200** with
`"valid": false` (this is data, not an HTTP error — agents are meant to
read it):

```json
{ "valid": false,
  "compile_errors": [ { "code": "unknown_field", "path": "/nodes/0/taskTypo", "message": "…" } ] }
```

or a 400 envelope error for a malformed request itself: `invalid_format`
(`format` not `"bpmn"`/`"dsl"`), `empty_source`, `invalid_request_body`.

**Cause** — the DSL is stricter than BPMN on purpose: unknown fields are
rejected (typos fail loudly), `serviceTask` requires `task_type`,
`timerIntermediateCatch` requires `timer.duration`, `condition` and
`default` are mutually exclusive, node ids must be unique, edges must
reference existing ids.

**Fix** — read `code` + `path` from each entry: `path` is a JSON pointer
into YOUR document (`/nodes/2/id`). The full table (snake_case codes:
`json_parse`, `schema_version_unsupported`, `unknown_field`,
`missing_field`, `invalid_type`, `duplicate_node_id`,
`duplicate_edge_id`, `unknown_node_ref`, `empty_node_list`, `compile`) is
in `docs/dsl-guide.md` § Error codes and paths; the JSON Schema is
`crates/dsl/schema/bpm-dsl-v1.json`. The envelope must carry the document
as a string: `jq -Rs '{format:"dsl", source: .}' flow.json | curl … -d @-`
(quickstart §8.1). Then dry-run it before deploying.

## 9. Task never completes

**Symptom** — the instance stays `RUNNING` forever, the token waits at the
external-task node; `GET /api/v1/tasks?type=external` shows the task
`Locked` or `Waiting`.

**Cause → fix**, in the order they actually happen:

1. **No worker is running** (or it is not polling this engine) — start
   one: `cargo run -p bpm-engine-worker-sdk --example payment`
   (quickstart §6). Workers poll `fetch-and-lock` every `poll_interval`.
2. **Topic mismatch** — the worker's `task_type()` must equal the
   flow's `task_type` (BPMN: the `<taskDefinition type="…"/>` extension;
   DSL: the `serviceTask.task_type` field; builder: the
   `external_task(id, task_type)` argument). A worker polling `payment`
   never fetches a `charge` task — check both sides, not one.
3. **A dead worker still holds the lease** — the task shows `Locked` with
   the old `worker_id`. The engine does not time out locked tasks on its
   own; the lease is released by expiry on the next
   `fetch-and-lock` (reclaim) — run (another) worker and it re-fetches
   after the lock expires, or trigger a fetch to reclaim
   (`docs/accident-scenarios.md` §3 walks the crash → expiry → reclaim →
   second-worker path with the expected trace).
4. **Retries exhausted / permanent failure** — the task went to `Failed`
   and a dead-letter entry exists: check [11](#11-dead-letter-queue-dlq-behavior).
   Requeue it or start a new instance.

Diagnosis tools: `GET /api/v1/process-instances/:id/steps` (the timeline —
Locked/Completed events per task, worker ids, attempts),
`GET …/trace`, `GET …/history`. Worker-side: `RUST_LOG=info` shows every
fetch/complete. Remember the delivery contract: the ENGINE completes a
token exactly once; workers receive tasks at-least-once and must be
idempotent (`docs/sdk-rust.md` § Delivery and idempotency).

## 10. Variables too large

**Symptom** (actual):

```
$ curl -X POST …/process-instances -d '{"process_def_id":"minimal","variables":{"big":"<70 000 bytes>"}}'
{"error":"variable 'big' is 70000 bytes, exceeding the 65536 byte limit; store large payloads externally and reference them via an artifact envelope","code":"variable_too_large"}   [HTTP 400]
```

**Cause** — every variable value (envelope included) is capped at
`engine.variable_max_bytes` (default 65 536), enforced at the REST write
boundaries: start-instance, user-task complete, external-task complete.
The check runs again after encryption (ciphertext inflation counts).

**Fix** — store large payloads in YOUR object storage and submit an
artifact envelope instead (`docs/variables-and-artifacts.md` §2–3):

```json
{ "big": "__var:1:{\"t\":\"artifact\",\"uri\":\"https://storage.example.com/reports/q3.pdf\",\"size\":102400,\"sha256\":\"a1b2…\"}" }
```

The engine never dereferences artifact URIs — resolution is yours.
Related codes: `variable_invalid_envelope` (malformed envelope),
`variable_invalid_artifact_uri` (URI contract violation — no userinfo, no
fragment, ≤ 2048 bytes), `reserved_variable_key` (you may not write
`__task_envelope` yourself), `sensitive_requires_encryption` /
`encryption_key_not_configured` (the `encrypted` list needs
`BPM_VARIABLE_ENCRYPTION_KEY`). To raise the cap (it applies to ALL
variables): `BPM__ENGINE__VARIABLE_MAX_BYTES=131072`.

## 11. Dead letter queue (DLQ) behavior

**Symptom** — the task is `Failed`; `GET /api/v1/dead-letters` (actual):

```json
{ "entries": [ {
    "id": "31ead5a2-…", "task_id": "445d3cc2-…", "task_type": "payment",
    "process_instance_id": "15b130b6-…", "token_id": "182d3580-…",
    "error_message": "{\"retry_category\":\"permanent\"}: policy refusal: amount exceeds limit",
    "variables": "{}", "tenant_id": null, "created_at": "1793212647" } ] }
```

**Cause** — either retries ran out (transient failures decrement and
requeue, then dead-letter) or the worker failed the task with
`category: "permanent"`, which skips the retry loop entirely:

```bash
curl -X POST …/external-tasks/$TASK_ID/fail -H "Content-Type: application/json" \
  -d '{"worker_id":"w1","error":"policy refusal: amount exceeds limit","category":"permanent"}'
# {"status":"FAILED"}    ← the SDK's TaskResult::Fail{category: Permanent} sends exactly this
```

**Fix** — decide per case: fix the worker and **requeue**
(`POST /api/v1/dead-letters/:id/requeue` creates a new external task from
the entry and removes it), or **delete** it
(`DELETE /api/v1/dead-letters/:id`). Both are operator-role operations in
JWT mode; the failing worker's claim tenant tags the entry and gates
list/get/requeue/delete. Note the fetch bound: a dead-letters page returns
at most the first 100 entries (`docs/api-spec.md` §5b).

## 12. 429 `rate_limited`

**Symptom** (actual — server started with
`BPM__RATE_LIMIT__REQUESTS_PER_MINUTE=3`):

```
200 429 429 429 429
{"error":"rate limit exceeded","code":"rate_limited"}   [HTTP 429]
```

**Cause** — every request to the `/api/v1` nest passes a sliding-window
limiter; the default is **100 requests/minute**. A worker polling at
1 s intervals alone eats 60 of those; a tight polling loop, a second
worker, or a load test exhausts the rest.

**Fix** — raise the window for development/CI:

```bash
BPM__RATE_LIMIT__REQUESTS_PER_MINUTE=100000 cargo run -p bpm-engine-server-rest
```

or poll less aggressively (`WorkerConfig::poll_interval`). The limiter
counts ALL `/api/v1` requests per client; `/health`, `/ready` and
`/capabilities` are outside it.

## 13. MCP wiring

**Symptoms → causes → fixes** (the E4-2 server, `docs/mcp-server.md`):

1. **`tools/list` shows fewer tools than expected / a tool call answers
   `-32602` "unknown tool"** — the allowlist does not contain that name.
   `BPM_MCP_ALLOWED_TOOLS` is an EXACT, comma-separated match against
   catalog names (`curl -s $ENGINE/capabilities | jq -r '.tools[].name'`)
   — a typo or the REST path instead of the tool name (`POST
   /process-instances` vs `start_process_instance`) silently drops it.
   Role presets (`BPM_MCP_ROLE=auditor|deployer|operator|worker`) down-
   scope to that role's matrix; role ∩ explicit list when both are set.
2. **Server refuses to start over a cleartext credential** — the
   cleartext guard (fail closed): a credential (`BPM_MCP_TOKEN` /
   `BPM_MCP_API_KEY`) plus a NON-loopback `http://` base URL would send
   the secret in cleartext. Use `https://`, a loopback host, or set
   `BPM_MCP_ALLOW_INSECURE_HTTP=1` to accept the risk explicitly (it
   prints a warning; only the exact value `1` overrides).
3. **Every tool call returns the engine's 401 body** (`invalid_token` /
   `unauthorized`) — the credential is valid MCP-side but rejected by the
   engine: `BPM_MCP_TOKEN` must be signed with the SAME
   `BPM_JWT_SECRET` the engine verifies (mint per quickstart §8.4), and
   the role claim must hold the permissions you are exercising (fix: see
   [2](#2-jwt-401)/[3](#3-403-forbidden-rbac-or-tenant-header-conflict)).
   With no credential configured the MCP server runs OPEN mode, which
   only works against an open-mode engine.
4. **`initialize` works, nothing else does** — check `BPM_BASE_URL`
   (default `http://127.0.0.1:3000`) and `BPM_MCP_TIMEOUT_MS` (default
   10 000 ms) against your engine; startup diagnostics go to stderr
   while stdout is reserved for the protocol.

Manual smoke test without any MCP client:

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  | BPM_BASE_URL=http://127.0.0.1:3000 cargo run -p bpm-engine-mcp-server
```

---

## Not covered here

- Engine-internal invariant rejections (`invariant_violation` + the
  `X-Invariant-Violation` header): `docs/invariants.md`.
- Crash / kill / restart recovery: `docs/recovery.md` and the scripted
  demo `./deploy/verify-recovery.sh`.
- Worker execution context (lease renewal, timeouts, idempotency
  helpers): `docs/worker-context.md`.
