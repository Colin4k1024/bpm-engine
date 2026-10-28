---
layout: default
---

# MCP Server — Model Context Protocol Adapter (E4-2)

> Issue #20 deliverable: expose definition validation, deployment,
> instance queries, history, and task operations over the stable
> REST/OpenAPI contract, with **minimal-permission authorization and
> tenant validation**. The MCP server is a thin, catalog-driven adapter:
> every tool call is one authenticated REST call against the engine, and
> every engine answer (success OR error) is surfaced verbatim to the
> agent.

---

## 1. What it is

`crates/mcp-server` (package `bpm-engine-mcp-server`, binary
`bpm-engine-mcp-server`) speaks the **Model Context Protocol over
stdio** — newline-delimited JSON-RPC 2.0 on stdin/stdout — and maps MCP
tool calls 1:1 onto the engine's REST API. It has **no runtime
dependency on any engine crate**: the engine is a black box reached over
HTTP (`bpm-engine-server-rest` is a dev-dependency used only by the
in-process test server).

The tool surface is derived **mechanically from the E4-1 agent tool
catalog** (`crates/server/rest/capabilities/agent-tools.json`, served at
`GET /capabilities`): the server embeds that exact file via a
workspace-relative `include_str!` — there is no copy, so the catalog
cannot drift (a lockstep test additionally compares the embedded bytes
with the server crate's file and its own embed). Tool names, parameter
schemas, read-only/mutating classifications, and descriptions all come
from the catalog; regenerating the catalog needs no code change here.

Because that `include_str!` reaches into `../server/rest/capabilities/`
(a path escaping the crate directory), the package is
**workspace-internal by design**: `publish = false` in its `Cargo.toml`
(it is a tool you run from a workspace checkout, not a crate.io
artifact), and `cargo package -p bpm-engine-mcp-server` is intentionally
unsupported. Standalone packaging would need a build-time copy of the
catalog — out of scope for v1; the single-source embedding is the
feature.

## 2. Running it

```sh
# Terminal 1: the engine (memory mode, open auth — dev default)
cargo run -p bpm-engine-server-rest

# Terminal 2: the MCP server
BPM_BASE_URL=http://127.0.0.1:3000 \
BPM_MCP_TOKEN=$(cargo run -p bpm-engine-server-rest --example mint_jwt -- \
    --sub mcp-agent --tenant acme --role operator) \
cargo run -p bpm-engine-mcp-server
```

Point any stdio MCP client at the binary (Claude Desktop, Claude Code,
Cursor, `mcphost`, …). Example client configuration:

```json
{
  "mcpServers": {
    "bpm-engine": {
      "command": "cargo",
      "args": ["run", "--release", "-p", "bpm-engine-mcp-server"],
      "env": {
        "BPM_BASE_URL": "http://127.0.0.1:3000",
        "BPM_MCP_TOKEN": "<minted JWT>",
        "BPM_MCP_ROLE": "operator"
      }
    }
  }
}
```

A manual smoke test needs no MCP client at all:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  | BPM_BASE_URL=http://127.0.0.1:3000 cargo run -p bpm-engine-mcp-server
```

### Environment variables

| Variable | Meaning | Default |
| --- | --- | --- |
| `BPM_BASE_URL` | Engine REST base URL | `http://127.0.0.1:3000` |
| `BPM_MCP_TOKEN` | Bearer credential (`Authorization: Bearer …`) — a role-scoped JWT minted with `mint_jwt` (**preferred**) | unset |
| `BPM_MCP_API_KEY` | Legacy admin API key (`X-API-Key`) — full engine access | unset |
| `BPM_MCP_TIMEOUT_MS` | Per-call engine REST timeout | `10000` |
| `BPM_MCP_ROLE` | Tool allowlist role preset: `auditor` \| `deployer` \| `operator` \| `worker` | unset |
| `BPM_MCP_ALLOWED_TOOLS` | Explicit, comma-separated tool allowlist | unset |
| `BPM_MCP_ALLOW_INSECURE_HTTP` | Explicit opt-in (`1`) to send a credential over plaintext non-loopback `http://` — dangerous, prints a warning | unset — the combination refuses to start |

Startup diagnostics go to **stderr** (stdout is the protocol channel —
nothing else may be written there); secrets are never logged. With
neither credential set the server runs in **open mode**: it attaches no
credential and only works against an engine that is itself in open
mode — local development only. When both are set, the token wins (a
presented bearer token also takes precedence engine-side).

**Cleartext-credential guard (fail closed)**: a configured credential
together with a NON-loopback `http://` base URL would send the secret in
cleartext — startup refuses with an error naming the remedies: use an
`https://` base URL (or a loopback host), or set
`BPM_MCP_ALLOW_INSECURE_HTTP=1` to accept the risk explicitly (a single
warning is printed on stderr when the override is used; only the exact
value `1` overrides). `https://` anywhere and plaintext loopback
(`localhost` and the loopback IP ranges: `127.0.0.1`, `::1`, …) are
always allowed; open mode (no credential) is unaffected.

## 3. Protocol coverage

The server implements the initialize-handshake era of MCP that
mainstream stdio clients speak (protocol versions
`2024-11-05`, `2025-03-26`, `2025-06-18`):

- **`initialize`** — negotiates the protocol version (a client-known
  version is echoed; an unknown or newer version gets our latest
  supported), answers `capabilities` (`tools`, `listChanged: false`),
  `serverInfo`, and `instructions` describing the permission model.
- **`notifications/initialized`** (and every other notification) —
  accepted silently; notifications never get responses, even when their
  other fields are malformed (a wrong `jsonrpc` version on a
  notification stays silent — the no-`id` check runs before all other
  validation, per JSON-RPC's MUST-NOT-reply rule; only requests with an
  `id` can earn `-32600`).
- **`tools/list`** — the allowlisted catalog tools. Each entry carries
  the catalog `summary` + `description` + a machine-derived REST line
  (`REST <METHOD> <path> (read-only|mutating — side effects: …)`), a
  flattened JSON-Schema `inputSchema` (`type: object`,
  `additionalProperties: false`), and `annotations` (`title`,
  `readOnlyHint` per the E4-1 classification).
- **`tools/call`** — validates the arguments (see §5), builds the REST
  request, and dispatches it. The result is a single text content item:
  the engine's response body, pretty-printed; `isError` mirrors the
  HTTP status (2xx → false, else → true). **Engine errors are tool
  results, not protocol errors** — the JSON error body
  (`{error, code, invariant_violation?}`) is carried verbatim so agents
  can branch on the stable `code` values.
- **`ping`** — answers `{}`.

Robustness (all covered by tests): malformed JSON lines answer `-32700`
and the loop keeps reading; non-object messages, JSON-RPC batches
(removed from MCP), a wrong `jsonrpc`, or a missing `method` answer
`-32600`; unknown methods answer `-32601`; unknown tools and invalid
arguments answer `-32602` (per the MCP spec these are protocol-level
errors); request ids are echoed verbatim (string | number | null);
messages are bounded (8 MiB) — **enforced at the READ layer**: the loop
reads one chunk at a time and never buffers more than the bound plus one
64 KiB reader chunk, so a client streaming bytes without newlines cannot
grow the process's memory without bound (an undelimited frame past the
limit answers exactly one `-32700` oversized-frame error and the
connection closes; a frame that arrives with its newline but over the
limit is answered `-32600` and the loop keeps reading; invalid UTF-8
answers `-32700` and the loop keeps reading); stdin **EOF exits
cleanly** (the MCP shutdown signal), as does a client closing stdout
(broken pipe); timeouts/transport failures become tool errors, never
hangs. Messages are processed sequentially in arrival order;
`notifications/cancelled` is accepted but v1 does not abort in-flight
REST calls. Responses are single-line compact JSON (MCP forbids embedded
newlines).

**Not implemented in v1** (documented deviations): the
per-request-metadata / `server/discover` era of MCP (revision
2026-07-28+) — a modern-era client falling back to `initialize` works
unchanged; server-initiated requests, resources, prompts, sampling,
progress notifications, and HTTP transports.

## 4. The tool surface (v1, decided)

20 of the catalog's 35 tools — the issue's scope (validation, deploy,
instance queries, history, task operations) minus replay sessions,
definition view/activate/deprecate, invariant checks, and health (all
reachable in v2 by widening `allowlist::V1_SURFACE`):

| Tool | REST | Classification |
| --- | --- | --- |
| `validate_process_definition` | POST `/api/v1/process-definitions/validate` | read-only (E2-2) |
| `dry_run_process_definition` | POST `/api/v1/process-definitions/dry-run` | read-only (E2-2) |
| `list_process_definitions` | GET `/api/v1/process-definitions` | read-only |
| `list_definition_versions` | GET `/api/v1/process-definitions/versions/{key}` | read-only |
| `list_process_instances` | GET `/api/v1/process-instances` | read-only |
| `get_process_instance` | GET `/api/v1/process-instances/{id}` | read-only |
| `get_instance_history` | GET `/api/v1/process-instances/{id}/history` | read-only |
| `get_instance_steps` | GET `/api/v1/process-instances/{id}/steps` | read-only |
| `list_tasks` | GET `/api/v1/tasks` | read-only |
| `list_dead_letters` | GET `/api/v1/dead-letters` | read-only |
| `get_dead_letter` | GET `/api/v1/dead-letters/{id}` | read-only |
| `get_capabilities` | GET `/capabilities` | read-only, unauthenticated |
| `deploy_process_definition` | POST `/api/v1/process-definitions/deploy` | mutating |
| `start_process_instance` | POST `/api/v1/process-instances` | mutating |
| `fetch_and_lock_external_tasks` | POST `/api/v1/external-tasks/fetch-and-lock` | mutating |
| `complete_external_task` | POST `/api/v1/external-tasks/{task_id}/complete` | mutating |
| `fail_external_task` | POST `/api/v1/external-tasks/{task_id}/fail` | mutating |
| `extend_external_task_lock` | POST `/api/v1/external-tasks/{task_id}/extend-lock` | mutating |
| `complete_user_task` | POST `/api/v1/tasks/{task_id}/complete` | mutating |
| `requeue_dead_letter` | POST `/api/v1/dead-letters/{id}/requeue` | mutating |

The `v1 surface` is enforced **structurally**: `McpServer::new` refuses
any tool name outside it, so no configuration or code path can bypass
the decided boundary.

## 5. Permission model (minimal permission)

Minimal permission = **the server only exposes allowlisted tools**. The
allowlist resolves at startup:

| `BPM_MCP_ROLE` | `BPM_MCP_ALLOWED_TOOLS` | Effective tools |
| --- | --- | --- |
| unset | unset | the 12 read-only tools (every read + validate/dry-run) |
| set | unset | the role preset |
| unset | set | the explicit list, bounded by the v1 surface |
| set | set | role preset ∩ explicit list (pure down-scope) |

Role presets mirror the E5-1 role→permission matrix
([`rbac.md`](rbac.md)) so the exposed tools match what the credential's
role may actually use:

- **auditor** — the 12 read-only tools (the default);
- **deployer** — reads + `deploy_process_definition`;
- **operator** — reads + `start_process_instance`,
  `complete_user_task`, `requeue_dead_letter`;
- **worker** — reads + `fetch_and_lock_external_tasks`,
  `complete_external_task`, `fail_external_task`,
  `extend_external_task_lock`.

Unknown role names and allowlist entries that are not v1 surface tools
**abort startup** with the valid names listed — a permission boundary
fails closed on typos.

This is the first boundary only. The engine remains the authority:
every tool call carries the configured credential, and the engine
enforces the role matrix (403 `forbidden`) and tenancy (404
`not_found`) on its side — an allowlist mistake degrades to an
engine-side rejection, never to unauthorized access.

### Tenant validation — where the boundary sits

The MCP server **adds no trust of its own**:

- it forwards ONE pre-provisioned credential (`BPM_MCP_TOKEN` /
  `BPM_MCP_API_KEY`) and never interprets its claims;
- it **never sets `X-Tenant-Id`**, and no tool argument can: header
  parameters are excluded from the input schemas, and unknown
  arguments are rejected (`additionalProperties: false` +
  server-side validation) — an MCP client cannot smuggle a tenant (or
  any other header) past the boundary. The one exception is
  `Idempotency-Key`, exposed as the optional `idempotency_key`
  argument of `start_process_instance` (E2-4 idempotency);
- tenancy is enforced **engine-side from the verified claim** (E5-1):
  listings are claim-scoped, cross-tenant access answers 404
  `not_found`, and a conflicting `X-Tenant-Id` header is rejected —
  none of which this server can weaken.

The end-to-end suite proves the whole chain: a worker-role token
calling a listed mutating tool gets the engine's 403 `forbidden` body
as a tool error; a tenant-B token reading a tenant-A instance gets 404
`not_found`; a garbage token gets 401 `invalid_token`.

## 6. Argument flattening and dispatch

Each catalog tool's parameters flatten into ONE MCP `inputSchema`:

- path/query parameters become top-level properties (with the catalog
  descriptions);
- object bodies are **merged** — each body property becomes a
  top-level property and body `required` feeds the top-level
  `required`;
- non-object bodies (deploy's raw BPMN XML) become a single `body`
  string property sent with `application/xml`;
- header parameters are dropped (except `Idempotency-Key`, above).

Dispatch is pure and catalog-driven: path segments are substituted with
percent-encoding (`/` is always encoded, so an argument can never
traverse into another path segment; `:` stays literal because task ids
are `<instance_id>:<node_id>`), query parameters render as scalars, and
the JSON body collects the body-key arguments. Validation checks the
flattened schema (unknown keys, missing required, primitive types,
enums) BEFORE any I/O; nested structures pass through for the engine's
REST layer to validate with its stable error codes.

## 7. Testing

Tests drive the in-process handler (`McpServer::handle_line`), the real
read/write loop (`run_over` over duplex pipes), and the real BINARY over
true stdio pipes — no external MCP client is needed:

- `tests/catalog_lockstep.rs` — the embedded catalog equals the server
  crate's file (disk and its own embed) byte-for-byte; every catalog
  tool flattens.
- `tests/protocol.rs` — handshake (version negotiation), `tools/list`
  shape (names ⊆ allowlist, `inputSchema` type/properties, no
  `X-Tenant-Id`, `idempotency_key` present), `tools/call` dispatch via
  a fake caller, error mapping (-32700/-32600/-32601/-32602), id echo,
  notification silence-before-validation, tenant-header smuggling
  rejection, garbage-line fuzz, oversized lines, CRLF/blank tolerance.
- `tests/stdio.rs` — the REAL loop (`run_over`) over in-process duplex
  pipes: session ordering with silent notifications, malformed and
  invalid-UTF-8 frames keeping the loop alive, the partial line at EOF
  still answered, the clean EOF exit, and the bounded read — an
  undelimited overrun past 8 MiB answers exactly one `-32700` and
  stops, while a delimited over-limit line is answered `-32600` with
  the loop continuing.
- `tests/stdio_process.rs` — the actual `bpm-engine-mcp-server` binary
  (via `CARGO_BIN_EXE_…`) over real OS pipes: a full session (handshake,
  notification, `tools/list`, a real `tools/call` returning engine data
  from a real `build_app` server, a malformed line mid-session answered
  `-32700` with the loop continuing), then closing stdin and asserting
  the **clean exit 0** (the EOF path); plus streaming ~9 MiB without a
  newline and asserting the process answers one `-32700`
  oversized-frame line and terminates cleanly — the end-to-end proof of
  the read-layer bound.
- `tests/end_to_end.rs` — **MCP → REST → engine**: a real `build_app`
  memory-mode engine on an ephemeral port; the full workflow (validate
  clean/compile-errors/400, definition list/versions, idempotent
  start, instance get/history/steps, task list, fetch-and-lock,
  complete, COMPLETED, capabilities) through MCP lines; the default
  allowlist hiding mutating tools (-32602); engine-side RBAC 403 and
  cross-tenant 404 surfacing as tool errors; an unreachable engine
  becoming a tool error (not a hang).
- `tests/env_config.rs` — env parsing (defaults, precedence, timeout
  fallback, allowlist resolution, and the cleartext-credential guard's
  fail-closed matrix: http+credential+remote refused, https/loopback
  allowed, `BPM_MCP_ALLOW_INSECURE_HTTP=1` override).

## 8. Security notes

- No secret is ever logged or included in a tool result; startup
  prints only the credential MODE.
- Proxies are disabled on the HTTP client: engine credentials must not
  ride an ambient `HTTP_PROXY`, and loopback calls must not be
  intercepted.
- The server binds nothing and listens nowhere; it is a subprocess of
  the MCP client.
- Recommended production setup: JWT mode on the engine
  (`BPM_AUTH__JWT_SECRET`), a dedicated minted token per MCP
  deployment (`--sub mcp --tenant <tenant> --role <role>`), and
  `BPM_MCP_ROLE` set to the SAME role — two aligned least-privilege
  boundaries.
