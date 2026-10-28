---
layout: default
---

# Agent Tool Catalog — Machine Capability Discovery (E4-1)

> Issue #19 deliverable: publish the tool directory, parameter schemas,
> auth notes, and error examples — separating read-only from mutating
> operations. The **single source of truth** is the normative OpenAPI
> contract ([`docs/openapi.yaml`](openapi.yaml)); the catalog is a
> derived, committed artifact, and CI fails if the two ever drift.

---

## 1. How an agent discovers capabilities

Three equivalent views of the same catalog:

1. **The endpoint** — `GET /capabilities` (no `/api/v1` prefix, no
   authentication):
   ```sh
   curl http://127.0.0.1:3000/capabilities
   ```
   Serves the committed artifact verbatim (`application/json`), embedded
   at compile time — no runtime YAML parsing, no new runtime
   dependencies.

2. **The committed file** —
   [`crates/server/rest/capabilities/agent-tools.json`](../crates/server/rest/capabilities/agent-tools.json),
   useful for offline tooling and prompt assembly.

3. **The spec** — `docs/openapi.yaml` is what the catalog is generated
   from; for details the catalog deliberately omits (bounds, defaults,
   full response shapes), the spec is the reference.

### Why the endpoint is unauthenticated

`/capabilities` is registered like `/health` and `/ready` (top-level
`root_route!`, outside the api-key + rate-limit middleware that wraps
`/api/v1`): it is **discovery metadata for agents BEFORE they hold an
API key**, and it carries no secrets — only route shapes and the stable
error codes derived from the public spec. An agent can learn the
contract, then acquire a key to use the engine operations (which stay
fully behind the `X-API-Key` layer whenever the server has one
configured — the catalog's `auth` field documents this honestly).

## 2. Catalog structure

```jsonc
{
  "catalog_version": "1",              // catalog format version
  "api_version": "0.2.0",              // spec info.version
  "generated_from": "docs/openapi.yaml",
  "regenerate": "cargo run -p bpm-engine-server-rest --example dump_capabilities",
  "auth": {
    "type": "apiKey", "in": "header", "name": "X-API-Key",
    "required": false,                  // enforced only when configured
    "notes": "..."                      // current auth semantics + X-Tenant-Id
  },
  "conventions": {
    "classification": "read_only vs mutating semantics",
    "error_shape": "ErrorResponse contract + deploy 400 exception",
    "pagination": "which endpoints paginate and how",
    "idempotency": "Idempotency-Key semantics on start-instance"
  },
  "error_examples": {                   // one per DISTINCT stable code
    "not_found": { "status": 404, "body": { "error": "...", "code": "not_found" } },
    ...
  },
  "tools": [
    {
      "name": "start_process_instance", // snake_case from operationId
      "operation": "POST /api/v1/process-instances",
      "summary": "Start a process instance",
      "description": "...",             // spec description, when present
      "classification": "mutating",     // read_only | mutating
      "side_effects": "...",            // what the call changes
      "auth_required": true,            // behind the X-API-Key layer?
      "parameters": [
        { "name": "X-Tenant-Id", "in": "header", "required": false,
          "schema": { "type": "string" }, "description": "..." },
        { "name": "body", "in": "body", "required": true,
          "content_type": "application/json",
          "schema": { "type": "object", "required": ["process_def_id"],
                     "properties": { "process_def_id": { "type": "string" }, ... } } }
      ],
      "responses": {
        "success": { "status": 201, "description": "Instance created.",
                     "schema": "StartInstanceResponse" },
        "errors": [
          { "status": 400, "code": "invalid_request" },
          { "status": 401, "code": "unauthorized" },
          ...
        ]
      }
    }
  ]
}
```

Field semantics:

- **`name`** — snake_case of the spec `operationId` (stable identifier).
- **`operation`** — `"<METHOD> <path>"` in the spec's `{param}` form.
- **`parameters`** — ordered `path` → `query` → `header` → `body`.
  Header/query/path parameters come from the spec's parameter
  components (schemas flattened); the `body` parameter carries the
  request body's **minimal flattened schema**: local `$ref`s resolved
  inline, only `type`/`enum`/`required`/`properties`/`items`/
  `additionalProperties` kept. Bounds, defaults, and prose live in the
  spec's parameter descriptions — the catalog keeps those verbatim.
- **`responses.success.schema`** — the NAME of the referenced response
  schema in the spec (a pointer for the full shape; absent for inline
  or bodyless `204` responses).
- **`responses.errors`** — `(status, code)` pairs for every error
  response the operation references, expanded to the stable codes the
  spec's response components document. A `400` referencing the shared
  `BadRequest` component therefore lists every boundary code that
  component documents — a documented superset of what a specific call
  can trigger. The one code-less entry is deploy's inline `400`
  (the `DeployError` shape carries no `code` field; see its
  `description`).
- **`error_examples`** — one realistic `ErrorResponse`-shaped example
  body per DISTINCT code (the CI suite asserts set equality with
  `routes::error_codes::ERROR_CODES`). The `error` message text is an
  example, not a contract — only the `code` is stable. The
  `invariant_violation` example also shows the optional third field and
  the `X-Invariant-Violation` header mirroring.

## 3. Read-only vs mutating

Classification rules (also in `conventions.classification`):

- **`read_only`** — all `GET` operations, plus the two side-effect-free
  `POST`s: `POST /api/v1/process-definitions/validate` and
  `POST /api/v1/process-definitions/dry-run`. Those two compile/simulate
  in memory and persist nothing (no definition, instance, task, timer,
  or history record — E2-2), so an agent may call them freely while
  exploring; their `side_effects` note says so explicitly.
- **`mutating`** — every other `POST`/`PUT`/`DELETE`. The
  **`side_effects`** note states what changes: `start_process_instance`
  creates an instance, `complete_user_task` advances tokens, deploy
  persists a definition version, requeue mutates the dead-letter queue,
  etc. Replay-session operations are mutating but **ephemeral** (in
  memory, expiring on inactivity; no engine state changes) — the notes
  say so.

## 4. Using the error examples

Agents should branch on `code`, never on the `error` prose:

1. Read the tool's `responses.errors` for the codes (and statuses) the
   operation can answer.
2. Look the code up in `error_examples` for a realistic body shape.
3. Retry behavior follows the conventions: `429 rate_limited` (back
   off), `401 unauthorized` (the key is missing/invalid — or the server
   has none configured), `400 task_not_locked`/`lock_expired`
   (fetch-and-lock again), etc.

## 5. Lockstep: regeneration and CI

The catalog is **generated** from `docs/openapi.yaml` by
`bpm_engine_server_rest::capabilities::generate_catalog` (pure
transformation of the parsed spec — typed structs, sorted collections,
byte-deterministic output). Two things keep it honest:

- **The lockstep test** (`crates/server/rest/tests/openapi_contract.rs`,
  `capabilities_catalog_matches_generated_output`) regenerates the
  catalog from the spec and compares it with the committed file **byte
  for byte**. A spec change without regeneration fails CI with the
  command to fix it.
- **The endpoint test** asserts `GET /capabilities` serves exactly the
  committed bytes and validates against the spec's `AgentToolsCatalog`
  schema; structural tests pin one tool per spec operation, one example
  per canonical code, and the classification rules.

Regenerate after any change to `docs/openapi.yaml` (add a route, tweak
a schema, add an error code):

```sh
cargo run -p bpm-engine-server-rest --example dump_capabilities
```

Hand-maintained derivation tables in
`crates/server/rest/src/capabilities.rs` (each drift-guarded — the
generator panics, and CI fails, when they stop matching the spec):

| Table | Purpose |
|---|---|
| `RESPONSE_CODES` | error codes each named error-response component documents |
| `SIDE_EFFECT_FREE_POSTS` | the two POSTs classified `read_only` |
| `SIDE_EFFECT_NOTES` | per-operation side-effect note (required for every mutating operation) |
| `ERROR_EXAMPLES` | one realistic example per DISTINCT code |

The generation also asserts, loudly: every mutating operation has a
side-effect note, no orphaned table entries, and the example table
equals the spec's `ErrorResponse.code` enum exactly.

## 6. Related

- [`docs/openapi.yaml`](openapi.yaml) — the normative contract (the
  catalog's `AgentToolsCatalog` and related schemas live there too).
- [`docs/api-spec.md`](api-spec.md) — the prose REST API specification.
- `crates/server/rest/src/capabilities.rs` — the generator and the
  embedded artifact constant.
- `crates/server/rest/examples/dump_capabilities.rs` — the regeneration
  command (the DSL `dump_schema` pattern,
  `crates/dsl/tests/schema_lockstep.rs`).
- [`docs/mcp-server.md`](mcp-server.md) — the E4-2 MCP server that turns
  this catalog into Model Context Protocol tools over stdio (the same
  committed artifact, embedded via a workspace-relative `include_str!`).
