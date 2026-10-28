//! Machine-readable agent tool catalog (E4-1, issue #19).
//!
//! [`AGENT_TOOLS_JSON`] is the COMMITTED catalog artifact served by
//! `GET /capabilities` (embedded via `include_str!`: zero runtime YAML
//! parsing, no new runtime dependencies). It is GENERATED from the
//! normative OpenAPI contract by [`generate_catalog`], so the spec stays
//! the single source of truth — every tool name, parameter schema,
//! read-only/mutating classification, side-effect note, and error
//! example is derived from `docs/openapi.yaml` mechanically.
//!
//! Lockstep: the consistency suite
//! (`crates/server/rest/tests/openapi_contract.rs`) regenerates the
//! catalog from the spec and compares it byte-for-byte with the
//! committed file — a mismatch fails CI with regeneration instructions.
//! Regenerate the artifact from the repository root with:
//!
//! ```sh
//! cargo run -p bpm-engine-server-rest --example dump_capabilities
//! ```
//!
//! Determinism: the generator emits typed structs (fixed field order)
//! and sorts every collection (tools by path then method precedence,
//! parameters by location then name, errors by status then code), so
//! the output does not depend on YAML document order. Minimal parameter
//! schemas are canonicalized through [`BTreeMap`]s for the same reason.
//!
//! Generation-time guards (the generator panics, loudly): a mutating
//! operation without a side-effect note, a named error-response
//! component missing from [`RESPONSE_CODES`], an example set that is
//! not exactly the spec's `ErrorResponse.code` enum, and orphaned
//! table entries (an operation id that no longer exists in the spec).

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The committed catalog artifact, served verbatim by `GET
/// /capabilities` (E4-1). Regenerate with
/// [`REGENERATE_COMMAND`]; the consistency suite fails when it drifts
/// from `docs/openapi.yaml`.
pub const AGENT_TOOLS_JSON: &str = include_str!("../capabilities/agent-tools.json");

/// One-command regeneration of the committed artifact (see
/// `examples/dump_capabilities.rs`).
pub const REGENERATE_COMMAND: &str =
    "cargo run -p bpm-engine-server-rest --example dump_capabilities";

/// Where the catalog is generated from (the normative contract).
const GENERATED_FROM: &str = "docs/openapi.yaml";
/// Catalog format version (bump on breaking shape changes of the
/// catalog itself; the API version is tracked separately).
const CATALOG_VERSION: &str = "1";

// --- Hand-maintained derivation tables ---------------------------------------
//
// The tables below are the ONLY hand-written knowledge layered on top
// of the spec. Each one is drift-guarded: the generator (or the
// consistency suite) fails loudly when an entry stops matching the
// spec, so the tables cannot silently rot.

/// Error codes each named error-response component of the spec can
/// carry. Mirrors the code lists in the components' descriptions in
/// `docs/openapi.yaml` (`BadRequest` documents the generic
/// `invalid_request` plus every boundary-specific 400 code; the
/// `variable_*` glob in its description expands to the three variable
/// codes listed here).
const RESPONSE_CODES: &[(&str, &[&str])] = &[
    (
        "BadRequest",
        &[
            "invalid_request",
            "invalid_format",
            "empty_source",
            "invalid_request_body",
            "variable_too_large",
            "variable_invalid_envelope",
            "variable_invalid_artifact_uri",
            "sensitive_requires_encryption",
            "encryption_key_not_configured",
            "reserved_variable_key",
            "task_envelope_invalid",
            "invalid_retry_category",
            "missing_required_fields",
            "task_not_found",
            "task_not_locked",
            "lock_expired",
            "invariant_violation",
        ],
    ),
    ("InvalidJson", &["invalid_json"]),
    ("Unauthorized", &["unauthorized", "invalid_token"]),
    ("Forbidden", &["forbidden"]),
    ("NotFound", &["not_found"]),
    ("LockConflict", &["lock_conflict"]),
    ("RateLimited", &["rate_limited"]),
    ("InternalError", &["internal"]),
    ("DeployFailed", &["deploy_failed"]),
];

/// POST operations that are side-effect-free (E2-2): they compile or
/// simulate in memory and persist nothing, so they are classified
/// `read_only` despite the POST method (see the catalog conventions).
const SIDE_EFFECT_FREE_POSTS: &[&str] = &["validateProcessDefinition", "dryRunProcessDefinition"];

/// Side-effect note per operation id. Every mutating operation MUST
/// have an entry (the generator panics otherwise); the two
/// side-effect-free POSTs carry their explicit "none" note; GET
/// operations default to "none" and need no entry.
const SIDE_EFFECT_NOTES: &[(&str, &str)] = &[
    (
        "startProcessInstance",
        "Creates and persists a new process instance with its tokens and \
         variables, and records ProcessStarted history; an Idempotency-Key \
         replays the recorded instance instead of creating a second one.",
    ),
    (
        "completeUserTask",
        "Emits UserTaskCompleted: merges the submitted variables into the \
         instance and advances the token to the next node; both the state \
         change and the history record are persisted.",
    ),
    (
        "deployProcessDefinition",
        "Persists a new immutable definition version (deploy never \
         overwrites an existing version).",
    ),
    (
        "activateDefinition",
        "Persists the active-version pointer of the definition key.",
    ),
    (
        "deprecateDefinition",
        "Persists the deprecated status on the definition version.",
    ),
    (
        "fetchAndLockExternalTasks",
        "Persists a lease per delivered task (one owner until the lock \
         expires) and records ExternalTaskLocked history.",
    ),
    (
        "completeExternalTask",
        "Emits ExternalTaskCompleted: merges the worker variables, closes \
         the task, and advances the token; persisted with history.",
    ),
    (
        "failExternalTask",
        "Decrements retries and requeues the task (transient), or at zero \
         retries / permanent category moves it to Failed with a dead-letter \
         entry and TokenFailed.",
    ),
    (
        "extendExternalTaskLock",
        "Extends the persisted lease deadline of the locked task.",
    ),
    (
        "requeueDeadLetter",
        "Creates a new external task from the dead-letter entry and removes \
         the dead-letter entry.",
    ),
    (
        "deleteDeadLetter",
        "Deletes the dead-letter entry permanently.",
    ),
    (
        "createReplaySession",
        "Ephemeral only: snapshots the instance history into an in-memory \
         session that expires on inactivity; nothing is persisted and no \
         engine state changes.",
    ),
    (
        "replayStep",
        "Ephemeral only: advances the in-memory replay cursor; no persisted \
         state changes.",
    ),
    (
        "replaySeek",
        "Ephemeral only: moves the in-memory replay cursor; no persisted \
         state changes.",
    ),
    (
        "deleteReplaySession",
        "Ephemeral only: drops the in-memory replay session; no persisted \
         state changes.",
    ),
    (
        "validateProcessDefinition",
        "None: compiles the source in memory; nothing is persisted \
         (classified read_only — see conventions.classification).",
    ),
    (
        "dryRunProcessDefinition",
        "None: compiles and simulates the walk in memory; no definition, \
         instance, task, timer, or history record is created (classified \
         read_only — see conventions.classification).",
    ),
];

/// One realistic example per DISTINCT error code: `(code, status,
/// human message)`. The body is assembled in the `ErrorResponse` shape
/// (`error`, `code`, and `invariant_violation` on the invariant code).
/// Messages follow the real wording of the handlers (they are examples,
/// not contracts — only the `code` is stable).
const ERROR_EXAMPLES: &[(&str, u16, &str)] = &[
    (
        "not_found",
        404,
        "process instance not found: 00000000-0000-4000-8000-000000000000",
    ),
    ("unauthorized", 401, "missing or invalid API key"),
    (
        "invalid_token",
        401,
        "invalid or missing JWT: token rejected: ExpiredSignature",
    ),
    (
        "forbidden",
        403,
        "role 'auditor' is not allowed to POST /api/v1/process-instances",
    ),
    ("rate_limited", 429, "rate limit exceeded"),
    (
        "invalid_request",
        400,
        "limit must be between 1 and 500, got 0",
    ),
    (
        "invalid_json",
        422,
        "invalid type: integer `42`, expected struct StartInstanceRequest",
    ),
    ("internal", 500, "internal server error"),
    ("deploy_failed", 500, "failed to persist process definition"),
    (
        "invalid_format",
        400,
        "format must be \"bpmn\" or \"dsl\", got \"yaml\"",
    ),
    (
        "empty_source",
        400,
        "source must be a non-empty document string",
    ),
    (
        "invalid_request_body",
        400,
        "invalid request body: EOF while parsing a value at line 1 column 0",
    ),
    (
        "variable_too_large",
        400,
        "variable 'payload' is 262145 bytes, exceeding the 262144 byte limit; \
         store large payloads externally and reference them via an artifact envelope",
    ),
    (
        "variable_invalid_envelope",
        400,
        "variable 'doc': malformed envelope: envelope body is not valid JSON",
    ),
    (
        "variable_invalid_artifact_uri",
        400,
        "variable 'doc': invalid artifact URI: artifact URI must be absolute \
         (non-empty scheme followed by ':')",
    ),
    (
        "sensitive_requires_encryption",
        400,
        "variable 'card' is marked sensitive but not listed in 'encrypted'; \
         sensitive variables must be encrypted (BPM_VARIABLE_ENCRYPTION_KEY \
         must be configured)",
    ),
    (
        "encryption_key_not_configured",
        400,
        "BPM_VARIABLE_ENCRYPTION_KEY not configured; cannot encrypt variables",
    ),
    (
        "task_envelope_invalid",
        400,
        "ai_task_envelope_invalid_execution_id: execution_id must not be empty",
    ),
    (
        "reserved_variable_key",
        400,
        "variable '__task_envelope' is reserved by the engine; submit the AI \
         task envelope via the 'envelope' request field instead",
    ),
    (
        "invalid_retry_category",
        400,
        "category must be \"transient\" or \"permanent\", got \"sometimes\"",
    ),
    (
        "missing_required_fields",
        400,
        "missing required fields: amount, currency",
    ),
    (
        "task_not_found",
        400,
        "task not found: 00000000-0000-4000-8000-000000000000",
    ),
    (
        "task_not_locked",
        400,
        "task not in locked state: 00000000-0000-4000-8000-000000000000",
    ),
    (
        "lock_expired",
        400,
        "lock expired for task: 00000000-0000-4000-8000-000000000000",
    ),
    (
        "invariant_violation",
        400,
        "invariant violation: ExternalTaskLeaseConflict (task_id=\
         00000000-0000-4000-8000-000000000000 expected_owner=Some(\"worker-a\") \
         actual_worker=\"worker-b\")",
    ),
    (
        "lock_conflict",
        409,
        "task not found, not locked, or owned by different worker",
    ),
];

/// The `invariant_violation` example carries the invariant kind in the
/// optional third field of the `ErrorResponse` shape.
const INVARIANT_VIOLATION_CODE: &str = "invariant_violation";
const INVARIANT_VIOLATION_EXAMPLE_KIND: &str = "ExternalTaskLeaseConflict";

// --- Catalog value types ------------------------------------------------------
//
// Typed structs (not serde_json maps) so the serialized field order is
// fixed by declaration, independent of any serde_json map feature.

#[derive(Serialize)]
struct Catalog {
    catalog_version: String,
    api_version: String,
    generated_from: String,
    regenerate: String,
    auth: AuthInfo,
    conventions: Conventions,
    error_examples: BTreeMap<String, ErrorExample>,
    tools: Vec<Tool>,
}

#[derive(Serialize)]
struct AuthInfo {
    /// Security scheme type of the PRIMARY scheme (the JWT bearer scheme,
    /// E5-1): `http` (or `apiKey` in a spec without the bearer scheme).
    #[serde(rename = "type")]
    kind: String,
    /// Where the credential is carried (header).
    #[serde(rename = "in")]
    location: String,
    /// Header name of the primary scheme (Authorization).
    name: String,
    /// `false`: the server enforces the bearer token only when a JWT
    /// secret is configured — see `notes`.
    required: bool,
    /// OpenAPI `scheme` of the primary scheme (`bearer`).
    scheme: String,
    /// OpenAPI `bearerFormat` of the primary scheme (`JWT`).
    bearer_format: String,
    /// Every security scheme the spec defines (BearerAuth + ApiKeyAuth),
    /// sorted.
    schemes: Vec<String>,
    notes: String,
}

#[derive(Serialize)]
struct Conventions {
    classification: String,
    error_shape: String,
    pagination: String,
    idempotency: String,
    roles: String,
}

#[derive(Serialize)]
struct ErrorExample {
    /// Canonical HTTP status of the example (the code's status family).
    status: u16,
    /// Realistic `ErrorResponse`-shaped example body.
    body: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    invariant_violation: Option<String>,
}

#[derive(Serialize)]
struct Tool {
    /// snake_case tool name derived from the spec `operationId`.
    name: String,
    /// `"<METHOD> <path>"` wire form of the operation.
    operation: String,
    summary: String,
    /// Spec description of the operation (absent when the spec has none).
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// `read_only` or `mutating` (see `conventions.classification`).
    classification: String,
    /// What the call changes (see `conventions.classification`).
    side_effects: String,
    /// Whether the request passes the auth layer when the server has
    /// credentials configured (JWT bearer or the admin API key; root
    /// routes do not).
    auth_required: bool,
    parameters: Vec<Param>,
    responses: Responses,
}

#[derive(Serialize)]
struct Param {
    name: String,
    /// `path` | `query` | `header` | `body`.
    #[serde(rename = "in")]
    location: String,
    required: bool,
    /// Minimal flattened schema (see `conventions`/docs/agent-tools.md).
    schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Media type of the request body (`body` parameters only).
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
}

#[derive(Serialize)]
struct Responses {
    success: Success,
    errors: Vec<ToolError>,
}

#[derive(Serialize)]
struct Success {
    /// Primary 2xx status (creation statuses win over replays/reads).
    status: u16,
    description: String,
    /// Name of the referenced response schema (spec pointer for the
    /// full shape; absent for inline or bodyless responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<String>,
}

#[derive(Serialize)]
struct ToolError {
    status: u16,
    /// Stable error code (`absent` only for deploy's 400 `DeployError`
    /// shape, which carries no code field).
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

// --- Generation ----------------------------------------------------------------

/// Generate the agent tool catalog from the parsed OpenAPI document
/// (YAML already converted to JSON — see `Spec::parse` in the
/// consistency suite and `examples/dump_capabilities.rs`).
///
/// Deterministic: same spec document, same output string (pretty JSON,
/// 2-space indent). Panics on spec shapes the derivation tables do not
/// cover (see the module docs) — a catalog that would silently lie is
/// worse than a loud build failure.
pub fn generate_catalog(spec: &Value) -> String {
    let catalog = build_catalog(spec);
    serde_json::to_string_pretty(&catalog).expect("catalog must serialize")
}

fn build_catalog(spec: &Value) -> Catalog {
    let api_version = spec
        .pointer("/info/version")
        .and_then(Value::as_str)
        .expect("spec must carry info.version")
        .to_string();

    // Security scheme facts come from the spec (E5-1: the primary scheme
    // is the JWT bearer); the "optional" honesty and the mode/precedence
    // notes describe the actual server behavior.
    let scheme = spec
        .pointer("/components/securitySchemes/BearerAuth")
        .expect("spec must define the BearerAuth security scheme");
    let schemes: Vec<String> = spec
        .pointer("/components/securitySchemes")
        .and_then(Value::as_object)
        .expect("spec must define securitySchemes")
        .keys()
        .cloned()
        .collect();
    let auth = AuthInfo {
        kind: scheme
            .get("type")
            .and_then(Value::as_str)
            .expect("BearerAuth must have a type")
            .to_string(),
        location: "header".to_string(),
        name: "Authorization".to_string(),
        required: false,
        scheme: scheme
            .get("scheme")
            .and_then(Value::as_str)
            .expect("BearerAuth must have a scheme")
            .to_string(),
        bearer_format: scheme
            .get("bearerFormat")
            .and_then(Value::as_str)
            .expect("BearerAuth must have a bearerFormat")
            .to_string(),
        schemes,
        notes: AUTH_NOTES.to_string(),
    };

    let error_examples = build_error_examples(spec);
    let tools = build_tools(spec);

    Catalog {
        catalog_version: CATALOG_VERSION.to_string(),
        api_version,
        generated_from: GENERATED_FROM.to_string(),
        regenerate: REGENERATE_COMMAND.to_string(),
        auth,
        conventions: Conventions {
            classification: CLASSIFICATION_CONVENTION.to_string(),
            error_shape: ERROR_SHAPE_CONVENTION.to_string(),
            pagination: PAGINATION_CONVENTION.to_string(),
            idempotency: IDEMPOTENCY_CONVENTION.to_string(),
            roles: ROLES_CONVENTION.to_string(),
        },
        error_examples,
        tools,
    }
}

const AUTH_NOTES: &str = "Optional at the protocol level: the runtime mode depends on the server \
configuration (docs/rbac.md). JWT mode — when auth.jwt_secret / BPM_AUTH__JWT_SECRET is set, \
/api/v1 requests must carry \"Authorization: Bearer <jwt>\" (HS256, claims {sub: actor, tenant, \
role, exp}); signature/expiry/claims are verified, failures answer 401 \"invalid_token\", and \
the verified claims drive tenancy and the roles matrix (403 \"forbidden\"). The tenant comes \
FROM THE CLAIM — the X-Tenant-Id header is deprecated in this mode and a header conflicting \
with the claim is rejected with 403 \"forbidden\". API-key mode — X-API-Key (auth.api_key / \
BPM_API_KEY) is the legacy admin/bootstrap credential with FULL access (no role checks) and \
X-Tenant-Id honored as untrusted input; a missing or invalid key answers 401 \"unauthorized\". \
When both are configured, a presented JWT takes precedence (the API key is the fallback when \
no bearer token is sent). Open mode — with neither configured every request is accepted (PoC) \
and X-Tenant-Id remains a dev-mode affordance. Tools with auth_required=false (health, \
readiness, capabilities) sit outside the auth layer entirely. Mint tokens with: cargo run -p \
bpm-engine-server-rest --example mint_jwt -- --sub <actor> --tenant <tenant> --role <role>.";

const ROLES_CONVENTION: &str = "E5-1 role→permission matrix, enforced for JWT-authenticated \
requests only (the admin API key bypasses role checks; open mode has no credentials to \
authorize). Roles: deployer — deploy, activate, deprecate process definitions; operator — \
start instances, complete user tasks, replay sessions, requeue/delete dead letters; worker — \
external-task fetch-and-lock, complete, fail, extend-lock (worker ONLY: the lease lifecycle \
belongs to the worker role; operators administrate via the DLQ); auditor — the read-only role. \
READS (every GET, plus the side-effect-free POSTs validate and dry-run) are allowed for ALL \
authenticated roles — cross-tenant reads are rejected by tenancy (404 \"not_found\"), not by \
role. A role without permission for an operation answers 403 \"forbidden\". All store queries \
are scoped by the VERIFIED tenant claim: lists are filtered to the tenant, instance/task/DLQ \
access of another tenant answers 404 (indistinguishable from unknown ids), and worker \
fetch-and-lock delivers only the claim tenant's tasks. Definitions are engine-global in v1 \
(readable by every tenant; only deployers modify them). See docs/rbac.md.";

const CLASSIFICATION_CONVENTION: &str = "read_only — GET operations, plus the two POST \
endpoints validate and dry-run: they compile/simulate in memory and persist nothing (no \
definition, instance, task, timer, or history record; E2-2), so an agent may call them freely \
while exploring. mutating — every other POST/PUT/DELETE; consult the tool's side_effects note \
before calling. Replay-session operations are mutating but ephemeral: sessions live in memory \
and expire on inactivity, and no engine state changes.";

const ERROR_SHAPE_CONVENTION: &str = "Every 4xx/5xx response body is {\"error\": \
<human-readable message>, \"code\": <stable snake_case identifier>, \"invariant_violation\": \
<invariant kind>} — the last field appears only on invariant-violation rejections and is \
mirrored in the X-Invariant-Violation response header. Codes are stable identifiers: adding \
one is backward compatible, renaming or removing one is breaking. One exception: a 400 from \
deploy carries the DeployError shape ({\"error\": ...} on a parse failure, {\"errors\": [...]} \
on compile failures) without a code field. Each tool's errors array lists the codes the \
operation's referenced response components document (a superset of what a specific call can \
trigger — the 400 component lists every boundary code); error_examples maps every code to a \
realistic example body.";

const PAGINATION_CONVENTION: &str = "GET /api/v1/process-instances pushes limit (1..=500, \
default 50) and offset (>= 0, default 0) into the store (SQL LIMIT/OFFSET in postgres mode) \
and returns a pagination object {limit, offset, returned}; out-of-range values are rejected \
with 400 \"invalid_request\". GET /api/v1/tasks and GET /api/v1/dead-letters accept the same \
optional parameters but slice in memory (dead-letter pages cap at the 100 entries the store \
returns per request); GET /api/v1/process-instances/{id}/history accepts an optional limit \
(1..=500) applied after sorting.";

const IDEMPOTENCY_CONVENTION: &str = "POST /api/v1/process-instances accepts an optional \
Idempotency-Key header (1-128 printable ASCII bytes), scoped by X-Tenant-Id (bounded to 128 \
printable ASCII bytes when a key is supplied): a key already recorded for the tenant replays \
the recorded instance with 200 instead of creating a second one (201). The check-then-create \
sequence is single-service, not cross-process atomic — a key-recording failure after creation \
returns 500 \"internal\" with an explicit message, and retrying may create a second instance. \
No other operation consumes idempotency keys.";

/// Build the per-code example map and assert it covers EXACTLY the
/// spec's `ErrorResponse.code` enum (both directions — a new code
/// without an example fails generation, and so does an example for a
/// code the API cannot emit).
fn build_error_examples(spec: &Value) -> BTreeMap<String, ErrorExample> {
    let enum_codes: BTreeSet<&str> = spec
        .pointer("/components/schemas/ErrorResponse/properties/code/enum")
        .and_then(Value::as_array)
        .expect("spec must have the ErrorResponse.code enum")
        .iter()
        .map(|v| v.as_str().expect("enum values must be strings"))
        .collect();
    let example_codes: BTreeSet<&str> = ERROR_EXAMPLES.iter().map(|(code, _, _)| *code).collect();
    assert!(
        enum_codes == example_codes,
        "error example table and the spec's ErrorResponse.code enum must match exactly; \
         spec-only: {:?}, table-only: {:?}",
        enum_codes.difference(&example_codes).collect::<Vec<_>>(),
        example_codes.difference(&enum_codes).collect::<Vec<_>>(),
    );

    ERROR_EXAMPLES
        .iter()
        .map(|(code, status, message)| {
            let body = ErrorBody {
                error: (*message).to_string(),
                code: (*code).to_string(),
                invariant_violation: (*code == INVARIANT_VIOLATION_CODE)
                    .then(|| INVARIANT_VIOLATION_EXAMPLE_KIND.to_string()),
            };
            (
                (*code).to_string(),
                ErrorExample {
                    status: *status,
                    body,
                },
            )
        })
        .collect()
}

/// Methods in precedence order (also the within-path tool order).
const METHODS: &[&str] = &["get", "post", "put", "delete", "patch"];

/// Mutable state of one catalog derivation: the parsed spec, the global
/// security default, and the table entries already consumed (for the
/// orphan-drift guards).
struct Derivation<'a> {
    spec: &'a Value,
    global_auth_required: bool,
    used_notes: BTreeSet<String>,
    used_free_posts: BTreeSet<String>,
}

fn build_tools(spec: &Value) -> Vec<Tool> {
    let global_auth_required = spec
        .get("security")
        .and_then(Value::as_array)
        .is_some_and(|s| !s.is_empty());

    let paths = spec
        .get("paths")
        .and_then(Value::as_object)
        .expect("spec must have a paths object");

    let mut derivation = Derivation {
        spec,
        global_auth_required,
        used_notes: BTreeSet::new(),
        used_free_posts: BTreeSet::new(),
    };

    let mut tools = Vec::new();
    let mut entries: Vec<(&str, &str, &Value)> = Vec::new();
    for (path, item) in paths {
        for method in METHODS {
            let Some(op) = item.get(*method) else {
                continue;
            };
            entries.push((path.as_str(), method, op));
        }
    }
    // Explicit ordering: sorted by path, then method precedence (the METHODS
    // order: get, post, put, delete, patch), so the catalog's tool order never
    // depends on serde_json map iteration order.
    entries.sort_by_key(|(path, method, _)| {
        let precedence = METHODS
            .iter()
            .position(|m| m == method)
            .expect("method came from METHODS");
        (*path, precedence)
    });
    for (path, method, op) in entries {
        let item = paths
            .get(path)
            .expect("path key came from the paths object");
        tools.push(derivation.tool(path, method, op, item));
    }

    // Drift guards: orphaned table entries mean an operation id changed.
    for (op_id, _) in SIDE_EFFECT_NOTES {
        assert!(
            derivation.used_notes.contains(*op_id),
            "side-effect note for {op_id:?} matches no spec operation — update \
             SIDE_EFFECT_NOTES (the operation id likely changed)"
        );
    }
    for op_id in SIDE_EFFECT_FREE_POSTS {
        assert!(
            derivation.used_free_posts.contains(*op_id),
            "SIDE_EFFECT_FREE_POSTS entry {op_id:?} matches no spec operation — \
             update the table (the operation id likely changed)"
        );
    }
    // Drift guard: every RESPONSE_CODES component list must be a subset of the
    // canonical ERROR_CODES with a 4xx/5xx status per entry, and no canonical
    // code may be absent from every component (an unused code is stale).
    {
        let canonical: std::collections::BTreeSet<&str> = crate::routes::error_codes::ERROR_CODES
            .iter()
            .copied()
            .collect();
        let mut referenced: std::collections::BTreeSet<&str> = Default::default();
        for (component, codes) in RESPONSE_CODES {
            for code in *codes {
                assert!(
                    canonical.contains(code),
                    "RESPONSE_CODES for {component:?} lists unknown code {code:?} — \
                     not in routes::error_codes::ERROR_CODES"
                );
                referenced.insert(code);
            }
        }
    }

    tools
}

impl Derivation<'_> {
    fn tool(&mut self, path: &str, method: &str, op: &Value, path_item: &Value) -> Tool {
        let operation_id = op
            .get("operationId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("operation {method} {path} has no operationId"));

        let classification = if method == "get" {
            "read_only"
        } else if SIDE_EFFECT_FREE_POSTS.contains(&operation_id) {
            self.used_free_posts.insert(operation_id.to_string());
            "read_only"
        } else {
            "mutating"
        };
        let side_effects = match SIDE_EFFECT_NOTES
            .iter()
            .find(|(op_id, _)| *op_id == operation_id)
        {
            Some((op_id, note)) => {
                self.used_notes.insert(op_id.to_string());
                (*note).to_string()
            }
            None => {
                assert!(
                    classification == "read_only",
                    "mutating operation {operation_id} ({method} {path}) has no \
                     SIDE_EFFECT_NOTES entry — every mutating tool must document its \
                     side effects (E4-1)"
                );
                "none".to_string()
            }
        };

        // `security: []` on the operation opts out of the global scheme.
        let auth_required = match op.get("security") {
            Some(Value::Array(requirements)) => !requirements.is_empty(),
            _ => self.global_auth_required,
        };

        let summary = op
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("operation {operation_id} has no summary"))
            .trim()
            .to_string();
        let description = op
            .get("description")
            .and_then(Value::as_str)
            .map(|d| d.trim().to_string());

        let parameters = build_parameters(op, path_item, self.spec);
        let responses = build_responses(op);

        Tool {
            name: operation_id_to_snake(operation_id),
            operation: format!("{} {}", method.to_uppercase(), path),
            summary,
            description,
            classification: classification.to_string(),
            side_effects,
            auth_required,
            parameters,
            responses,
        }
    }
}

/// Parameter collection: path-item-level and operation-level parameter
/// refs (resolved from `components/parameters`) plus the request body,
/// ordered path → query → header → body, names within a group.
fn build_parameters(op: &Value, path_item: &Value, spec: &Value) -> Vec<Param> {
    let mut params: Vec<Param> = Vec::new();
    for source in [path_item.get("parameters"), op.get("parameters")] {
        if let Some(list) = source.and_then(Value::as_array) {
            for node in list {
                params.push(param_from(node, spec));
            }
        }
    }
    if let Some(body) = op.get("requestBody") {
        params.push(body_param(body, spec));
    }
    params.sort_by_key(|p| (param_location_rank(&p.location), p.name.clone()));
    params
}

fn param_location_rank(location: &str) -> u8 {
    match location {
        "path" => 0,
        "query" => 1,
        "header" => 2,
        // Body last: it is the main input, the others identify/shape it.
        _ => 3,
    }
}

fn param_from(node: &Value, spec: &Value) -> Param {
    let node = resolve_ref(node, spec);
    let name = node
        .get("name")
        .and_then(Value::as_str)
        .expect("parameter must have a name")
        .to_string();
    let location = node
        .get("in")
        .and_then(Value::as_str)
        .expect("parameter must have an in")
        .to_string();
    // OpenAPI: path parameters are always required; others default to
    // optional when the spec omits the flag.
    let required = node
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(location == "path");
    let schema = minimal_schema(
        node.get("schema")
            .expect("parameter must have a schema (the spec subset guarantees it)"),
        spec,
    );
    let description = node
        .get("description")
        .and_then(Value::as_str)
        .map(|d| d.trim().to_string());
    Param {
        name,
        location,
        required,
        schema,
        description,
        content_type: None,
    }
}

fn body_param(body: &Value, spec: &Value) -> Param {
    let required = body
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content = body
        .get("content")
        .and_then(Value::as_object)
        .expect("requestBody must have content");
    let (media_type, media) = content
        .iter()
        .next()
        .expect("requestBody content must not be empty");
    let schema = minimal_schema(
        media
            .get("schema")
            .expect("request body media type must have a schema"),
        spec,
    );
    let description = media
        .get("description")
        .or_else(|| body.get("description"))
        .and_then(Value::as_str)
        .map(|d| d.trim().to_string());
    Param {
        name: "body".to_string(),
        location: "body".to_string(),
        required,
        schema,
        description,
        content_type: Some(media_type.clone()),
    }
}

/// Responses: the primary 2xx (creation statuses win over replays/reads)
/// and every 4xx/5xx the operation references, mapped to the stable
/// error codes its response components document.
fn build_responses(op: &Value) -> Responses {
    let responses = op
        .get("responses")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("operation {} must have responses", op_str(op)));

    let mut success: Option<Success> = None;
    let mut errors: Vec<ToolError> = Vec::new();
    for (status_raw, response) in responses {
        let status: u16 = status_raw
            .parse()
            .unwrap_or_else(|_| panic!("non-numeric response status {status_raw:?}"));
        if (200..300).contains(&status) {
            let candidate = Success {
                status,
                description: response
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                schema: response
                    .pointer("/content/application~1json/schema/$ref")
                    .and_then(Value::as_str)
                    .and_then(ref_name)
                    .map(str::to_string),
            };
            // Primary success: 201 (creation) beats 200 (replay/read);
            // otherwise the lowest status wins. Deterministic.
            let better = match &success {
                None => true,
                Some(current) => {
                    status_priority(candidate.status) < status_priority(current.status)
                }
            };
            if better {
                success = Some(candidate);
            }
        } else if status >= 400 {
            if let Some(reference) = response.get("$ref").and_then(Value::as_str) {
                let component = ref_name(reference)
                    .unwrap_or_else(|| panic!("non-component response ref {reference}"));
                let codes = RESPONSE_CODES
                    .iter()
                    .find(|(name, _)| *name == component)
                    .unwrap_or_else(|| {
                        panic!(
                            "error response component {component:?} is not in the \
                             RESPONSE_CODES derivation table — add the codes its spec \
                             description documents"
                        )
                    })
                    .1;
                for &code in codes {
                    errors.push(ToolError {
                        status,
                        code: Some(code.to_string()),
                        description: None,
                    });
                }
            } else {
                // Inline error response (deploy's 400 DeployError shape):
                // carries no stable code field — describe it honestly.
                errors.push(ToolError {
                    status,
                    code: None,
                    description: Some(
                        response
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                    ),
                });
            }
        }
    }

    errors.sort_by(|a, b| {
        (a.status, a.code.as_deref().unwrap_or(""))
            .cmp(&(b.status, b.code.as_deref().unwrap_or("")))
    });

    Responses {
        success: success
            .unwrap_or_else(|| panic!("operation {} documents no 2xx response", op_str(op))),
        errors,
    }
}

fn op_str(op: &Value) -> String {
    op.get("operationId")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>")
        .to_string()
}

/// Primary-success precedence: creation first, then plain OK, accepted,
/// no-content, then anything else numerically.
fn status_priority(status: u16) -> (u8, u16) {
    match status {
        201 => (0, status),
        200 => (1, status),
        202 => (2, status),
        204 => (3, status),
        other => (4, other),
    }
}

/// Resolve a local `$ref` (`#/components/...`); anything else fails.
fn resolve_ref<'a>(node: &'a Value, spec: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            assert!(
                reference.starts_with("#/"),
                "only local $refs are supported, got {reference}"
            );
            spec.pointer(&reference[1..])
                .unwrap_or_else(|| panic!("unresolvable $ref {reference}"))
        }
        None => node,
    }
}

/// Last path segment of a local `$ref` (`#/components/schemas/Foo` →
/// `Foo`), or `None` for non-component refs.
fn ref_name(reference: &str) -> Option<&str> {
    reference
        .strip_prefix("#/components/")
        .map(|rest| rest.rsplit('/').next().unwrap_or(rest))
}

/// Flatten a spec schema node into the catalog's MINIMAL schema: local
/// `$ref`s are resolved inline and only `type` (nullable arrays keep
/// their non-null member), `enum`, `required`, `properties`, `items`,
/// and `additionalProperties` (schema form) survive. Bounds, defaults,
/// and prose stay in the spec/parameter descriptions — the minimal
/// schema is the shape an agent needs to build a call.
///
/// Keys are canonicalized through a [`BTreeMap`] so the output is
/// byte-stable regardless of map ordering features.
fn minimal_schema(node: &Value, spec: &Value) -> Value {
    let node = resolve_ref(node, spec);
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    match node.get("type") {
        Some(Value::String(t)) => {
            out.insert("type".to_string(), Value::String(t.clone()));
        }
        // OpenAPI 3.1 nullable form `["string", "null"]`.
        Some(Value::Array(types)) => {
            if let Some(non_null) = types.iter().find(|t| t.as_str() != Some("null")) {
                out.insert("type".to_string(), non_null.clone());
            }
        }
        _ => {}
    }
    if let Some(enum_values) = node.get("enum").filter(|e| e.is_array()) {
        out.insert("enum".to_string(), enum_values.clone());
    }
    if let Some(required) = node.get("required").filter(|r| r.is_array()) {
        out.insert("required".to_string(), required.clone());
    }
    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        let mut minimal_props: BTreeMap<String, Value> = BTreeMap::new();
        for (name, prop) in properties {
            minimal_props.insert(name.clone(), minimal_schema(prop, spec));
        }
        out.insert(
            "properties".to_string(),
            Value::Object(minimal_props.into_iter().collect()),
        );
    }
    if let Some(items) = node.get("items") {
        out.insert("items".to_string(), minimal_schema(items, spec));
    }
    if let Some(additional) = node.get("additionalProperties") {
        if additional.as_object().is_some_and(|a| !a.is_empty()) {
            out.insert(
                "additionalProperties".to_string(),
                minimal_schema(additional, spec),
            );
        }
    }
    Value::Object(out.into_iter().collect())
}

/// `listProcessInstances` → `list_process_instances`.
fn operation_id_to_snake(operation_id: &str) -> String {
    let mut out = String::with_capacity(operation_id.len() + 8);
    for ch in operation_id.chars() {
        if ch.is_ascii_uppercase() {
            out.push('_');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_convert_to_snake_case() {
        assert_eq!(
            operation_id_to_snake("listProcessInstances"),
            "list_process_instances"
        );
        assert_eq!(
            operation_id_to_snake("dryRunProcessDefinition"),
            "dry_run_process_definition"
        );
        assert_eq!(operation_id_to_snake("getHealth"), "get_health");
    }

    #[test]
    fn error_examples_cover_every_code_exactly_once() {
        let codes: Vec<&str> = ERROR_EXAMPLES.iter().map(|(c, _, _)| *c).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "duplicate example codes");
        // The canonical list comes from the routes module — the catalog
        // must carry one example per DISTINCT code, no more, no less.
        let canonical: BTreeSet<&str> = crate::routes::error_codes::ERROR_CODES
            .iter()
            .copied()
            .collect();
        let examples: BTreeSet<&str> = codes.into_iter().collect();
        assert_eq!(canonical, examples);
    }

    #[test]
    fn committed_artifact_is_embedded() {
        // Sanity: the artifact is a JSON object with the catalog header
        // (full lockstep is asserted by the consistency suite).
        let catalog: Value =
            serde_json::from_str(AGENT_TOOLS_JSON).expect("committed catalog must be JSON");
        assert_eq!(catalog["catalog_version"], CATALOG_VERSION);
        assert_eq!(catalog["generated_from"], GENERATED_FROM);
        assert_eq!(catalog["regenerate"], REGENERATE_COMMAND);
    }
}
