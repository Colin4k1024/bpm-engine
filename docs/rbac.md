# Identity, RBAC, and tenant isolation (E5-1, issue #23)

The engine derives `tenant`/`actor` identity from **verified credentials**
(JWT) instead of the untrusted `X-Tenant-Id` client header, and authorizes
every `/api/v1` operation against a role→permission matrix. This document
is the normative reference; `docs/openapi.yaml` (security schemes, error
codes) and the agent tool catalog (`GET /capabilities` — auth block and
`conventions.roles`) mirror it.

## Runtime modes

The two configuration credentials define three modes (see
`bpm_config.rs::AuthConfig`):

| Mode | Configured | Request credential | Identity |
|------|-----------|--------------------|----------|
| **JWT** (production) | `auth.jwt_secret` (`BPM_AUTH__JWT_SECRET` / `BPM_JWT_SECRET`) | `Authorization: Bearer <jwt>` | verified principal: `actor`, `tenant`, `role` from claims |
| **API key** (bootstrap) | `auth.api_key` (`BPM_AUTH__API_KEY` / `BPM_API_KEY`) | `X-API-Key: <key>` | admin: FULL access, no role checks |
| **Open** (PoC/dev) | neither | none | anonymous |

Precedence per request (middleware `authenticate`,
`crates/server/rest/src/middleware.rs`):

1. `BPM_JWT_SECRET` configured + a bearer token is presented → verify it
   (HS256, strict expiry, claims). Failure (including a missing token,
   when no API key path is available) → **401 `invalid_token`**. A
   presented JWT always wins over the API key.
2. No bearer token + `BPM_API_KEY` configured → the key is the legacy
   admin/bootstrap fallback. A presented-but-invalid key → 401
   `unauthorized`; no key at all → 401 `invalid_token` ("missing bearer
   token").
3. No `BPM_JWT_SECRET` → pre-E5-1 behavior: the optional API-key layer
   (401 `unauthorized` when configured and missing/invalid), otherwise
   every request is accepted.

The unauthenticated top-level routes (`/health`, `/ready`,
`/capabilities`) sit outside the auth layer in every mode.

## JWT token

HS256, signed with `auth.jwt_secret`. Claims:

| Claim | Type | Meaning |
|-------|------|---------|
| `sub` | string | actor identity (non-empty; recorded in request logs today — persisted audit trail is E5-3) |
| `tenant` | string | tenant scope of every store query (non-empty) |
| `role` | string | `deployer` \| `operator` \| `worker` \| `auditor` (anything else → 401 `invalid_token`) |
| `exp` | u64 | unix-seconds expiry, enforced with ZERO leeway |
| `iat` | u64, optional | issued-at (metadata; not required, not validated) |

Signature, expiry, and claims are verified by the middleware; a failure
answers 401 `invalid_token` with the standard `ErrorResponse` body.

### Minting tokens

Operators mint tokens with the server-crate example (same secret as the
server):

```sh
export BPM_JWT_SECRET=$(...the server's auth.jwt_secret...)
cargo run -p bpm-engine-server-rest --example mint_jwt -- \
    --sub alice --tenant acme --role operator
# → prints the token (default expiry: now + 1h; --exp <unix-secs> or
#   --exp-in <secs> to override)
```

Send it as `Authorization: Bearer <token>` on `/api/v1` requests, e.g.:

```sh
TOKEN=$(cargo run -q -p bpm-engine-server-rest --example mint_jwt -- \
    --sub ops-bot --tenant acme --role operator)
curl -H "Authorization: Bearer $TOKEN" \
    -H 'content-type: application/json' \
    -d '{"process_def_id":"minimal"}' \
    http://127.0.0.1:3000/api/v1/process-instances
```

In Rust, `bpm_engine_server_rest::middleware::mint_jwt(secret, sub, tenant,
role, exp)` is the programmatic helper (also used by the tests). The
server never issues tokens itself — minting is an operator concern.

### The operations console and the worker SDK

The embedded UI console (`ui/`) sends `X-API-Key` + `X-Tenant-Id` and
targets **open mode** (or the API-key bootstrap path); a JWT token field
in the console is intentionally out of scope for E5-1 — the same for the
worker SDK: point a worker at a JWT-mode server by sending the bearer
header with the plain REST contract above (SDK bearer support is
follow-up scope).

## Role → permission matrix

Enforced **before handlers** (403 `forbidden`) for JWT-authenticated
requests only: the admin API key bypasses role checks (documented
bootstrap path), and open mode has no credentials to authorize.

| Operation (`/api/v1`) | deployer | operator | worker | auditor |
|---|---|---|---|---|
| Deploy process definition (`POST /process-definitions/deploy`) | ✓ | | | |
| Activate / deprecate definition (`PUT /process-definitions/:id/...`) | ✓ | | | |
| Start instance (`POST /process-instances`) | | ✓ | | |
| Complete user task (`POST /tasks/:task_id/complete`) | | ✓ | | |
| Approve / reject an approval record (`POST /approvals/:approval_id/decide`, E5-2) | | ✓ | | |
| Pause / resume / cancel an instance (`POST /process-instances/:id/pause` / `resume` / `cancel`, E5-2) | | ✓ | | |
| Replay sessions (`POST /process-instances/:id/replay`, `POST /replay/:sid/step`, `POST /replay/:sid/seek`, `DELETE /replay/:sid`) | | ✓ | | |
| DLQ requeue / delete (`POST /dead-letters/:id/requeue`, `DELETE /dead-letters/:id`) | | ✓ | | |
| External-task lease lifecycle (`fetch-and-lock`, `complete`, `fail`, `extend-lock`) | | | ✓ | |
| **All reads** — every `GET` (instances, trace, history, steps, approval lists, definitions, versions, tasks, forms, dead-letters, invariants, replay snapshot) | ✓ | ✓ | ✓ | ✓ |
| Side-effect-free POSTs: `validate`, `dry-run` (E2-2) | ✓ | ✓ | ✓ | ✓ |

Decisions worth restating:

- **Reads are role-agnostic.** Any authenticated role may read (an
  operator needs to see what it operates on). Cross-tenant reads are
  rejected by *tenancy*, not by role — the auditor role is the *read-only*
  role, not the *only-read* role.
- **Worker-only external-task lifecycle.** The lease lifecycle
  (fetch/complete/fail/extend) belongs to the worker role exclusively:
  an operator cannot complete or fail a worker's task (that would violate
  the "exactly one lease owner" invariant's *intent*, not just its
  letter). Operators administrate failed work through the DLQ
  (requeue/delete), which IS operator-scoped.
- **Fail closed.** A `/api/v1` route missing from the permission table
  denies authenticated requests (the table is drift-guarded by
  `middleware::tests::permission_table_covers_every_registered_api_route`
  against `routes::registered_routes()`).

## Tenant isolation

Under JWT auth the **verified `tenant` claim** scopes everything:

- **Lists** (`process-instances`, `tasks`, `dead-letters`) are store-level
  filtered to the claim tenant (the pre-E5-1 mechanism — now sourced from
  the token instead of the header).
- **Instance-scoped reads and writes** (`GET /process-instances/:id`,
  `trace`, `history`, `steps`, `tasks/:task_id/form`,
  `tasks/:task_id/complete`, replay) of ANOTHER tenant's instance answer
  **404 `not_found`** with the exact body an unknown id produces — the
  response never reveals whether the instance exists.
- **External-task operations** (`complete`, `fail`, `extend-lock`) on
  another tenant's task are refused BEFORE any state changes with the
  same body an unknown task id produces (**400 `task_not_found`**).
- **Replay sessions** are bound to the creating tenant; another tenant's
  session (and an admin-created session) answers 404 like an expired one.
- **Dead letters** inherit the failing worker's claim tenant at insert
  (open/API-key inserts stay untagged, as before); tenant-filtered lists
  and the get/requeue/delete gates then apply (404 for other tenants).

### Worker fetch-and-lock tenancy (design decision)

`ExternalTaskStore::fetch_and_lock` predates tenancy and takes no tenant
parameter, but it **locks** the tasks it selects. Filtering its *results*
at the REST layer would be worse than leaking: tenant B's worker would
have silently *stolen the lease* of tenant A's task for the whole lock
duration. E5-1 therefore adds a store-level primitive:

```rust
async fn fetch_and_lock_scoped(worker_id, task_types, max_tasks,
                               lock_duration, tenant_id)
    -> anyhow::Result<Vec<ExternalTask>>;
```

implemented by both adapters (memory: the tenant check runs inside the
select-and-lock critical section; postgres: the `UPDATE ... FOR UPDATE
SKIP LOCKED` selection joins `process_instance` on `tenant_id`). Tasks of
another tenant — or of a tenant-less instance — are **never selected and
never leased**; the unscoped `fetch_and_lock` (used in open and API-key
modes, and by the admin path) is unchanged.

Un-scoped notes (honest v1 boundary):

- **Process definitions are engine-global**: every tenant reads them, only
  deployers modify them. Definitions are code, not tenant data; per-tenant
  definition namespaces are future scope.
- **The DLQ list of a tenant excludes legacy untagged entries** (inserted
  before a JWT deployment or via the admin path). Manage those with the
  API key.
- `/api/v1/invariants/check` reports global storage invariants to any
  authenticated role (read).

## X-Tenant-Id deprecation

`X-Tenant-Id` remains what it always was — **untrusted client input**:

- **JWT mode: deprecated.** The tenant comes from the verified claim. An
  absent header is fine, a header AGREEING with the claim is fine (and
  ignored), and a header **conflicting** with the claim answers
  **403 `forbidden`** (a client that believes it is tenant B while
  presenting tenant A's credentials is either buggy or probing).
- **Open / API-key modes: unchanged dev-mode affordance.** The header
  scopes listings, instance starts, and idempotency keys exactly as
  before E5-1 (trusted only as much as the client, which is to say: not
  at all — one more reason JWT mode exists).

## Error codes

Stable codes in `routes::error_codes::ERROR_CODES` (mirrored in
`docs/openapi.yaml` and the catalog — both directions asserted by the
consistency suite):

| Code | HTTP | When |
|------|------|------|
| `invalid_token` | 401 | JWT presented/required but missing, malformed, or rejected (signature, expiry, claims) |
| `forbidden` | 403 | RBAC denial, or `X-Tenant-Id` conflicting with the tenant claim |
| `approval_not_found` | 404 | unknown approval record id; another tenant's record answers identically (E5-2) |
| `approval_already_decided` | 409 | a CONFLICTING decide on a decided approval record — the original outcome stands (E5-2) |
| `instance_paused` | 409 | task complete/fail on a paused instance (E5-2) |
| `instance_cancelled` | 409 | task operations on a cancelled instance (E5-2) |
| `instance_not_pausable` | 409 | pause/resume on a terminal instance (E5-2) |

## Boundaries and forward references

- **Actor persistence is E5-3.** The verified `actor` is part of the
  request context (`Identity`, request logs) but is NOT yet written into
  history events / the audit trail — that is E5-3's deliverable, and the
  engine core has no actor field today.
- **Token issuance/rotation/revocation** is out of scope: the server only
  verifies. Operators mint tokens out-of-band (example above); short
  expiry plus secret rotation is the v1 story.
- **Shared-secret single-service boundary:** the JWT is HS256 with one
  shared secret and validates `sub`/`tenant`/`role`/`exp` only — there
  are no `iss`/`aud` claims. Any holder of the secret can mint any
  tenant/role, and a token minted for one consuming service is accepted
  by every service sharing that secret. v1 therefore relies entirely on
  secret confidentiality and rotation; if the engine is deployed where
  multiple services share the secret or tokens cross service
  boundaries, add `iss`/`aud` validation before relying on this
  boundary. `iat`/`nbf` are likewise not validated (the replay window
  of a leaked token is bounded by `exp` alone).
- Rate limiting is unchanged: the limiter is still the outermost `/api/v1`
  layer and counts every request, authenticated or not.
- Backward compatibility: open mode is byte-identical to pre-E5-1 (no
  credentials configured → the same accept-all behavior, the same
  `X-Tenant-Id` semantics, no per-request tenant gates); API-key mode
  keeps its 401 `unauthorized` contract and full access.
