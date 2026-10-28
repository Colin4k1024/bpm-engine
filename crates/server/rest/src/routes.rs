//! REST API v1 routes: process-instances, tasks, external-tasks.

use axum::{
    async_trait,
    extract::{rejection::JsonRejection, Extension, FromRequest, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware as axum_mw,
    response::{IntoResponse, Response},
    routing::MethodRouter,
    Json, Router,
};
use bpm_engine_core::{
    parse_envelope, payloads, redact_history_variable_value, validate_artifact_uri,
    AesGcmEncryptor, AiTaskEnvelope, ApprovalDecision, EngineEvent, ExternalTaskState,
    HistoryRedactionPolicy, InstanceState, NodeType, ProcessDefinition, ProcessInstance,
    RetryCategory, TokenStatus, VariableEncryptor, VariableEnvelope, AI_TASK_ENVELOPE_KEY,
    ENCRYPTED_PREFIX,
};
use bpm_engine_runtime::EngineContext;
use bpm_engine_storage::{
    ApprovalDecideOutcome, ApprovalError, ApprovalStore, CompensationRecordRepo, ExternalTaskStore,
    HistoryRepo, ParallelJoinRepo, ProcessDefinitionStore, ProcessInstanceStore, TimerStore,
    TokenStore,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use super::middleware;
use super::middleware::Identity;
use super::replay::evict_expired;

fn occurred_at_now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string()
}

// --- Stable error-code contract (E2-4) --------------------------------------
//
// The canonical, single-source-of-truth list of machine-readable error
// codes the REST API can emit in `ErrorResponse.code`. `docs/openapi.yaml`
// mirrors this list in the `ErrorResponse.code` enum, and the CI
// consistency suite (`crates/server/rest/tests/openapi_contract.rs`)
// asserts set equality between the two, so a code cannot be added on one
// side only.
pub mod error_codes {
    /// Every stable error code of the REST API (E2-4).
    ///
    /// Rules:
    /// - Codes are **stable identifiers**: renaming or removing one is a
    ///   breaking API change; adding one is backward compatible.
    /// - Each code has a fixed HTTP status family (documented per code in
    ///   `docs/api-spec.md` and `docs/openapi.yaml`).
    /// - Codes that predate E2-4 (E2-2/E2-3/E3-1) keep their exact names.
    pub const ERROR_CODES: &[&str] = &[
        // Generic request/transport-level codes.
        //
        // `not_found`: 404 — unknown instance / definition / task /
        // dead-letter / replay-session id (and pre-existing 400-family
        // not-found responses keep their status, see `task_not_found`).
        "not_found",
        // `unauthorized`: 401 — missing or invalid API key.
        "unauthorized",
        // `invalid_token`: 401 — missing, malformed, or rejected JWT
        // (signature/expiry/claims) when JWT auth is configured (E5-1).
        "invalid_token",
        // `forbidden`: 403 — the verified role lacks permission for the
        // operation (RBAC), or the X-Tenant-Id header conflicts with the
        // token's tenant claim (E5-1).
        "forbidden",
        // `rate_limited`: 429 — request rate above the configured window.
        "rate_limited",
        // `invalid_request`: 400 — generic client error (malformed path
        // ids, bad query parameters, semantically invalid requests).
        "invalid_request",
        // `invalid_json`: 400/422 — request body is not valid JSON for the
        // endpoint (syntax/content-type → 400; type mismatch → 422).
        "invalid_json",
        // `internal`: 500 — unexpected server-side failure. Details are
        // logged server-side, never leaked to the client.
        "internal",
        // Deploy & definition management.
        //
        // `deploy_failed`: 500 — the definition compiled but could not be
        // persisted (distinct from parse/compile 400s).
        "deploy_failed",
        // `invalid_format`: 400 — validate/dry-run `format` is neither
        // "bpmn" nor "dsl" (E2-2).
        "invalid_format",
        // `empty_source`: 400 — validate/dry-run `source` is empty (E2-2).
        "empty_source",
        // `invalid_request_body`: 400 — validate/dry-run body is not
        // deserializable JSON (E2-2).
        "invalid_request_body",
        // Variable contract (E2-3) and encryption.
        "variable_too_large",
        "variable_invalid_envelope",
        "variable_invalid_artifact_uri",
        "sensitive_requires_encryption",
        // `encryption_key_not_configured`: 400 — `encrypted` variables
        // submitted while `BPM_VARIABLE_ENCRYPTION_KEY` is unset.
        "encryption_key_not_configured",
        // AI task envelope (E3-1).
        "task_envelope_invalid",
        "reserved_variable_key",
        "invalid_retry_category",
        // User tasks.
        //
        // `missing_required_fields`: 400 — user-task complete without the
        // form's required fields.
        "missing_required_fields",
        // External-task lease errors (store-level rejections).
        //
        // `task_not_found` / `task_not_locked` / `lock_expired`: 400 —
        // the store rejected the operation (kept at 400 for backward
        // compatibility with the pre-code contract).
        "task_not_found",
        "task_not_locked",
        "lock_expired",
        // `invariant_violation`: 400 — an engine invariant was violated
        // (e.g. lease conflict); the kind is in `invariant_violation` and
        // the `X-Invariant-Violation` header.
        "invariant_violation",
        // `lock_conflict`: 409 — lock extension refused (task unknown, not
        // locked, or owned by a different worker).
        "lock_conflict",
        // Approval & instance lifecycle (E5-2, ADR-008).
        //
        // `approval_not_found`: 404 — unknown approval record id (a record
        // of another tenant is reported the same way — no existence leak).
        "approval_not_found",
        // `approval_already_decided`: 409 — a decide on an already-decided
        // record whose decision CONFLICTS with the submitted one (a retried
        // same decision is a 200 replay).
        "approval_already_decided",
        // `instance_paused`: 409 — external-task complete/fail or user-task
        // complete on an instance whose tokens are Suspended (paused);
        // the operation is refused until resume.
        "instance_paused",
        // `instance_cancelled`: 409 — external-task or user-task operations
        // on a cancelled (Terminated) instance.
        "instance_cancelled",
        // `instance_not_pausable`: 409 — pause/resume on an instance that
        // already reached a terminal state.
        "instance_not_pausable",
    ];

    /// 404 — unknown resource id.
    pub const NOT_FOUND: &str = "not_found";
    /// 401 — missing or invalid API key.
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// 401 — missing/malformed/rejected JWT under JWT auth (E5-1).
    pub const INVALID_TOKEN: &str = "invalid_token";
    /// 403 — RBAC denial or X-Tenant-Id/claim tenant mismatch (E5-1).
    pub const FORBIDDEN: &str = "forbidden";
    /// 429 — rate limit exceeded.
    pub const RATE_LIMITED: &str = "rate_limited";
    /// 400 — generic invalid request.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// 400/422 — invalid JSON request body.
    pub const INVALID_JSON: &str = "invalid_json";
    /// 500 — internal server error.
    pub const INTERNAL: &str = "internal";
    /// 500 — definition persistence failed at deploy.
    pub const DEPLOY_FAILED: &str = "deploy_failed";
    /// 400 — external task id unknown (store rejection).
    pub const TASK_NOT_FOUND: &str = "task_not_found";
    /// 400 — external task not in the Locked state.
    pub const TASK_NOT_LOCKED: &str = "task_not_locked";
    /// 400 — external task lock expired.
    pub const LOCK_EXPIRED: &str = "lock_expired";
    /// 400 — an engine invariant was violated (kind in `invariant_violation`).
    pub const INVARIANT_VIOLATION: &str = "invariant_violation";
    /// 409 — lock extension refused.
    pub const LOCK_CONFLICT: &str = "lock_conflict";
    /// 404 — unknown approval record id (E5-2).
    pub const APPROVAL_NOT_FOUND: &str = "approval_not_found";
    /// 409 — a conflicting decide on an already-decided approval record (E5-2).
    pub const APPROVAL_ALREADY_DECIDED: &str = "approval_already_decided";
    /// 409 — task operations on a paused instance (E5-2).
    pub const INSTANCE_PAUSED: &str = "instance_paused";
    /// 409 — task operations on a cancelled instance (E5-2).
    pub const INSTANCE_CANCELLED: &str = "instance_cancelled";
    /// 409 — pause/resume on a terminal instance (E5-2).
    pub const INSTANCE_NOT_PAUSABLE: &str = "instance_not_pausable";
}

/// A request-body extractor that produces the API's typed error contract
/// instead of axum's plain-text JSON rejections (E2-4).
///
/// Wraps [`Json`]: on success the body is deserialized exactly like
/// `Json<T>`; on rejection the response is the standard
/// [`ErrorResponse`] JSON with the stable code `invalid_json` (400 for
/// syntax/content-type problems, 422 — via axum's semantics — when the
/// JSON is well-formed but does not match the target type). Use this for
/// every handler that accepts a JSON request body.
pub struct ValidatedJson<T>(pub T);

#[async_trait]
impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<ErrorResponse>);

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(ValidatedJson(value)),
            Err(rejection) => {
                let status = match &rejection {
                    JsonRejection::JsonDataError(_) => StatusCode::UNPROCESSABLE_ENTITY,
                    _ => StatusCode::BAD_REQUEST,
                };
                Err((
                    status,
                    Json(ErrorResponse::with_code(
                        rejection.body_text(),
                        error_codes::INVALID_JSON,
                    )),
                ))
            }
        }
    }
}

// --- Pagination (E2-4) ------------------------------------------------------

/// Default page size for `GET /api/v1/process-instances`.
const DEFAULT_LIST_LIMIT: u64 = 50;
/// Maximum accepted page size for the paginated list endpoints.
const MAX_LIST_LIMIT: u64 = 500;

/// A validated `limit`/`offset` query window.
#[derive(Debug, Clone, Copy)]
struct Page {
    limit: u64,
    offset: u64,
}

/// Parse `limit`/`offset` query parameters with the API's error contract.
///
/// `limit` must be an integer in `1..=MAX_LIST_LIMIT` and `offset` a
/// non-negative integer; anything else is 400 `invalid_request`.
fn parse_page_params(
    params: &HashMap<String, String>,
) -> Result<Page, (StatusCode, Json<ErrorResponse>)> {
    let limit = match params.get("limit") {
        None => DEFAULT_LIST_LIMIT,
        Some(raw) => raw.parse::<u64>().map_err(|_| {
            bad_request(format!(
                "limit must be a non-negative integer, got {:?}",
                raw
            ))
        })?,
    };
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(bad_request(format!(
            "limit must be between 1 and {}, got {}",
            MAX_LIST_LIMIT, limit
        )));
    }
    let offset = match params.get("offset") {
        None => 0,
        Some(raw) => raw.parse::<u64>().map_err(|_| {
            bad_request(format!(
                "offset must be a non-negative integer, got {:?}",
                raw
            ))
        })?,
    };
    Ok(Page { limit, offset })
}

/// Build a 400 `invalid_request` response tuple.
fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse::with_code(msg, error_codes::INVALID_REQUEST)),
    )
}

// --- Idempotency keys (E2-4) ------------------------------------------------

/// Extract and validate the optional `Idempotency-Key` header (E2-4).
///
/// The key must be 1–128 bytes of printable ASCII (`0x20..=0x7E`); an
/// empty, oversized, or non-printable key is 400 `invalid_request`
/// (rejected rather than silently ignored, so clients learn their retry
/// deduplication is not in effect). Absent header → `Ok(None)`.
fn idempotency_key_from_headers(
    headers: &HeaderMap,
) -> Result<Option<String>, (StatusCode, Json<ErrorResponse>)> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let raw = value
        .to_str()
        .map_err(|_| bad_request("Idempotency-Key header must be printable ASCII (1-128 bytes)"))?;
    let valid_len = (1..=128).contains(&raw.len());
    let printable = raw.bytes().all(|b| (0x20..=0x7E).contains(&b));
    if !valid_len || !printable {
        return Err(bad_request(
            "Idempotency-Key header must be printable ASCII (1-128 bytes)",
        ));
    }
    Ok(Some(raw.to_string()))
}

/// Maximum accepted `X-Tenant-Id` length when an `Idempotency-Key` is
/// supplied (E2-4 review fix).
///
/// The `(tenant, key)` pair co-forms the idempotency primary key, so an
/// unbounded tenant id would let a single request inflate the stored key
/// row. The bound applies ONLY at this save boundary: the tenant scope
/// keeps its unbounded, pass-through semantics on every other endpoint
/// (the tenant comes from the verified JWT claim under JWT auth, or the
/// untrusted `X-Tenant-Id` header in open/API-key modes — see
/// [`tenant_from_identity`]).
const MAX_IDEMPOTENCY_TENANT_BYTES: usize = 128;

/// Validate the tenant of an idempotent start-instance request: when an
/// `Idempotency-Key` is supplied, the co-stored tenant id (the JWT claim
/// under JWT auth, else the `X-Tenant-Id` header) must be at most
/// [`MAX_IDEMPOTENCY_TENANT_BYTES`] printable ASCII bytes, else 400
/// `invalid_request` ("tenant id too long"). An absent tenant id is fine
/// (idempotency keys work without a tenant scope).
fn tenant_id_for_idempotency(
    tenant: Option<String>,
) -> Result<Option<String>, (StatusCode, Json<ErrorResponse>)> {
    let Some(id) = tenant else {
        return Ok(None);
    };
    if id.len() > MAX_IDEMPOTENCY_TENANT_BYTES || !id.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(bad_request(format!(
            "tenant id too long: the tenant scope (X-Tenant-Id header, or the JWT tenant \
             claim) must be at most {} printable ASCII bytes when an Idempotency-Key is supplied",
            MAX_IDEMPOTENCY_TENANT_BYTES
        )));
    }
    Ok(Some(id))
}

use super::replay::ReplaySession;
use super::state::{AppState, VariableContractLimits};
use bpm_engine_adapter_memory::MemoryRepo;

// --- External task API (plan §6) ---

#[derive(Deserialize)]
pub struct FetchAndLockRequest {
    pub worker_id: String,
    pub task_types: Vec<String>,
    #[serde(default = "default_max_tasks")]
    pub max_tasks: u32,
    pub lock_duration_ms: u64,
}

fn default_max_tasks() -> u32 {
    10
}

#[derive(Serialize)]
pub struct ExternalTaskResponse {
    pub task_id: String,
    pub token_id: String,
    pub process_instance_id: String,
    pub task_type: String,
    pub variables: HashMap<String, String>,
}

#[derive(Deserialize)]
pub struct ExternalTaskCompleteRequest {
    pub worker_id: String,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// Optional AI task envelope (E3-1): validated, then serialized into
    /// the reserved `__task_envelope` variable of the merged map — the
    /// variables channel itself is unchanged. Absent ⇒ byte-identical
    /// legacy behavior.
    #[serde(default)]
    pub envelope: Option<AiTaskEnvelope>,
}

#[derive(Deserialize)]
pub struct ExternalTaskFailRequest {
    pub worker_id: String,
    pub error: String,
    pub retry_after_ms: Option<u64>,
    /// Optional retry category (E3-1 field, E3-2 behavior): `"transient"`
    /// or `"permanent"`. `permanent` skips the retry loop — the task goes
    /// straight to Failed with the DLQ + `TokenFailed` path; `transient`
    /// (or absent) is the legacy decrement-and-requeue behavior.
    #[serde(default)]
    pub category: Option<String>,
    /// Optional AI task envelope (E3-1), validated like on complete. On
    /// fail the engine has no variables channel (the store's `fail` does
    /// not take variables), so the envelope survives in the
    /// `ExternalTaskFailed` history payload only — see
    /// `docs/ai-task-envelope.md`.
    #[serde(default)]
    pub envelope: Option<AiTaskEnvelope>,
}

#[derive(Deserialize)]
pub struct ExtendLockRequest {
    pub worker_id: String,
    pub extension_ms: u64,
}

/// Build 4xx error response for external task errors.
/// Maps `ExternalTaskError` variants to appropriate HTTP status codes, safe
/// error messages, and stable codes (E2-4: every rejection is typed).
fn external_task_error_response(e: bpm_engine_storage::ExternalTaskError) -> Response {
    use bpm_engine_storage::ExternalTaskError;

    match &e {
        ExternalTaskError::InvariantViolation(inv) => {
            tracing::warn!(
                kind = %inv.kind,
                context = %inv.context,
                "invariant violation"
            );
            let kind_str = inv.kind.to_string();
            let body = ErrorResponse {
                error: e.to_string(),
                code: Some(error_codes::INVARIANT_VIOLATION.to_string()),
                invariant_violation: Some(kind_str.clone()),
            };
            let mut res = (StatusCode::BAD_REQUEST, Json(body)).into_response();
            if let Ok(hv) = HeaderValue::try_from(kind_str.as_str()) {
                res.headers_mut().insert("X-Invariant-Violation", hv);
            }
            res
        }
        ExternalTaskError::TaskNotFound { .. } => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                e.to_string(),
                error_codes::TASK_NOT_FOUND,
            )),
        )
            .into_response(),
        ExternalTaskError::TaskNotLocked { .. } => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                e.to_string(),
                error_codes::TASK_NOT_LOCKED,
            )),
        )
            .into_response(),
        ExternalTaskError::LockExpired { .. } => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                e.to_string(),
                error_codes::LOCK_EXPIRED,
            )),
        )
            .into_response(),
        ExternalTaskError::Internal(msg) => {
            tracing::error!(error = %msg, "external task internal error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::with_code(
                    "internal server error",
                    error_codes::INTERNAL,
                )),
            )
                .into_response()
        }
    }
}

/// Log error details server-side and return a generic 500 response.
fn internal_error(e: anyhow::Error) -> Response {
    tracing::error!(error = %e, "internal server error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::with_code(
            "internal server error",
            error_codes::INTERNAL,
        )),
    )
        .into_response()
}

/// Log error details server-side and return a generic 500 tuple (for non-Response handlers).
fn internal_error_tuple(e: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    tracing::error!(error = %e, "internal server error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::with_code(
            "internal server error",
            error_codes::INTERNAL,
        )),
    )
}

/// The tenant scope of a request, from the verified [`Identity`] (E5-1).
///
/// JWT-authenticated requests scope by the verified `tenant` claim (the
/// client `X-Tenant-Id` header is deprecated in that mode and a conflicting
/// header was already rejected by the middleware); open and API-key modes
/// keep the pre-E5-1 untrusted-header value. This replaces the old
/// `tenant_from_headers` for every call site.
fn tenant_from_identity(identity: &Identity) -> Option<String> {
    identity.tenant().map(String::from)
}

/// E5-1 cross-tenant gate: under a JWT-verified identity, an instance whose
/// tenant does not match the claim is reported as 404 `not_found` — the
/// SAME body an unknown id produces, so the response never leaks whether
/// the instance exists. Open and API-key modes keep the pre-E5-1 behavior
/// (no per-request tenant check on loads).
fn tenant_denied(identity: &Identity, instance: &ProcessInstance) -> bool {
    match identity {
        Identity::Principal { tenant, .. } => {
            instance.tenant_id.as_deref() != Some(tenant.as_str())
        }
        _ => false,
    }
}

/// 404 `not_found` for a tenant-gated instance access (same shape as an
/// unknown instance id).
fn tenant_gate_not_found(id: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse::not_found(format!(
            "process instance not found: {}",
            id
        ))),
    )
}

/// E5-1 cross-tenant gate for external-task operations (complete / fail /
/// extend-lock): under a verified identity the task's instance must belong
/// to the claim tenant, checked BEFORE the store mutates the task. A
/// refusal uses the SAME body an unknown task id produces (400
/// `task_not_found` — no existence leak); a task whose instance cannot be
/// loaded also fails closed (its tenancy cannot be established). Open and
/// API-key modes keep the pre-E5-1 behavior (no gate).
async fn external_task_tenant_gate(
    state: &AppState,
    identity: &Identity,
    external_task_store: &Arc<dyn ExternalTaskStore>,
    task_id: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let Identity::Principal { tenant, .. } = identity else {
        return Ok(());
    };
    let unknown_task = || {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                format!("task not found: {}", task_id),
                error_codes::TASK_NOT_FOUND,
            )),
        )
    };
    let task = external_task_store
        .get(task_id)
        .await
        .map_err(internal_error_tuple)?;
    let Some(task) = task else {
        return Err(unknown_task());
    };
    let instance = state
        .storage
        .process_store
        .load(&task.process_instance_id)
        .await
        .map_err(internal_error_tuple)?;
    match instance {
        Some(instance) if instance.tenant_id.as_deref() == Some(tenant.as_str()) => Ok(()),
        _ => Err(unknown_task()),
    }
}

fn status_str(state: InstanceState) -> &'static str {
    match state {
        InstanceState::Running => "RUNNING",
        InstanceState::Completed => "COMPLETED",
        InstanceState::Terminated => "TERMINATED",
    }
}

fn current_nodes(inst: &ProcessInstance) -> Vec<String> {
    let mut nodes: Vec<String> = inst
        .tokens
        .iter()
        .filter(|t| t.status == TokenStatus::Waiting)
        .map(|t| t.node_id.clone())
        .collect();
    nodes.sort();
    nodes.dedup();
    nodes
}

fn build_ctx(state: &AppState, tenant_id: Option<String>) -> EngineContext {
    let s = &state.storage;
    let mut builder = EngineContext::builder(
        Arc::clone(&s.process_store),
        Arc::clone(&s.token_store),
        Arc::clone(&s.def_store),
    )
    .parallel_join_repo(Arc::clone(&s.parallel_join_repo))
    .timer_store(Arc::clone(&s.timer_store))
    .compensation_repo(Arc::clone(&s.compensation_repo))
    .external_task_store(Arc::clone(&s.external_task_store))
    .history_repo(Arc::clone(&s.history_repo))
    .approval_store(Arc::clone(&s.approval_store))
    .approval_required_task_types(Arc::clone(&state.approval_required_task_types))
    .history_redaction_policy(state.history_redaction_policy);
    if let Some(id) = tenant_id {
        builder = builder.tenant_id(id);
    }
    builder.build()
}

/// Build engine context for timer event processing (no tenant).
pub fn build_ctx_for_timer(state: &AppState) -> EngineContext {
    build_ctx(state, None)
}

/// Build engine context with a specific repo (e.g. replay temporary repo).
///
/// `repo` is the ephemeral replay shadow store (always a fresh `MemoryRepo`);
/// `def_store` is the application's definition store (memory or Postgres).
fn build_ctx_with_repo(
    repo: Arc<MemoryRepo>,
    def_store: Arc<dyn ProcessDefinitionStore>,
    history_redaction_policy: HistoryRedactionPolicy,
    tenant_id: Option<String>,
) -> EngineContext {
    let mut builder = EngineContext::builder(
        repo.clone() as Arc<dyn ProcessInstanceStore>,
        repo.clone() as Arc<dyn TokenStore>,
        def_store,
    )
    .parallel_join_repo(repo.clone() as Arc<dyn ParallelJoinRepo>)
    .timer_store(repo.clone() as Arc<dyn TimerStore>)
    .compensation_repo(repo.clone() as Arc<dyn CompensationRecordRepo>)
    .external_task_store(repo.clone() as Arc<dyn ExternalTaskStore>)
    .history_repo(repo.clone() as Arc<dyn HistoryRepo>)
    // E5-2: replay runs against the shadow MemoryRepo with an EMPTY
    // approval-required set — approval decisions are route-layer history
    // events (ApprovalGranted/Rejected), not engine events, so an
    // approval-required step replays as immediate task creation in the
    // simulation (ADR-008 §8).
    .approval_store(repo as Arc<dyn ApprovalStore>)
    .history_redaction_policy(history_redaction_policy);
    if let Some(id) = tenant_id {
        builder = builder.tenant_id(id);
    }
    builder.build()
}

fn instance_id_from_event(ev: &EngineEvent) -> Option<String> {
    use bpm_engine_core::EngineEvent;
    match ev {
        EngineEvent::ProcessStarted(p) => Some(p.instance_id.clone()),
        EngineEvent::TokenArrived(p) => Some(p.instance_id.clone()),
        EngineEvent::TokenCompleted(p) => Some(p.instance_id.clone()),
        EngineEvent::UserTaskCreated(p) => Some(p.instance_id.clone()),
        EngineEvent::UserTaskCompleted(p) => Some(p.instance_id.clone()),
        EngineEvent::TimerFired(_) => None,
        EngineEvent::TimerScheduled(p) => Some(p.instance_id.clone()),
        EngineEvent::TokenFailed(p) => Some(p.instance_id.clone()),
        EngineEvent::SagaStarted(p) => Some(p.instance_id.clone()),
        EngineEvent::SagaCompleted(p) => Some(p.instance_id.clone()),
        EngineEvent::ProcessCompleted(p) => Some(p.instance_id.clone()),
        EngineEvent::CallActivityStarted(p) => Some(p.parent_instance_id.clone()),
        EngineEvent::CallActivityCompleted(p) => Some(p.parent_instance_id.clone()),
        EngineEvent::MessageSent(p) => Some(p.instance_id.clone()),
        EngineEvent::SignalSent(p) => Some(p.instance_id.clone()),
        EngineEvent::ProcessTerminated(p) => Some(p.instance_id.clone()),
        EngineEvent::ExternalTaskCompleted(p) => Some(p.instance_id.clone()),
    }
}

#[derive(Deserialize)]
pub struct StartInstanceRequest {
    pub process_def_id: String,
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// List of variable names that should be encrypted before storage.
    #[serde(default)]
    pub encrypted: Vec<String>,
    /// List of variable names declared sensitive: each must exist in the
    /// submitted variables AND be listed in `encrypted` (sensitive ⇒ must be
    /// encrypted, E2-3). Rationale: encrypted values are ciphertext at rest,
    /// so history and logs never contain the plaintext.
    #[serde(default)]
    pub sensitive: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct StartInstanceResponse {
    pub instance_id: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct InstanceStateResponse {
    pub instance_id: String,
    pub process_def_id: String,
    pub status: String,
    pub current_nodes: Vec<String>,
    pub tokens: Vec<bpm_engine_core::Token>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<HashMap<String, String>>,
}

#[derive(Serialize)]
pub struct InstanceListResponse {
    /// Page of instances visible to the tenant.
    pub instances: Vec<InstanceStateResponse>,
    /// Page metadata (E2-4, additive field: `limit`, `offset`, `returned`).
    pub pagination: PaginationView,
}

#[derive(Deserialize)]
pub struct CompleteTaskBodyRequest {
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct CompleteTaskResponse {
    pub status: String,
}

#[derive(Serialize)]
pub struct TaskListItem {
    pub task_id: String,
    pub node_id: String,
    pub instance_id: String,
    pub task_type: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
    /// Stable machine-readable error code (E2-3 variable contract errors
    /// and future typed 4xx errors); absent on legacy error responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invariant_violation: Option<String>,
}

impl ErrorResponse {
    /// Build error response carrying a stable machine-readable error code.
    pub fn with_code(msg: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            error: msg.into(),
            code: Some(code.into()),
            invariant_violation: None,
        }
    }

    /// 404-shaped error with the stable `not_found` code (E2-4).
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::with_code(msg, error_codes::NOT_FOUND)
    }

    /// 500-shaped error with the stable `internal` code; details are only
    /// logged server-side (E2-4).
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::with_code(msg, error_codes::INTERNAL)
    }
}

#[derive(Serialize)]
pub struct DeployResponse {
    pub process_definition_id: String,
}

/// Failure shape of `POST /api/v1/process-definitions/deploy`.
///
/// Variants (untagged, additive):
/// - `Parse` (400): the BPMN XML could not be parsed (`{"error": "..."}`).
/// - `Compile` (400): semantic compile errors (`{"errors": [...]}`).
/// - `Persist` (500, E2-4): the definition compiled but could NOT be
///   persisted. Previously this reused the `Parse` shape, mislabeling a
///   server-side failure as a client-side parse error. The body now matches
///   the standard [`ErrorResponse`] shape with the stable code
///   `deploy_failed`.
#[derive(Serialize)]
#[serde(untagged)]
pub enum DeployErrorResponse {
    /// BPMN XML parse failure (single message, 400).
    Parse {
        /// Parse error message.
        error: String,
    },
    /// Semantic compile errors (400).
    Compile {
        /// Compile errors with stable codes.
        errors: Vec<bpm_engine_bpmn::CompilerError>,
    },
    /// Persistence failure (500, `code: "deploy_failed"`).
    Persist {
        /// Server-side failure message (safe: no internal details).
        error: String,
        /// Stable error code, always `deploy_failed`.
        code: String,
    },
}

// --- Process definition view (Trace UI diagram) ---

#[derive(Serialize)]
pub struct NodeView {
    pub id: String,
    pub node_type: String,
}

#[derive(Serialize)]
pub struct EdgeView {
    pub source: String,
    pub target: String,
}

#[derive(Serialize)]
pub struct ProcessDefinitionView {
    pub id: String,
    pub start: String,
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
}

fn node_type_str(nt: &NodeType) -> &'static str {
    match nt {
        NodeType::Start => "Start",
        NodeType::End => "End",
        NodeType::ServiceTask(_) => "ServiceTask",
        NodeType::UserTask { .. } => "UserTask",
        NodeType::ExternalTask { .. } => "ExternalTask",
        NodeType::ExclusiveGateway => "ExclusiveGateway",
        NodeType::ParallelFork => "ParallelFork",
        NodeType::ParallelJoin { .. } => "ParallelJoin",
        NodeType::TimerIntermediateCatch { .. } => "TimerIntermediateCatch",
        NodeType::BoundaryTimer { .. } => "BoundaryTimer",
        NodeType::BoundaryError { .. } => "BoundaryError",
        NodeType::CallActivity { .. } => "CallActivity",
        NodeType::MessageIntermediateCatch { .. } => "MessageIntermediateCatch",
        NodeType::MessageIntermediateThrow { .. } => "MessageIntermediateThrow",
        NodeType::SignalIntermediateThrow { .. } => "SignalIntermediateThrow",
        NodeType::SignalIntermediateCatch { .. } => "SignalIntermediateCatch",
        NodeType::TerminateEnd => "TerminateEnd",
    }
}

fn process_definition_to_view(def: &ProcessDefinition) -> ProcessDefinitionView {
    let mut nodes = Vec::with_capacity(def.nodes.len());
    let mut edges = Vec::new();
    for (id, node) in &def.nodes {
        nodes.push(NodeView {
            id: id.to_string(),
            node_type: node_type_str(&node.node_type).to_string(),
        });
        for out in &node.outgoing_edges {
            edges.push(EdgeView {
                source: id.to_string(),
                target: out.target.to_string(),
            });
        }
    }
    ProcessDefinitionView {
        id: def.id.to_string(),
        start: def.start.to_string(),
        nodes,
        edges,
    }
}

/// Read the encryption key from the `BPM_VARIABLE_ENCRYPTION_KEY` environment variable.
/// Returns `Ok(None)` if the variable is not set.
fn get_encryption_key() -> Result<Option<String>, (StatusCode, Json<ErrorResponse>)> {
    match std::env::var("BPM_VARIABLE_ENCRYPTION_KEY") {
        Ok(key) if !key.is_empty() => Ok(Some(key)),
        _ => Ok(None),
    }
}

/// Encrypt variables that are listed in the `encrypted` set.
///
/// If the encryption key is not configured and encrypted variables are requested,
/// returns an error.
fn encrypt_variables(
    variables: &mut HashMap<String, String>,
    encrypted_names: &[String],
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if encrypted_names.is_empty() {
        return Ok(());
    }
    let key = get_encryption_key()?.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                "BPM_VARIABLE_ENCRYPTION_KEY not configured; cannot encrypt variables",
                "encryption_key_not_configured",
            )),
        )
    })?;
    let encryptor = AesGcmEncryptor::new();
    for name in encrypted_names {
        if let Some(value) = variables.get(name) {
            let encrypted = encryptor.encrypt(&key, value).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::internal(format!(
                        "encryption failed for variable '{}': {}",
                        name, e
                    ))),
                )
            })?;
            variables.insert(name.clone(), encrypted.to_storage_string());
        }
    }
    Ok(())
}

/// Decrypt all `__encrypted:` variables in a process instance.
///
/// If the encryption key is not configured, encrypted variables are left as-is.
fn decrypt_instance_variables(
    variables: &mut HashMap<String, String>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let Ok(Some(key)) = get_encryption_key() else {
        return Ok(());
    };
    let encryptor = AesGcmEncryptor::new();
    for value in variables.values_mut() {
        if value.starts_with(ENCRYPTED_PREFIX) {
            let var_val = bpm_engine_core::VariableValue::from_storage_string(value);
            match encryptor.decrypt(&key, &var_val) {
                Ok(plaintext) => *value = plaintext,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to decrypt variable, leaving encrypted");
                }
            }
        }
    }
    Ok(())
}

// --- AI task envelope boundary enforcement (E3-1) ---------------------------

/// Reject a user-supplied variable under the reserved [`AI_TASK_ENVELOPE_KEY`]
/// (4xx `reserved_variable_key`): the engine owns the envelope slot, so
/// envelopes may only arrive via the typed `envelope` request field.
///
/// Enforced STRICTLY at the boundaries that have no envelope semantics of
/// their own — start-instance and user-task complete. External-task
/// complete additionally accepts the worker-sdk wire format (a serialized
/// envelope value at the reserved key) and rejects junk/collisions with
/// the same code; see [`external_task_complete`].
fn enforce_reserved_variable_keys(
    variables: &HashMap<String, String>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if variables.contains_key(AI_TASK_ENVELOPE_KEY) {
        warn!(
            variable = AI_TASK_ENVELOPE_KEY,
            "rejecting user-supplied reserved variable key"
        );
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                format!(
                    "variable '{}' is reserved by the engine; submit the AI task envelope \
                     via the 'envelope' request field instead",
                    AI_TASK_ENVELOPE_KEY
                ),
                "reserved_variable_key",
            )),
        ));
    }
    Ok(())
}

/// Validate an AI task envelope and merge its serialized form into the
/// reserved [`AI_TASK_ENVELOPE_KEY`] of the variable map (E3-1).
///
/// 4xx `task_envelope_invalid` (with the envelope error's stable sub-code in
/// the message) on validation failure. The caller subsequently runs the
/// ordinary E2-3 boundary contract on the MERGED map, so the size cap and
/// envelope/artifact checks cover the envelope value too.
fn merge_task_envelope(
    envelope: &AiTaskEnvelope,
    variables: &mut HashMap<String, String>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if let Err(e) = envelope.validate() {
        warn!(code = e.code(), "rejecting invalid AI task envelope");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_code(
                format!("{}: {}", e.code(), e),
                "task_envelope_invalid",
            )),
        ));
    }
    variables.insert(
        AI_TASK_ENVELOPE_KEY.to_string(),
        envelope.to_envelope_value(),
    );
    Ok(())
}

// --- Variable value contract enforcement (E2-3) -----------------------------

/// Hint returned with `variable_too_large` errors.
const LARGE_PAYLOAD_HINT: &str =
    "store large payloads externally and reference them via an artifact envelope";

/// Enforce the variable value contract at a REST write boundary:
/// every variable value must be a valid envelope (when it carries the
/// envelope prefix), pass artifact URI validation (when it is an artifact
/// envelope), and be within the `variable_max_bytes` cap (measured on the
/// final stored string, envelope included).
///
/// Plain legacy strings and `__encrypted:` values are only size-checked:
/// their semantics are unchanged.
fn enforce_variable_contract(
    variables: &HashMap<String, String>,
    limits: &VariableContractLimits,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for (name, value) in variables {
        // Size limit FIRST: an oversized value is rejected before any
        // envelope deserialization, so the cap also bounds parse cost.
        if value.len() > limits.variable_max_bytes {
            warn!(
                variable = %name,
                size = value.len(),
                limit = limits.variable_max_bytes,
                "rejecting oversized variable"
            );
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(
                    format!(
                        "variable '{}' is {} bytes, exceeding the {} byte limit; {}",
                        name,
                        value.len(),
                        limits.variable_max_bytes,
                        LARGE_PAYLOAD_HINT
                    ),
                    "variable_too_large",
                )),
            ));
        }
        if let Some(parsed) = parse_envelope(value) {
            match parsed {
                Ok(envelope) => {
                    if let VariableEnvelope::Artifact {
                        uri, size, sha256, ..
                    } = &envelope
                    {
                        if let Err(e) = validate_artifact_uri(uri, *size, sha256.as_deref()) {
                            warn!(
                                variable = %name,
                                code = e.code(),
                                "rejecting variable with invalid artifact URI"
                            );
                            return Err((
                                StatusCode::BAD_REQUEST,
                                Json(ErrorResponse::with_code(
                                    format!("variable '{}': {}", name, e),
                                    e.code(),
                                )),
                            ));
                        }
                    }
                }
                Err(e) => {
                    // The boundary code is stable: any parse failure
                    // (malformed body OR unsupported version) is reported as
                    // `variable_invalid_envelope`; the detailed reason (with
                    // the version, when relevant) stays in the message.
                    warn!(
                        variable = %name,
                        code = e.code(),
                        "rejecting variable with invalid envelope"
                    );
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse::with_code(
                            format!("variable '{}': {}", name, e),
                            "variable_invalid_envelope",
                        )),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Size-only cap check for values produced by the server itself (encryption
/// inflates the stored string, so the final stored value is re-checked).
fn enforce_variable_size_limit(
    variables: &HashMap<String, String>,
    limits: &VariableContractLimits,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for (name, value) in variables {
        if value.len() > limits.variable_max_bytes {
            warn!(
                variable = %name,
                size = value.len(),
                limit = limits.variable_max_bytes,
                "rejecting oversized variable after encryption"
            );
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(
                    format!(
                        "variable '{}' is {} bytes after encryption, exceeding the {} byte limit; {}",
                        name, value.len(), limits.variable_max_bytes, LARGE_PAYLOAD_HINT
                    ),
                    "variable_too_large",
                )),
            ));
        }
    }
    Ok(())
}

/// Validate the `sensitive` list of a start-instance request: every
/// sensitive name must exist in the submitted variables and be listed in
/// `encrypted` (sensitive ⇒ must be encrypted).
fn validate_sensitive_names(
    sensitive: &Option<Vec<String>>,
    variables: &HashMap<String, String>,
    encrypted: &[String],
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let Some(sensitive) = sensitive.as_ref() else {
        return Ok(());
    };
    for name in sensitive {
        if !variables.contains_key(name) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(
                    format!(
                        "variable '{}' is marked sensitive but not present in the submitted variables",
                        name
                    ),
                    "sensitive_requires_encryption",
                )),
            ));
        }
        if !encrypted.iter().any(|e| e == name) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(
                    format!(
                        "variable '{}' is marked sensitive but not listed in 'encrypted'; \
                         sensitive variables must be encrypted \
                         (BPM_VARIABLE_ENCRYPTION_KEY must be configured)",
                        name
                    ),
                    "sensitive_requires_encryption",
                )),
            ));
        }
    }
    Ok(())
}

/// POST /api/v1/process-instances — start a process instance.
///
/// # Idempotency (E2-4)
///
/// When the request carries an `Idempotency-Key` header (1–128 printable
/// ASCII bytes):
///
/// - a key that has already been recorded for the same tenant scope
///   returns **200** (not 201) with the SAME `{instance_id, status}`
///   response shape for the originally created instance — an idempotent
///   replay, no second instance is created;
/// - an unknown key creates the instance (as today, 201) and records the
///   key → instance_id mapping afterwards;
/// - the mapping is tenant-scoped (`X-Tenant-Id`), so the same key under
///   different tenants addresses different instances.
///
/// Atomicity boundary (honest, single-service): the check-then-create
/// sequence is not cross-process atomic. The engine runs as one service
/// per database (like the migration runner), so concurrent identical
/// requests can still race; a key-recording failure AFTER the instance was
/// created is reported as 500 `internal` with an explicit message — the
/// retry may then create a second instance. Cross-process uniqueness
/// would require a unique-constraint insert path, out of scope for v1.
pub async fn start_instance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Extension(identity): Extension<Identity>,
    ValidatedJson(body): ValidatedJson<StartInstanceRequest>,
) -> Result<(StatusCode, Json<StartInstanceResponse>), (StatusCode, Json<ErrorResponse>)> {
    let tenant_id = tenant_from_identity(&identity);
    let idempotency_key = idempotency_key_from_headers(&headers)?;
    // E2-4 review fix: bound the tenant id at the idempotency SAVE
    // boundary — only when a key is supplied (other endpoints, and
    // key-less starts, keep unbounded tenant semantics).
    let tenant_id = if idempotency_key.is_some() {
        tenant_id_for_idempotency(tenant_id)?
    } else {
        tenant_id
    };

    // Idempotent replay: a known key returns the recorded instance.
    if let Some(key) = &idempotency_key {
        match state
            .storage
            .process_store
            .find_idempotency_key(key, tenant_id.as_deref())
            .await
        {
            Ok(Some(instance_id)) => {
                let inst = state
                    .storage
                    .process_store
                    .load(&instance_id)
                    .await
                    .map_err(internal_error_tuple)?;
                let Some(inst) = inst else {
                    warn!(
                        idempotency_key = %key,
                        instance_id = %instance_id,
                        "idempotency key points at a missing instance"
                    );
                    return Err(internal_error_tuple(anyhow::anyhow!(
                        "recorded idempotency key points at an instance that no longer exists"
                    )));
                };
                info!(
                    idempotency_key = %key,
                    instance_id = %instance_id,
                    "idempotent start replayed"
                );
                return Ok((
                    StatusCode::OK,
                    Json(StartInstanceResponse {
                        instance_id,
                        status: status_str(inst.state).to_string(),
                    }),
                ));
            }
            Ok(None) => {} // unknown key → create below
            Err(e) => return Err(internal_error_tuple(e)),
        }
    }

    let mut ctx = build_ctx(state.as_ref(), tenant_id.clone());
    let instance_id = uuid::Uuid::new_v4().to_string();
    let mut variables = body.variables;
    // E2-3 boundary enforcement, in order:
    // 1. sensitive ⇒ must be encrypted (checked against the submitted map)
    // 2. reserved variable keys: `__task_envelope` is engine-owned (E3-1)
    // 3. variable value contract: valid envelopes, valid artifact URIs,
    //    size cap on the submitted value
    // 4. encryption, then a final size check on the stored string (the
    //    ciphertext may exceed the cap even when the plaintext did not)
    validate_sensitive_names(&body.sensitive, &variables, &body.encrypted)?;
    enforce_reserved_variable_keys(&variables)?;
    enforce_variable_contract(&variables, &state.variable_limits)?;
    encrypt_variables(&mut variables, &body.encrypted)?;
    enforce_variable_size_limit(&variables, &state.variable_limits)?;
    let initial_variables = if variables.is_empty() {
        None
    } else {
        Some(variables)
    };
    let ev = EngineEvent::ProcessStarted(payloads::ProcessStarted {
        process_id: body.process_def_id.clone(),
        instance_id: instance_id.clone(),
        initial_variables,
    });
    state.engine.run_async(ev, &mut ctx).await;
    // E5-3 boundary event: the engine's `ProcessStarted` has no human
    // actor (core events stay actorless by design); the route appends the
    // attribution the audit trail needs — WHO started the instance, and
    // against which definition version (the id IS `key:version`). Not a
    // replayable event type, so replay is unaffected.
    {
        let payload = serde_json::json!({
            "instance_id": instance_id.as_str(),
            "process_def_id": body.process_def_id,
            "actor": identity.actor(),
        });
        let _ = HistoryRepo::append(
            state.storage.history_repo.as_ref(),
            &instance_id,
            "InstanceStarted",
            &payload,
            &occurred_at_now(),
        )
        .await;
    }
    // Record the idempotency key AFTER creation: on failure the instance
    // exists but the mapping is missing, so the response is a 500 whose
    // message names the situation (see the handler docs).
    if let Some(key) = &idempotency_key {
        if let Err(e) = state
            .storage
            .process_store
            .save_idempotency_key(key, tenant_id.as_deref(), &instance_id)
            .await
        {
            tracing::error!(error = %e, "failed to record idempotency key");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal(
                    "instance was started but its idempotency key could not be recorded; \
                     retrying with the same key may create a second instance",
                )),
            ));
        }
    }
    info!(
        instance_id = %instance_id,
        definition_id = %body.process_def_id,
        actor = identity.actor(),
        idempotency_key = idempotency_key.as_deref(),
        "process instance started"
    );
    Ok((
        StatusCode::CREATED,
        Json(StartInstanceResponse {
            instance_id,
            status: "RUNNING".to_string(),
        }),
    ))
}

/// Pagination metadata returned by `GET /api/v1/process-instances` (E2-4,
/// additive response field).
#[derive(Serialize)]
pub struct PaginationView {
    /// Effective page size (defaults to 50, capped at 500).
    pub limit: u64,
    /// Zero-based index of the first returned instance.
    pub offset: u64,
    /// Number of instances in this response (`0` when offset is beyond the
    /// end).
    pub returned: u64,
}

/// GET /api/v1/process-instances — list process instances visible to the
/// tenant, one page at a time (E2-4).
///
/// Query parameters: `limit` (default 50, max 500) and `offset` (default
/// 0), validated as 400 `invalid_request` when malformed or out of range.
/// Instances are ordered by instance id (the store's deterministic
/// `list_page` order). An offset beyond the end returns an empty page.
/// The response gains the additive `pagination` object; the `instances`
/// array keeps its existing shape.
pub async fn list_process_instances(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<InstanceListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let page = parse_page_params(&params)?;
    let tenant_id = tenant_from_identity(&identity);
    // Store-level pagination (the hot path): the window is pushed into the
    // store (SQL LIMIT/OFFSET in postgres mode).
    let instance_ids = state
        .storage
        .process_store
        .list_page(tenant_id.as_deref(), page.offset, page.limit)
        .await
        .map_err(internal_error_tuple)?;
    let mut responses = Vec::with_capacity(instance_ids.len());
    for id in instance_ids {
        // A load failure is a 500 (E2-4 review fix: load errors are never
        // silently skipped); an instance that vanished between listing and
        // loading (Ok(None), e.g. deleted) is skipped as before.
        let inst = state
            .storage
            .process_store
            .load(&id)
            .await
            .map_err(internal_error_tuple)?;
        let Some(instance) = inst else {
            continue;
        };
        let mut variables = instance.variables.clone();
        let _ = decrypt_instance_variables(&mut variables);
        let current_nodes = current_nodes(&instance);
        responses.push(InstanceStateResponse {
            instance_id: instance.id,
            process_def_id: instance.process_def_id,
            status: status_str(instance.state).to_string(),
            current_nodes,
            tokens: instance.tokens,
            variables: Some(variables),
        });
    }
    Ok(Json(InstanceListResponse {
        pagination: PaginationView {
            limit: page.limit,
            offset: page.offset,
            returned: responses.len() as u64,
        },
        instances: responses,
    }))
}

/// GET /api/v1/process-instances/:id
pub async fn get_instance(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<InstanceStateResponse>, (StatusCode, Json<ErrorResponse>)> {
    let tenant_id = tenant_from_identity(&identity);
    let _ctx = build_ctx(state.as_ref(), tenant_id);
    let process_store = Arc::clone(&state.storage.process_store);
    // E2-4 review fix: a storage ERROR is 500 `internal`, only a missing
    // instance is 404 `not_found` (previously both collapsed into 404).
    let inst = process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?;
    match inst {
        Some(inst) if tenant_denied(&identity, &inst) => Err(tenant_gate_not_found(&id)),
        Some(inst) => {
            let mut vars = inst.variables.clone();
            let _ = decrypt_instance_variables(&mut vars);
            Ok(Json(InstanceStateResponse {
                instance_id: inst.id.clone(),
                process_def_id: inst.process_def_id.clone(),
                status: status_str(inst.state).to_string(),
                current_nodes: current_nodes(&inst),
                tokens: inst.tokens.clone(),
                variables: Some(vars),
            }))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!(
                "process instance not found: {}",
                id
            ))),
        )),
    }
}

// --- Trace API (aggregated view) ---

#[derive(Serialize)]
pub struct TraceEventView {
    pub event_type: String,
    pub occurred_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct TokenTimelineView {
    pub token_id: String,
    pub node_id: String,
    pub status: String,
    pub events: Vec<TraceEventView>,
}

#[derive(Serialize)]
pub struct ExternalTaskHistoryEntryView {
    pub task_id: String,
    pub token_id: String,
    pub process_instance_id: String,
    pub events: Vec<TraceEventView>,
}

#[derive(Serialize)]
pub struct TraceResponse {
    pub instance: InstanceStateResponse,
    pub token_timelines: Vec<TokenTimelineView>,
    pub external_task_history: Vec<ExternalTaskHistoryEntryView>,
}

// --- History API (auditable execution timeline) ---

#[derive(Serialize)]
pub struct HistoryEventView {
    pub sequence: u64,
    pub id: String,
    pub event_type: String,
    pub category: String,
    pub occurred_at: String,
    pub payload: serde_json::Value,
}

#[derive(Serialize)]
pub struct HistoryResponse {
    pub instance_id: String,
    pub events: Vec<HistoryEventView>,
}

/// Map event_type to human-friendly category: instance | token | external
/// | approval.
fn event_type_to_category(event_type: &str) -> &'static str {
    match event_type {
        "ProcessStarted" | "ProcessCompleted" | "ProcessTerminated" => "instance",
        // E5-3 route-layer boundary event: the operator who STARTED the
        // instance (the engine's ProcessStarted has no human actor).
        "InstanceStarted" => "instance",
        // E5-2 route-layer instance lifecycle events (ADR-008 §8).
        "InstancePaused" | "InstanceResumed" | "InstanceCancelled" => "instance",
        "TokenArrived" | "TokenCompleted" | "TokenFailed" | "UserTaskCreated"
        | "UserTaskCompleted" | "TimerScheduled" | "TimerFired" | "SagaStarted"
        | "SagaCompleted" => "token",
        // E5-3 route-layer boundary event: the operator/worker who
        // completed a task (user or external).
        "TaskCompleted" => "token",
        "ExternalTaskLocked" | "ExternalTaskCompleted" | "ExternalTaskFailed" => "external",
        // E5-3 route-layer boundary events: the operator who requeued /
        // deleted a dead-letter entry (the DLQ is external-task family).
        "DeadLetterRequeued" | "DeadLetterDeleted" => "external",
        // E5-2 approval events: ApprovalCreated is written by the engine
        // gate, Granted/Rejected by the decide endpoint (with the actor).
        "ApprovalCreated" | "ApprovalGranted" | "ApprovalRejected" => "approval",
        _ => "token",
    }
}

fn token_status_str(status: &bpm_engine_core::TokenStatus) -> &'static str {
    use bpm_engine_core::TokenStatus;
    match status {
        TokenStatus::Created => "CREATED",
        TokenStatus::Ready => "READY",
        TokenStatus::Executing => "EXECUTING",
        TokenStatus::Waiting => "WAITING",
        TokenStatus::Suspended => "SUSPENDED",
        TokenStatus::Completed => "COMPLETED",
        TokenStatus::Terminated => "TERMINATED",
    }
}

/// GET /api/v1/process-instances/:id/trace — aggregated execution trace (instance + token timelines + external task history).
pub async fn get_instance_trace(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<TraceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let process_store = Arc::clone(&state.storage.process_store);
    // E2-4 review fix: storage ERROR → 500 internal; missing instance →
    // 404 not_found. E5-1: an instance of ANOTHER tenant is reported
    // like a missing one (no existence leak).
    let inst = process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| tenant_gate_not_found(&id))?;
    if tenant_denied(&identity, &inst) {
        return Err(tenant_gate_not_found(&id));
    }
    let mut vars = inst.variables.clone();
    let _ = decrypt_instance_variables(&mut vars);
    let instance_response = InstanceStateResponse {
        instance_id: inst.id.clone(),
        process_def_id: inst.process_def_id.clone(),
        status: status_str(inst.state).to_string(),
        current_nodes: current_nodes(&inst),
        tokens: inst.tokens.clone(),
        variables: Some(vars),
    };
    let events =
        HistoryRepo::list_by_instance(state.storage.history_repo.as_ref(), &id, None, None)
            .await
            .map_err(internal_error_tuple)?;
    let token_ids_in_instance: std::collections::HashMap<
        String,
        (String, bpm_engine_core::TokenStatus),
    > = inst
        .tokens
        .iter()
        .map(|t| (t.id.clone(), (t.node_id.clone(), t.status)))
        .collect();
    let mut by_token: std::collections::HashMap<String, Vec<TraceEventView>> =
        std::collections::HashMap::new();
    for ev in &events {
        let token_id = ev
            .payload
            .get("token_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let Some(token_id) = token_id else {
            continue;
        };
        if token_id.is_empty() {
            continue;
        }
        let entry = by_token.entry(token_id).or_default();
        entry.push(TraceEventView {
            event_type: ev.event_type.clone(),
            occurred_at: ev.occurred_at.clone(),
            payload: Some(ev.payload.clone()),
        });
    }
    let mut token_timelines: Vec<TokenTimelineView> = by_token
        .into_iter()
        .map(|(token_id, evs)| {
            let (node_id, status) = token_ids_in_instance
                .get(&token_id)
                .map(|(n, s)| (n.clone(), token_status_str(s).to_string()))
                .unwrap_or_else(|| {
                    let node_id = evs
                        .last()
                        .and_then(|e| e.payload.as_ref())
                        .and_then(|p| p.get("node_id").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let status = evs
                        .last()
                        .map(|e| e.event_type.as_str())
                        .map(|t| match t {
                            "TokenCompleted" => "COMPLETED",
                            "TokenFailed" => "TERMINATED",
                            _ => "UNKNOWN",
                        })
                        .unwrap_or("UNKNOWN")
                        .to_string();
                    (node_id, status)
                });
            TokenTimelineView {
                token_id,
                node_id,
                status,
                events: evs,
            }
        })
        .collect();
    token_timelines.sort_by(|a, b| {
        a.events
            .first()
            .zip(b.events.first())
            .map(|(ea, eb)| ea.occurred_at.cmp(&eb.occurred_at))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let ext_events: Vec<&bpm_engine_storage::HistoryEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e.event_type.as_str(),
                "ExternalTaskLocked" | "ExternalTaskFailed" | "ExternalTaskCompleted"
            )
        })
        .collect();
    let mut by_task: std::collections::HashMap<String, Vec<TraceEventView>> =
        std::collections::HashMap::new();
    for ev in ext_events {
        let task_id = ev
            .payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| ev.id.clone());
        let entry = by_task.entry(task_id).or_default();
        entry.push(TraceEventView {
            event_type: ev.event_type.clone(),
            occurred_at: ev.occurred_at.clone(),
            payload: Some(ev.payload.clone()),
        });
    }
    let external_task_history: Vec<ExternalTaskHistoryEntryView> = by_task
        .into_iter()
        .map(|(task_id, evs)| {
            let first = evs.first().and_then(|e| e.payload.as_ref());
            let token_id = first
                .and_then(|p| p.get("token_id").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let process_instance_id = first
                .and_then(|p| p.get("process_instance_id").and_then(|v| v.as_str()))
                .unwrap_or(id.as_str())
                .to_string();
            ExternalTaskHistoryEntryView {
                task_id,
                token_id,
                process_instance_id,
                events: evs,
            }
        })
        .collect();
    Ok(Json(TraceResponse {
        instance: instance_response,
        token_timelines,
        external_task_history,
    }))
}

/// GET /api/v1/process-definitions/:id — process definition view for Trace UI diagram.
pub async fn get_process_definition(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ProcessDefinitionView>, (StatusCode, Json<ErrorResponse>)> {
    // E2-4 review fix: storage ERROR → 500 internal; only a missing
    // definition is 404 not_found.
    let def = state
        .storage
        .def_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "process definition not found: {}",
                    id
                ))),
            )
        })?;
    Ok(Json(process_definition_to_view(&def)))
}

/// GET /api/v1/process-instances/:id/history — execution history for Trace UI and auditing.
/// Returns events in causal order with sequence and category (instance | token | external).
///
/// Query parameters: `token_id`, `event_type` (filters, as before) and the
/// optional `limit` (E2-4): a positive integer ≤ 500 that truncates the
/// returned event list after sorting; absent ⇒ the full history (the
/// pre-existing behavior). Malformed or out-of-range values are 400
/// `invalid_request`.
pub async fn get_instance_history(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<HistoryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let token_id = params.get("token_id").map(String::as_str);
    let event_type = params.get("event_type").map(String::as_str);
    let limit = match params.get("limit") {
        None => None,
        Some(raw) => {
            let parsed = raw.parse::<u64>().map_err(|_| {
                bad_request(format!("limit must be a positive integer, got {:?}", raw))
            })?;
            if !(1..=MAX_LIST_LIMIT).contains(&parsed) {
                return Err(bad_request(format!(
                    "limit must be between 1 and {}, got {}",
                    MAX_LIST_LIMIT, parsed
                )));
            }
            Some(parsed)
        }
    };
    // E5-1: under a verified identity, an instance of another tenant is
    // reported like an unknown one BEFORE any history rows are returned.
    if let Identity::Principal { .. } = &identity {
        let inst = state
            .storage
            .process_store
            .load(&id)
            .await
            .map_err(internal_error_tuple)?;
        match inst {
            Some(inst) if tenant_denied(&identity, &inst) => {
                return Err(tenant_gate_not_found(&id));
            }
            // A missing instance keeps the pre-E5-1 behavior: an empty
            // event list, not a 404.
            _ => {}
        }
    }
    let mut events = HistoryRepo::list_by_instance(
        state.storage.history_repo.as_ref(),
        &id,
        token_id,
        event_type,
    )
    .await
    .map_err(internal_error_tuple)?;
    events.sort_by(|a, b| {
        a.occurred_at
            .cmp(&b.occurred_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    // Route-level window over the sorted event list (documented; the store
    // has no paged history API).
    if let Some(limit) = limit {
        events.truncate(limit as usize);
    }
    let events: Vec<HistoryEventView> = events
        .into_iter()
        .enumerate()
        .map(|(seq, ev)| HistoryEventView {
            sequence: seq as u64,
            id: ev.id,
            event_type: ev.event_type.clone(),
            category: event_type_to_category(&ev.event_type).to_string(),
            occurred_at: ev.occurred_at,
            payload: ev.payload,
        })
        .collect();
    Ok(Json(HistoryResponse {
        instance_id: id,
        events,
    }))
}

// --- Audit API (E5-3, issue #25) ----------------------------------------------

/// One audited history event with the ACTOR attribution joined on (E5-3).
///
/// The actor is read from the event payload's `actor` field — carried by
/// the ROUTE-LAYER boundary events (`InstanceStarted`, `TaskCompleted`,
/// `ExternalTaskLocked`, `ExternalTaskFailed`, `InstancePaused`/
/// `InstanceResumed`/`InstanceCancelled`, `ApprovalGranted`/
/// `ApprovalRejected`, `DeadLetterRequeued`/`DeadLetterDeleted`).
/// Engine-internal transitions (`TokenArrived`, `TokenCompleted`, …)
/// have no human actor BY DESIGN (see docs/audit-and-retention.md) and
/// serialize without the field.
#[derive(Debug, Serialize)]
pub struct AuditEventView {
    pub sequence: u64,
    pub id: String,
    pub event_type: String,
    pub category: String,
    pub occurred_at: String,
    /// The verified principal who performed the boundary operation, when
    /// the event carries one (`"api-key"` / `"anonymous"` for the
    /// non-JWT modes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The history payload VERBATIM — already redacted by the E2-3 policy
    /// (oversized → `__truncated:` markers) and never decrypted: `__encrypted:`
    /// values appear as ciphertext.
    pub payload: serde_json::Value,
}

/// One approval decision surfaced by the audit endpoint (E5-3): the
/// decision, who made it, and the recorded note.
#[derive(Debug, Serialize)]
pub struct AuditDecisionView {
    pub approval_id: String,
    /// `approved` | `rejected`.
    pub decision: String,
    pub node_id: String,
    /// The verified principal who decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The operator note recorded with the decision, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub occurred_at: String,
}

/// One artifact reference seen in the instance's data (E5-3): parsed
/// read-only from a `__var:1:` artifact envelope in a history payload or
/// the instance's variables — the engine never dereferences the URI.
///
/// Envelope values redacted by the E2-3 history policy (`__truncated:`)
/// are no longer parseable and therefore do not appear — the audit shows
/// what is recoverable, not a guess.
#[derive(Debug, Serialize)]
pub struct AuditArtifactView {
    /// The variable name the artifact envelope was stored under.
    pub variable: String,
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Where the reference was seen: `history` (an event payload) or
    /// `instance` (the current variables).
    pub source: String,
}

/// Definition-version context of the audited instance (E5-3). The
/// instance's `process_def_id` IS the versioned id (`key:version`), so
/// the version is inherent; `status` is the definition store's CURRENT
/// metadata for that version record.
#[derive(Debug, Serialize)]
pub struct AuditDefinitionView {
    /// The versioned definition id the instance executes (e.g.
    /// `order-flow:3`).
    pub id: String,
    /// Definition key (e.g. `order-flow`).
    pub key: String,
    /// Version number parsed from the id (`1` when the id carries none).
    pub version: u32,
    /// Current record status (`active` | `deprecated`); absent when the
    /// definition record is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Instance context of the audit response (E5-3).
#[derive(Debug, Serialize)]
pub struct AuditInstanceView {
    /// `RUNNING` | `COMPLETED` | `TERMINATED`.
    pub state: String,
    /// The instance's tenant scope, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

/// Response of `GET /api/v1/process-instances/:id/audit` (E5-3): the
/// single place that answers "who did what, against which definition
/// version, with which decisions and artifact references".
#[derive(Debug, Serialize)]
pub struct AuditResponse {
    pub instance_id: String,
    pub instance: AuditInstanceView,
    pub definition: AuditDefinitionView,
    /// The full event timeline with actor attribution on boundary events.
    pub events: Vec<AuditEventView>,
    /// Approval decisions (approved/rejected) with actor and note.
    pub decisions: Vec<AuditDecisionView>,
    /// Artifact references parsed from `__var:1:` envelopes.
    pub artifacts: Vec<AuditArtifactView>,
}

/// Parse `(key, version)` out of a versioned definition id, using the
/// same rule as the definition stores (`order-flow:3` →
/// `("order-flow", 3)`; a colonless id → version 1).
fn parse_definition_id_version(id: &str) -> (String, u32) {
    if let Some((key, ver_str)) = id.rsplit_once(':') {
        if let Ok(v) = ver_str.parse::<u32>() {
            return (key.to_string(), v);
        }
    }
    (id.to_string(), 1)
}

/// Build the definition-version context of the audit response (E5-3).
///
/// `list_versions` failures PROPAGATE (a storage error is a 500, like
/// every other store error); a definition record that is merely ABSENT
/// degrades to the id-inherent metadata with no `status` (the instance
/// may outlive its definition).
async fn audit_definition_view(
    state: &AppState,
    process_def_id: &str,
) -> anyhow::Result<AuditDefinitionView> {
    let (key, version) = parse_definition_id_version(process_def_id);
    let versions = state.storage.def_store.list_versions(&key).await?;
    let status = versions
        .iter()
        .find(|r| r.id == process_def_id)
        .map(|r| r.status.to_string());
    Ok(AuditDefinitionView {
        id: process_def_id.to_string(),
        key,
        version,
        status,
    })
}

/// Extract the actor of a history event payload, when it carries one.
fn audit_event_actor(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("actor")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Collect the artifact references of one variable map into `out` (E5-3):
/// every value that parses as a v1 ARTIFACT envelope contributes its
/// URI + metadata. Read-only extraction — nothing is dereferenced or
/// decrypted, and redacted values simply do not parse.
fn collect_artifact_refs(
    variables: &serde_json::Map<String, serde_json::Value>,
    source: &str,
    out: &mut Vec<AuditArtifactView>,
    seen: &mut std::collections::HashSet<(String, String)>,
) {
    for (name, value) in variables {
        let Some(stored) = value.as_str() else {
            continue;
        };
        if let Some(Ok(VariableEnvelope::Artifact {
            uri,
            size,
            sha256,
            media_type,
        })) = parse_envelope(stored)
        {
            if seen.insert((uri.clone(), name.clone())) {
                out.push(AuditArtifactView {
                    variable: name.clone(),
                    uri,
                    sha256,
                    size,
                    media_type,
                    source: source.to_string(),
                });
            }
        }
    }
}

/// GET /api/v1/process-instances/:id/audit — the audit view of one
/// process instance (E5-3, issue #25): the actor-attributed event
/// timeline joined with definition-version context, the approval
/// decisions, and the artifact references parsed from envelope
/// variables.
///
/// Read-only aggregation over the instance, definition, and history
/// stores — no new stores, no new writes. Secrecy by construction: the
/// endpoint NEVER decrypts (`__encrypted:` values appear as ciphertext,
/// exactly as history stored them) and history was already redacted by
/// the E2-3 policy at write time (oversized values are `__truncated:`
/// markers). RBAC: a read — every authenticated role may call it;
/// cross-tenant access is rejected as 404 like every instance-scoped
/// read. Errors: 404 `not_found` for an unknown instance, 500 `internal`
/// on storage failures.
pub async fn get_instance_audit(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<AuditResponse>, (StatusCode, Json<ErrorResponse>)> {
    // E5-1: an instance of another tenant is reported like a missing one
    // (no existence leak) — the same gate shape as every instance read.
    let inst = state
        .storage
        .process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| tenant_gate_not_found(&id))?;
    if tenant_denied(&identity, &inst) {
        return Err(tenant_gate_not_found(&id));
    }

    let definition = audit_definition_view(state.as_ref(), &inst.process_def_id)
        .await
        .map_err(internal_error_tuple)?;

    let mut events =
        HistoryRepo::list_by_instance(state.storage.history_repo.as_ref(), &id, None, None)
            .await
            .map_err(internal_error_tuple)?;
    // Deterministic causal order across stores (same key the /steps
    // endpoint uses): (occurred_at, phase_rank, listing order).
    crate::step_trace::sort_history_causally(&mut events);

    // Actor-attributed event views + approval decisions, in one pass.
    let mut views = Vec::with_capacity(events.len());
    let mut decisions = Vec::new();
    for (seq, ev) in events.iter().enumerate() {
        let actor = audit_event_actor(&ev.payload);
        match ev.event_type.as_str() {
            "ApprovalGranted" | "ApprovalRejected" => decisions.push(AuditDecisionView {
                approval_id: ev
                    .payload
                    .get("approval_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                decision: if ev.event_type == "ApprovalGranted" {
                    "approved"
                } else {
                    "rejected"
                }
                .to_string(),
                node_id: ev
                    .payload
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                actor: actor.clone(),
                note: ev
                    .payload
                    .get("note")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                occurred_at: ev.occurred_at.clone(),
            }),
            _ => {}
        }
        views.push(AuditEventView {
            sequence: seq as u64,
            id: ev.id.clone(),
            event_type: ev.event_type.clone(),
            category: event_type_to_category(&ev.event_type).to_string(),
            occurred_at: ev.occurred_at.clone(),
            actor,
            payload: ev.payload.clone(),
        });
    }

    // Artifact references: history payloads first (chronological), then
    // the instance's current variables (deduped by (uri, variable)).
    let mut artifacts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for ev in &events {
        for key in ["variables", "initial_variables"] {
            if let Some(map) = ev.payload.get(key).and_then(|v| v.as_object()) {
                collect_artifact_refs(map, "history", &mut artifacts, &mut seen);
            }
        }
    }
    let instance_variables: serde_json::Map<String, serde_json::Value> = inst
        .variables
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    collect_artifact_refs(&instance_variables, "instance", &mut artifacts, &mut seen);

    Ok(Json(AuditResponse {
        instance_id: id,
        instance: AuditInstanceView {
            state: status_str(inst.state).to_string(),
            tenant_id: inst.tenant_id.clone(),
        },
        definition,
        events: views,
        decisions,
        artifacts,
    }))
}

// --- Step trace API (E6-1, issue #26) -----------------------------------------

/// GET /api/v1/process-instances/:id/steps — the unified step timeline
/// (E6-1): one step per `(node, token)` in causal order, joining token
/// transitions, worker lease attempts, and the E3-1 AI task envelope
/// metadata (model/tool versions, cost) keyed by the trace id.
///
/// Read-only aggregation over existing stores (instance, definition,
/// history) — see [`super::step_trace`]. Sensitive-by-default: the
/// response contains NO variable values, only names/metadata; the trace
/// id IS the instance id in v1 (`docs/trace.md`). Errors: 404
/// `not_found` for an unknown instance, 500 `internal` on storage
/// failures — a definition that merely FAILS TO LOAD is one of those
/// storage failures (consistent with the E2-4 contract); only a
/// definition that is genuinely ABSENT (`Ok(None)`) degrades step-kind
/// classification to the event-type fallback.
pub async fn get_instance_steps(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<crate::step_trace::StepsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let process_store = Arc::clone(&state.storage.process_store);
    let inst = process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| tenant_gate_not_found(&id))?;
    // E5-1: an instance of another tenant is reported like a missing one.
    if tenant_denied(&identity, &inst) {
        return Err(tenant_gate_not_found(&id));
    }
    // Err (storage failure) ⇒ 500 `internal` like every other store error
    // here; Ok(None) (definition genuinely absent) ⇒ degrade node-kind
    // classification to the event-type fallback (see
    // step_trace::step_kind). By inspection: no faulting-store test —
    // injecting a failing ProcessDefinitionStore is not cheap with the
    // current memory/Postgres AppState wiring; the memory driver's
    // load() is infallible in practice.
    let definition = state
        .storage
        .def_store
        .load(&inst.process_def_id)
        .await
        .map_err(internal_error_tuple)?;
    let mut events =
        HistoryRepo::list_by_instance(state.storage.history_repo.as_ref(), &id, None, None)
            .await
            .map_err(internal_error_tuple)?;
    // Causal order: (occurred_at, phase_rank, insertion_index) via the
    // pure, unit-tested `sort_history_causally`. A plain stable
    // occurred_at sort would rely on the store preserving insertion
    // order on same-second ties (the memory adapter does; a SQL history
    // listing does not guarantee it), and an id tie-break would
    // RANDOMIZE same-second lock→outcome pairs (history ids are random
    // UUIDs). See step_trace::phase_rank and docs/trace.md.
    crate::step_trace::sort_history_causally(&mut events);
    Ok(Json(crate::step_trace::assemble_steps(
        &inst,
        definition.as_ref(),
        &events,
    )))
}

/// GET /api/v1/tasks?type=user|external
///
/// Optional route-level pagination (E2-4): `limit`/`offset` query
/// parameters (validated like the instances endpoint: `limit` 1..=500,
/// `offset` ≥ 0, else 400 `invalid_request`) slice the assembled task list
/// IN MEMORY after the full scan. Absent parameters keep the previous
/// behavior of returning every task. The store is not extended for this
/// endpoint — instance listing is the hot path and is paginated at the
/// store level instead (see [`list_process_instances`]).
pub async fn list_tasks(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<TaskListItem>>, (StatusCode, Json<ErrorResponse>)> {
    // Route-level pagination: absent parameters ⇒ no windowing (backward
    // compatible). When present, validation matches the instances list.
    let page = if params.contains_key("limit") || params.contains_key("offset") {
        Some(parse_page_params(&params)?)
    } else {
        None
    };
    let tenant_id = tenant_from_identity(&identity);
    let process_store = Arc::clone(&state.storage.process_store);
    let def_store = Arc::clone(&state.storage.def_store);
    let running_ids = process_store
        .list_running(tenant_id.as_deref())
        .await
        .map_err(internal_error_tuple)?;
    let type_filter = params.get("type").map(String::as_str);
    let mut out = Vec::new();
    for instance_id in running_ids {
        let Ok(Some(instance)) = process_store.load(&instance_id).await else {
            continue;
        };
        let Ok(Some(def)) = def_store.load(&instance.process_def_id).await else {
            continue;
        };
        for token in &instance.tokens {
            if token.status != TokenStatus::Waiting {
                continue;
            }
            let Some(node) = def.nodes.get(token.node_id.as_str()) else {
                continue;
            };
            let task_type = match &node.node_type {
                NodeType::UserTask { .. } => "user",
                NodeType::ServiceTask(_) | NodeType::ExternalTask { .. } => "external",
                _ => continue,
            };
            if let Some(filter) = type_filter {
                if filter != task_type {
                    continue;
                }
            }
            let task_id = format!("{}:{}", instance.id, token.node_id);
            out.push(TaskListItem {
                task_id,
                node_id: token.node_id.clone(),
                instance_id: instance.id.clone(),
                task_type: task_type.to_string(),
            });
        }
    }
    if let Some(page) = page {
        let offset = (page.offset as usize).min(out.len());
        out.drain(..offset);
        out.truncate(page.limit as usize);
    }
    Ok(Json(out))
}

fn parse_task_id(task_id: &str) -> Option<(String, String)> {
    let mut parts = task_id.rsplitn(2, ':');
    let node_id = parts.next()?.to_string();
    let instance_id = parts.next()?.to_string();
    if instance_id.is_empty() || node_id.is_empty() {
        return None;
    }
    Some((instance_id, node_id))
}

#[derive(Serialize)]
pub struct FormSchemaResponse {
    pub task_id: String,
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<bpm_engine_core::FormField>>,
}

/// GET /api/v1/tasks/:task_id/form — return form schema for a user task.
pub async fn get_task_form(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(task_id): Path<String>,
) -> Result<Json<FormSchemaResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (instance_id, node_id) = parse_task_id(&task_id)
        .ok_or_else(|| bad_request(format!("invalid task_id: {}", task_id)))?;
    let process_store = Arc::clone(&state.storage.process_store);
    let def_store = Arc::clone(&state.storage.def_store);
    let instance = process_store
        .load(&instance_id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| tenant_gate_not_found(&instance_id))?;
    // E5-1: a task of another tenant is reported like an unknown one.
    if tenant_denied(&identity, &instance) {
        return Err(tenant_gate_not_found(&instance_id));
    }
    let def = def_store
        .load(&instance.process_def_id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "process definition not found: {}",
                    instance.process_def_id
                ))),
            )
        })?;
    let node = def.nodes.get(node_id.as_str()).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!(
                "node not found: {}",
                node_id
            ))),
        )
    })?;
    match &node.node_type {
        NodeType::UserTask {
            form_key,
            form_fields,
        } => Ok(Json(FormSchemaResponse {
            task_id,
            node_id,
            form_key: form_key.clone(),
            fields: form_fields.clone(),
        })),
        _ => Err(bad_request(format!("node {} is not a user task", node_id))),
    }
}

/// POST /api/v1/tasks/:task_id/complete
pub async fn complete_task_by_id(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(task_id): Path<String>,
    ValidatedJson(body): ValidatedJson<CompleteTaskBodyRequest>,
) -> Result<Json<CompleteTaskResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (instance_id, node_id) = parse_task_id(&task_id)
        .ok_or_else(|| bad_request(format!("invalid task_id: {}", task_id)))?;

    // E5-1 cross-tenant gate: under a verified identity, a task whose
    // instance belongs to another tenant is reported like an unknown
    // instance BEFORE any engine state is touched. (A missing instance
    // keeps the pre-E5-1 fall-through behavior.)
    if let Identity::Principal { .. } = &identity {
        match state.storage.process_store.load(&instance_id).await {
            Ok(Some(instance)) if tenant_denied(&identity, &instance) => {
                return Err(tenant_gate_not_found(&instance_id));
            }
            // Storage errors on the gate path are surfaced as 500 like
            // every other store error in this handler's flow.
            Err(e) => return Err(internal_error_tuple(e)),
            Ok(_) => {}
        }
    }

    // E2-3: enforce the variable value contract at this write boundary
    // (valid envelopes, valid artifact URIs, size cap) before the event
    // enters the engine. E3-1: `__task_envelope` is engine-owned —
    // user tasks have no envelope field, so the key is simply reserved.
    enforce_reserved_variable_keys(&body.variables)?;
    enforce_variable_contract(&body.variables, &state.variable_limits)?;

    // E5-2 lifecycle gate: a user-task completion is a token transition —
    // refused while the instance is paused (409 `instance_paused`) and
    // rejected after it was cancelled (409 `instance_cancelled`).
    instance_task_op_gate(state.as_ref(), &instance_id, false).await?;

    // Validate required form fields if the user task has a form definition
    let process_store = Arc::clone(&state.storage.process_store);
    let def_store = Arc::clone(&state.storage.def_store);
    if let Ok(Some(instance)) = process_store.load(&instance_id).await {
        if let Ok(Some(def)) = def_store.load(&instance.process_def_id).await {
            if let Some(node) = def.nodes.get(node_id.as_str()) {
                if let NodeType::UserTask {
                    form_fields: Some(ref fields),
                    ..
                } = node.node_type
                {
                    let mut missing = Vec::new();
                    for f in fields {
                        if f.required && !body.variables.contains_key(&f.id) {
                            missing.push(f.id.clone());
                        }
                    }
                    if !missing.is_empty() {
                        return Err((
                            StatusCode::BAD_REQUEST,
                            Json(ErrorResponse::with_code(
                                format!("missing required fields: {}", missing.join(", ")),
                                "missing_required_fields",
                            )),
                        ));
                    }
                }
            }
        }
    }

    let tenant_id = tenant_from_identity(&identity);
    let mut ctx = build_ctx(state.as_ref(), tenant_id);
    let ev = EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
        task_id: task_id.clone(),
        instance_id: instance_id.clone(),
        node_id: node_id.clone(),
        variables: body.variables,
    });
    state.engine.run_async(ev, &mut ctx).await;
    // E5-3 boundary event: WHO completed the user task (the engine's
    // `UserTaskCompleted` carries the variables but no actor).
    {
        let payload = serde_json::json!({
            "instance_id": instance_id.as_str(),
            "task_id": task_id.as_str(),
            "node_id": node_id.as_str(),
            "task_kind": "user",
            "actor": identity.actor(),
        });
        let _ = HistoryRepo::append(
            state.storage.history_repo.as_ref(),
            &instance_id,
            "TaskCompleted",
            &payload,
            &occurred_at_now(),
        )
        .await;
    }
    info!(
        task_id = %task_id,
        instance_id = %instance_id,
        actor = identity.actor(),
        "user task completed"
    );
    Ok(Json(CompleteTaskResponse {
        status: "COMPLETED".to_string(),
    }))
}

/// POST /api/v1/external-tasks/fetch-and-lock
///
/// E5-1 tenancy: under a JWT-verified identity the fetch is scoped to the
/// claim tenant — tasks of other tenants (or tenant-less instances) are
/// neither delivered NOR leased (store-level `fetch_and_lock_scoped`).
/// Open/API-key modes keep the pre-E5-1 unscoped fetch.
pub async fn external_task_fetch_and_lock(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    ValidatedJson(body): ValidatedJson<FetchAndLockRequest>,
) -> Result<Json<Vec<ExternalTaskResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let external_task_store = Arc::clone(&state.storage.external_task_store);
    let _ = external_task_store
        .reclaim_expired_locks()
        .await
        .map_err(internal_error_tuple)?;
    let lock_duration = Duration::from_millis(body.lock_duration_ms);
    let max_tasks = body.max_tasks as usize;
    let tasks = match &identity {
        Identity::Principal { tenant, .. } => {
            external_task_store
                .fetch_and_lock_scoped(
                    &body.worker_id,
                    &body.task_types,
                    max_tasks,
                    lock_duration,
                    tenant,
                )
                .await
        }
        _ => {
            external_task_store
                .fetch_and_lock(&body.worker_id, &body.task_types, max_tasks, lock_duration)
                .await
        }
    }
    .map_err(internal_error_tuple)?;
    let occurred_at = occurred_at_now();
    for t in &tasks {
        let payload = serde_json::json!({
            "task_id": t.task_id,
            "token_id": t.token_id,
            "process_instance_id": t.process_instance_id,
            "worker_id": body.worker_id,
            "retries": t.retries,
            // E5-3: the VERIFIED request identity (JWT `sub` under JWT
            // auth) alongside the client-claimed `worker_id` — the lease
            // owner and the accountable principal can differ.
            "actor": identity.actor(),
        });
        let _ = HistoryRepo::append(
            state.storage.history_repo.as_ref(),
            &t.process_instance_id,
            "ExternalTaskLocked",
            &payload,
            &occurred_at,
        )
        .await;
        info!(
            task_id = %t.task_id,
            worker_id = %body.worker_id,
            instance_id = %t.process_instance_id,
            "external task locked"
        );
    }
    let out: Vec<ExternalTaskResponse> = tasks
        .into_iter()
        .map(|t| ExternalTaskResponse {
            task_id: t.task_id,
            token_id: t.token_id,
            process_instance_id: t.process_instance_id,
            task_type: t.task_type,
            variables: t.variables,
        })
        .collect();
    Ok(Json(out))
}

/// POST /api/v1/external-tasks/:task_id/complete
pub async fn external_task_complete(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(task_id): Path<String>,
    ValidatedJson(body): ValidatedJson<ExternalTaskCompleteRequest>,
) -> Result<Json<CompleteTaskResponse>, Response> {
    // E3-1: the envelope may arrive through EITHER channel:
    // - the typed `envelope` field (validated and merged by the server), or
    // - already serialized at the reserved `__task_envelope` key of the
    //   variables map — the worker-sdk wire format (`TaskOutput::build` /
    //   `EngineClient::complete_with_envelope` put it there; byte-identical
    //   to a worker that completed "exactly as today").
    // The key is engine-owned either way: supplying BOTH channels, or a
    // value there that is not a well-formed serialized envelope, is
    // rejected with 4xx reserved_variable_key; a well-formed but invalid
    // envelope (any channel) is rejected with 4xx task_envelope_invalid.
    let mut variables = body.variables;
    let supplied_envelope_value = variables.remove(AI_TASK_ENVELOPE_KEY);
    let envelope = match (body.envelope, supplied_envelope_value) {
        (Some(_), Some(_)) => {
            warn!(
                variable = AI_TASK_ENVELOPE_KEY,
                "rejecting envelope supplied through both channels"
            );
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(
                    format!(
                        "variable '{}' collides with the typed 'envelope' field; \
                         supply the envelope through exactly one channel",
                        AI_TASK_ENVELOPE_KEY
                    ),
                    "reserved_variable_key",
                )),
            )
                .into_response());
        }
        (Some(envelope), None) => Some(envelope),
        (None, Some(value)) => match AiTaskEnvelope::from_envelope_value(&value) {
            Ok(envelope) => Some(envelope),
            Err(e) => {
                warn!(
                    variable = AI_TASK_ENVELOPE_KEY,
                    reason = %e,
                    "rejecting non-envelope value at the reserved key"
                );
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse::with_code(
                        format!(
                            "variable '{}' is reserved; it must carry the engine's serialized \
                             AI task envelope (the worker-sdk wire format) — use the 'envelope' \
                             field instead ({})",
                            AI_TASK_ENVELOPE_KEY, e
                        ),
                        "reserved_variable_key",
                    )),
                )
                    .into_response());
            }
        },
        (None, None) => None,
    };
    if let Some(envelope) = &envelope {
        merge_task_envelope(envelope, &mut variables)
            .map_err(|(status, json)| (status, json).into_response())?;
    }
    // E2-3: enforce the variable value contract at this write boundary
    // BEFORE the store mutates the task, so a rejected request leaves the
    // task locked and completable with valid variables. The checks run on
    // the MERGED map, so the envelope value is size-capped like any other
    // variable.
    enforce_variable_contract(&variables, &state.variable_limits)
        .map_err(|(status, json)| (status, json).into_response())?;
    let external_task_store = Arc::clone(&state.storage.external_task_store);
    // E5-1 cross-tenant gate: under a verified identity, a task whose
    // instance belongs to another tenant is refused BEFORE the store
    // mutates the task, with the SAME body an unknown task id produces
    // (400 task_not_found — no existence leak).
    external_task_tenant_gate(&state, &identity, &external_task_store, &task_id)
        .await
        .map_err(|(status, json)| (status, json).into_response())?;
    // E5-2 lifecycle gate: refuse the completion while the instance is
    // paused (the task stays Locked; the worker retries after resume or
    // the expired lease re-delivers) or after it was cancelled. Checked
    // BEFORE the store mutates the task.
    if let Some(task) = external_task_store
        .get(&task_id)
        .await
        .map_err(internal_error)?
    {
        instance_task_op_gate(state.as_ref(), &task.process_instance_id, false)
            .await
            .map_err(|(status, json)| (status, json).into_response())?;
    }
    external_task_store
        .complete(&task_id, &body.worker_id, variables.clone())
        .await
        .map_err(external_task_error_response)?;
    let task = external_task_store
        .get(&task_id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "task not found after complete: {}",
                    task_id
                ))),
            )
                .into_response()
        })?;
    info!(
        task_id = %task_id,
        worker_id = %body.worker_id,
        actor = identity.actor(),
        instance_id = %task.process_instance_id,
        "external task completed"
    );
    // Emit ExternalTaskCompleted through engine event loop.
    // The ExternalTaskCompletedHandler will:
    // 1. Merge worker-returned variables into the instance
    // 2. Remove the completed token
    // 3. Create new tokens via move_token
    // 4. Save the instance
    // 5. Emit TokenArrived events for each new token
    let tenant_id = tenant_from_identity(&identity);
    let mut ctx = build_ctx(state.as_ref(), tenant_id);
    // Load instance to get the node_id for the token
    let instance = ctx
        .process_store
        .load(&task.process_instance_id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "instance not found: {}",
                    task.process_instance_id
                ))),
            )
                .into_response()
        })?;
    let token = instance
        .tokens
        .iter()
        .find(|t| t.id == task.token_id)
        .ok_or_else(|| {
            bad_request(format!("token not found in instance: {}", task.token_id)).into_response()
        })?;
    let node_id = token.node_id.clone();
    let ev = EngineEvent::ExternalTaskCompleted(payloads::ExternalTaskCompleted {
        instance_id: task.process_instance_id.clone(),
        token_id: task.token_id.clone(),
        node_id: node_id.clone(),
        variables,
    });
    state.engine.run_async(ev, &mut ctx).await;
    // E5-3 boundary event: WHO completed the external task — the verified
    // actor alongside the client-claimed `worker_id` (the engine's
    // `ExternalTaskCompleted` event has neither).
    {
        let instance_id = task.process_instance_id.clone();
        let payload = serde_json::json!({
            "instance_id": instance_id.as_str(),
            "task_id": task.task_id,
            "node_id": node_id,
            "task_kind": "external",
            "worker_id": body.worker_id,
            "actor": identity.actor(),
        });
        let _ = HistoryRepo::append(
            state.storage.history_repo.as_ref(),
            &instance_id,
            "TaskCompleted",
            &payload,
            &occurred_at_now(),
        )
        .await;
    }
    Ok(Json(CompleteTaskResponse {
        status: "COMPLETED".to_string(),
    }))
}

/// POST /api/v1/external-tasks/:task_id/extend-lock — extend lock duration on a locked task.
pub async fn external_task_extend_lock(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(task_id): Path<String>,
    ValidatedJson(body): ValidatedJson<ExtendLockRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let external_task_store = Arc::clone(&state.storage.external_task_store);
    // E5-1 cross-tenant gate (see external_task_tenant_gate).
    external_task_tenant_gate(&state, &identity, &external_task_store, &task_id).await?;
    // E5-2 lifecycle gate: lease MAINTENANCE continues during a pause
    // (in-flight leases are deliberately kept alive, ADR-008 §5), but a
    // cancelled instance rejects even the extension.
    if let Some(task) = external_task_store
        .get(&task_id)
        .await
        .map_err(internal_error_tuple)?
    {
        instance_task_op_gate(state.as_ref(), &task.process_instance_id, true).await?;
    }
    let extension = Duration::from_millis(body.extension_ms);
    let ok = external_task_store
        .extend_lock(&task_id, &body.worker_id, extension)
        .await
        .map_err(internal_error_tuple)?;
    if !ok {
        // 409 with the stable `lock_conflict` code: the task is unknown,
        // not locked, or owned by a different worker (the store reports a
        // single boolean, so the causes share one code).
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse::with_code(
                "task not found, not locked, or owned by different worker",
                error_codes::LOCK_CONFLICT,
            )),
        ));
    }
    Ok(Json(serde_json::json!({ "status": "lock_extended" })))
}

/// POST /api/v1/external-tasks/:task_id/fail
pub async fn external_task_fail(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(task_id): Path<String>,
    ValidatedJson(body): ValidatedJson<ExternalTaskFailRequest>,
) -> Result<Json<CompleteTaskResponse>, Response> {
    // E3-1: optional retry category, validated up front. E3-2 semantics:
    // `permanent` is BEHAVIORAL — the task skips the retry loop and goes
    // straight to Failed (DLQ + TokenFailed, exactly like retry
    // exhaustion); `transient` (or absent) keeps the legacy
    // decrement-and-requeue behavior.
    let retry_category = match body.category.as_deref() {
        None => None,
        Some("transient") => Some(RetryCategory::Transient),
        Some("permanent") => Some(RetryCategory::Permanent),
        Some(other) => {
            warn!(category = %other, "rejecting invalid retry category");
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(
                    format!(
                        "category must be \"transient\" or \"permanent\", got {:?}",
                        other
                    ),
                    "invalid_retry_category",
                )),
            )
                .into_response());
        }
    };
    let permanent = matches!(retry_category, Some(RetryCategory::Permanent));
    // E3-1: optional AI task envelope, validated exactly like on complete
    // (4xx task_envelope_invalid) BEFORE the store mutates the task. A
    // given category is recorded into the envelope's retry_category.
    let mut envelope = body.envelope;
    if let (Some(envelope), Some(category)) = (&mut envelope, retry_category) {
        envelope.retry_category = Some(category);
    }
    if let Some(envelope) = &envelope {
        if let Err(e) = envelope.validate() {
            warn!(
                code = e.code(),
                "rejecting invalid AI task envelope on fail"
            );
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::with_code(
                    format!("{}: {}", e.code(), e),
                    "task_envelope_invalid",
                )),
            )
                .into_response());
        }
    }
    // Fail-path persistence decision (E3-1, refined by E3-2): the store's
    // `fail()` still has no variables channel — the category is persisted
    // as a structured JSON prefix on the error message
    // (`{"retry_category":...}: <error>`, surviving task.error_message,
    // ExternalTaskFailed history and the DLQ), and the envelope is
    // recorded in the ExternalTaskFailed history payload only. The envelope
    // is deliberately NOT embedded in the error message: schemas can be
    // multi-KB and that path bypasses history redaction. E3-2 adds the
    // BEHAVIOR: the permanent flag routes the store call to the
    // skip-retries path (task straight to Failed, DLQ + TokenFailed fire
    // exactly as retry exhaustion does today).
    let error_message = match retry_category {
        Some(category) => format!(
            "{}: {}",
            serde_json::json!({ "retry_category": category }),
            body.error
        ),
        None => body.error.clone(),
    };
    let external_task_store = Arc::clone(&state.storage.external_task_store);
    // E5-1 cross-tenant gate (see external_task_tenant_gate).
    external_task_tenant_gate(&state, &identity, &external_task_store, &task_id)
        .await
        .map_err(|(status, json)| (status, json).into_response())?;
    // E5-2 lifecycle gate: fail (retry/requeue) is a transition like
    // complete — refused while paused, rejected after cancel.
    if let Some(task) = external_task_store
        .get(&task_id)
        .await
        .map_err(internal_error)?
    {
        instance_task_op_gate(state.as_ref(), &task.process_instance_id, false)
            .await
            .map_err(|(status, json)| (status, json).into_response())?;
    }
    let retry_after = body.retry_after_ms.map(Duration::from_millis);
    external_task_store
        .fail(
            &task_id,
            &body.worker_id,
            error_message.clone(),
            retry_after,
            permanent,
        )
        .await
        .map_err(external_task_error_response)?;
    let worker_id_for_log = body.worker_id.clone();
    let task = external_task_store
        .get(&task_id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "task not found after fail: {}",
                    task_id
                ))),
            )
                .into_response()
        })?;
    let occurred_at = occurred_at_now();
    let mut payload = serde_json::json!({
        "task_id": task.task_id,
        "token_id": task.token_id,
        "process_instance_id": task.process_instance_id,
        "worker_id": worker_id_for_log,
        "retries": task.retries,
        "error_message": error_message,
        // E5-3: the VERIFIED request identity alongside the client-claimed
        // `worker_id` — who is accountable for the failure report.
        "actor": identity.actor(),
    });
    // The envelope rides in the standard variable-carrying payload key and
    // is redacted with the same policy the engine's history handler
    // applies (oversized → __truncated:), so the fail path gains none of
    // the redaction bypasses a raw error_message embedding would have.
    if let Some(envelope) = &envelope {
        let stored = envelope.to_envelope_value();
        let redacted = redact_history_variable_value(&stored, &state.history_redaction_policy);
        let mut envelope_vars = serde_json::Map::new();
        envelope_vars.insert(
            AI_TASK_ENVELOPE_KEY.to_string(),
            serde_json::Value::String(redacted),
        );
        if let serde_json::Value::Object(map) = &mut payload {
            map.insert(
                "variables".to_string(),
                serde_json::Value::Object(envelope_vars),
            );
        }
    }
    let _ = HistoryRepo::append(
        state.storage.history_repo.as_ref(),
        &task.process_instance_id,
        "ExternalTaskFailed",
        &payload,
        &occurred_at,
    )
    .await;
    warn!(
        task_id = %task_id,
        worker_id = %body.worker_id,
        retries = task.retries,
        instance_id = %task.process_instance_id,
        error = %body.error,
        "external task failed"
    );
    if task.state == ExternalTaskState::Failed {
        // Reached either by retry exhaustion (legacy) or by a permanent
        // category fail (E3-2 fast path): insert into dead letter queue
        // (input variables only — the fail-path envelope is in the
        // ExternalTaskFailed history event) and terminate the token.
        let dl_entry = bpm_engine_storage::DeadLetterEntry {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task.task_id.clone(),
            token_id: task.token_id.clone(),
            process_instance_id: task.process_instance_id.clone(),
            task_type: task.task_type.clone(),
            error_message: error_message.clone(),
            variables: serde_json::to_string(&task.variables).unwrap_or_default(),
            // E5-1: dead letters inherit the VERIFIED tenant under JWT
            // (so the tenant's operators can list/requeue them); open and
            // API-key modes keep the pre-E5-1 untagged behavior.
            tenant_id: match &identity {
                Identity::Principal { tenant, .. } => Some(tenant.clone()),
                _ => None,
            },
            created_at: occurred_at.clone(),
        };
        let _ = state.storage.dead_letter_store.insert(&dl_entry).await;

        let tenant_id = None::<String>;
        let mut ctx = build_ctx(state.as_ref(), tenant_id);
        // E2-4 review fix: a storage ERROR is 500 `internal` (previously
        // swallowed by `.ok()`); an instance that is merely missing after
        // the fail (Ok(None)) keeps the empty-node_id fallback below.
        let inst = state
            .storage
            .process_store
            .load(&task.process_instance_id)
            .await
            .map_err(internal_error)?;
        let node_id = inst
            .as_ref()
            .and_then(|i| {
                i.tokens
                    .iter()
                    .find(|t| t.id == task.token_id)
                    .map(|t| t.node_id.clone())
            })
            .unwrap_or_default();
        let ev = EngineEvent::TokenFailed(payloads::TokenFailed {
            instance_id: task.process_instance_id.clone(),
            token_id: task.token_id.clone(),
            node_id,
            reason: task.error_message.unwrap_or(body.error),
        });
        state.engine.run_async(ev, &mut ctx).await;
    }
    Ok(Json(CompleteTaskResponse {
        status: "FAILED".to_string(),
    }))
}

// --- Approval & instance lifecycle API (E5-2, ADR-008) ----------------------

/// E5-2 lifecycle gate for task operations: an external-task or user-task
/// operation is refused (409) while the target instance is paused
/// (`instance_paused`) or after it was cancelled (`instance_cancelled`).
///
/// `allow_paused = true` lets lease MAINTENANCE (extend-lock) continue
/// during a pause — the honest pause semantics keep in-flight leases
/// alive (ADR-008 §5) — while cancelled instances still reject everything.
/// A missing instance keeps the caller's own not-found error (the gate is
/// additive, not a replacement).
async fn instance_task_op_gate(
    state: &AppState,
    instance_id: &str,
    allow_paused: bool,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let inst = state
        .storage
        .process_store
        .load(instance_id)
        .await
        .map_err(internal_error_tuple)?;
    let Some(inst) = inst else {
        return Ok(());
    };
    if inst.state == InstanceState::Terminated {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse::with_code(
                format!(
                    "process instance {} was cancelled; task operations on it are rejected",
                    instance_id
                ),
                error_codes::INSTANCE_CANCELLED,
            )),
        ));
    }
    if !allow_paused
        && inst
            .tokens
            .iter()
            .any(|t| t.status == TokenStatus::Suspended)
    {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse::with_code(
                format!(
                    "process instance {} is paused; the task operation is refused until \
                     the instance is resumed",
                    instance_id
                ),
                error_codes::INSTANCE_PAUSED,
            )),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct ApprovalDecideRequest {
    /// The decision: `"approve"` or `"reject"` (anything else is 400
    /// `invalid_request`).
    pub decision: String,
    /// Optional operator note recorded with the decision (record + history).
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct ApprovalRecordResponse {
    pub approval_id: String,
    pub instance_id: String,
    pub node_id: String,
    pub token_id: String,
    pub task_type: String,
    /// `PENDING` | `APPROVED` | `REJECTED`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct ApprovalListResponse {
    pub instance_id: String,
    pub approvals: Vec<ApprovalRecordResponse>,
}

fn approval_record_response(record: &bpm_engine_core::ApprovalRecord) -> ApprovalRecordResponse {
    let status = match record.status {
        bpm_engine_core::ApprovalStatus::Pending => "PENDING",
        bpm_engine_core::ApprovalStatus::Approved => "APPROVED",
        bpm_engine_core::ApprovalStatus::Rejected => "REJECTED",
    };
    ApprovalRecordResponse {
        approval_id: record.id.clone(),
        instance_id: record.instance_id.clone(),
        node_id: record.node_id.clone(),
        token_id: record.token_id.clone(),
        task_type: record.task_type.clone(),
        status: status.to_string(),
        decided_by: record.decided_by.clone(),
        decided_at: record.decided_at.clone(),
        note: record.note.clone(),
        created_at: record.created_at.clone(),
    }
}

/// 404 `approval_not_found` — the same body for an unknown id and (under a
/// verified identity) another tenant's record (no existence leak).
fn approval_not_found(approval_id: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse::with_code(
            format!("approval not found: {}", approval_id),
            error_codes::APPROVAL_NOT_FOUND,
        )),
    )
}

/// Map an [`ApprovalError`] to the typed REST contract.
fn approval_error_response(e: ApprovalError) -> (StatusCode, Json<ErrorResponse>) {
    match e {
        ApprovalError::NotFound { approval_id } => approval_not_found(&approval_id),
        ApprovalError::Internal(msg) => {
            tracing::error!(error = %msg, "approval store internal error");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal("internal server error")),
            )
        }
    }
}

/// GET /api/v1/process-instances/:id/approvals — list the instance's
/// approval records (all roles may read; operators use this to discover
/// pending approval ids for the decide endpoint).
pub async fn list_instance_approvals(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<ApprovalListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let inst = state
        .storage
        .process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?;
    match inst {
        Some(inst) if tenant_denied(&identity, &inst) => Err(tenant_gate_not_found(&id)),
        Some(_) => {
            let records = state
                .storage
                .approval_store
                .list_by_instance(&id)
                .await
                .map_err(internal_error_tuple)?;
            Ok(Json(ApprovalListResponse {
                instance_id: id,
                approvals: records.iter().map(approval_record_response).collect(),
            }))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!(
                "process instance not found: {}",
                id
            ))),
        )),
    }
}

/// POST /api/v1/approvals/:approval_id/decide — approve or reject a pending
/// approval record (operator action, E5-2 / ADR-008).
///
/// Only-once: the FIRST decide flips the record and takes effect (approve
/// creates the fetchable external task with the engine-written
/// `__approval=true`; reject dead-letters the step and fails the token).
/// A retried SAME decision replays 200 with the recorded outcome — and, if
/// the decision's EFFECT is still Pending (a crash or failure between the
/// durable decision and its application), the replay RE-DRIVES the effect
/// before answering (self-healing retry). A CONFLICTING decision answers
/// 409 `approval_already_decided` — the original outcome always stands.
///
/// If applying the effect fails (e.g. the task store rejects the insert),
/// the DECISION is already durably recorded with effect=Pending and the
/// route answers 500 `internal` naming the approval id and the recovery
/// action — the operator retries the SAME decide call, which re-drives the
/// effect (the single-service recovery story, ADR-008 §7.1; the startup
/// sweep re-drives pending effects as well).
pub async fn approval_decide(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(approval_id): Path<String>,
    ValidatedJson(body): ValidatedJson<ApprovalDecideRequest>,
) -> Result<Json<ApprovalRecordResponse>, (StatusCode, Json<ErrorResponse>)> {
    let decision = ApprovalDecision::parse(&body.decision).ok_or_else(|| {
        bad_request(format!(
            "decision must be \"approve\" or \"reject\", got {:?}",
            body.decision
        ))
    })?;
    let approval_store = Arc::clone(&state.storage.approval_store);
    let record = approval_store
        .get(&approval_id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| approval_not_found(&approval_id))?;
    // E5-1 tenant gate: a record of another tenant is reported exactly like
    // an unknown one.
    let instance = state
        .storage
        .process_store
        .load(&record.instance_id)
        .await
        .map_err(internal_error_tuple)?;
    if instance
        .as_ref()
        .is_some_and(|i| tenant_denied(&identity, i))
    {
        return Err(approval_not_found(&approval_id));
    }

    let actor = identity.actor().to_string();
    let outcome = approval_store
        .decide(&approval_id, decision, &actor, body.note.as_deref())
        .await
        .map_err(approval_error_response)?;

    let record = match outcome {
        ApprovalDecideOutcome::AlreadyDecided(record) => {
            // Only-once: the original outcome stands. A retried same
            // decision is a 200 replay; a conflicting decision is 409.
            if record.status != decision.resulting_status() {
                warn!(
                    approval_id = %approval_id,
                    actor = %actor,
                    status = ?record.status,
                    "rejecting conflicting decision on an already-decided approval"
                );
                return Err((
                    StatusCode::CONFLICT,
                    Json(ErrorResponse::with_code(
                        format!(
                            "approval {} is already decided ({:?}); a conflicting decision \
                             is rejected and the original outcome stands",
                            approval_id, record.status
                        ),
                        error_codes::APPROVAL_ALREADY_DECIDED,
                    )),
                ));
            }
            if record.effect == bpm_engine_core::ApprovalEffect::Pending {
                // Self-healing replay (ADR-008 §7.1): the decision is
                // durable but its effect was never (fully) applied —
                // re-drive it BEFORE answering, so a retried decide
                // converges the state instead of papering over the gap.
                info!(
                    approval_id = %approval_id,
                    actor = %actor,
                    "approval decide replayed with a pending effect; re-driving the effect"
                );
                drive_approval_effect(&state, &record, Some(&identity))
                    .await
                    .map_err(|e| approval_effect_pending_error(&approval_id, e))?;
            } else {
                info!(
                    approval_id = %approval_id,
                    actor = %actor,
                    "approval decide replayed (already decided identically)"
                );
            }
            return Ok(Json(approval_record_response(&record)));
        }
        ApprovalDecideOutcome::Decided(record) => record,
    };

    // The decision history event is appended by ensure_decision_history
    // INSIDE drive_approval_effect's lifecycle lock (exactly-once by
    // approval id) — a route-level append here would duplicate or race it.

    info!(
        approval_id = %approval_id,
        instance_id = %record.instance_id,
        actor = %actor,
        decision = body.decision,
        "approval decided"
    );

    // Apply the effect of the now-durable decision. Errors PROPAGATE as a
    // typed 500: the decision is recorded and its effect is pending — the
    // operator retries the SAME decide call, which re-drives it.
    drive_approval_effect(&state, &record, Some(&identity))
        .await
        .map_err(|e| approval_effect_pending_error(&approval_id, e))?;
    Ok(Json(approval_record_response(&record)))
}

/// 500 `internal` for a failed approval EFFECT application (E5-2, ADR-008
/// §7.1): names the approval id and states the recovery contract — the
/// DECISION is durable, the EFFECT is pending, and retrying the SAME decide
/// call (or restarting, whose startup sweep re-drives pending effects)
/// converges it. The underlying error is logged server-side only.
fn approval_effect_pending_error(
    approval_id: &str,
    error: anyhow::Error,
) -> (StatusCode, Json<ErrorResponse>) {
    tracing::error!(
        approval_id = %approval_id,
        error = %error,
        "approval decision effect failed; the decision is recorded and its effect is pending"
    );
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::internal(format!(
            "approval {}: the decision is durably recorded but applying its effect \
             failed; retry the same decide call — the effect is re-driven (the \
             server's startup sweep re-drives pending effects as well)",
            approval_id
        ))),
    )
}

/// Apply (or re-apply) the EFFECT of a decided approval record and confirm
/// it (`mark_effect_applied`) — the decision/effect re-drivable unit of
/// ADR-008 §7.1.
///
/// - grant → [`apply_approval_granted`]: create the fetchable external task
///   with the ENGINE-written `__approval=true`;
/// - reject → [`apply_approval_rejected`]: dead-letter the step and
///   terminate the token.
///
/// The WHOLE application runs under `instance_lifecycle_lock` (F2 + the
/// concurrent-replay race): the first decide's drive, a replay's re-drive,
/// and pause/resume/cancel serialize, so the idempotency guards below can
/// never race each other and a cancel's Terminated save can never be
/// overwritten by a later reject save.
///
/// IDEMPOTENT by construction, so a re-drive (a retried same-decision
/// decide, or the startup sweep via [`recover_pending_approval_effects`])
/// never duplicates the effect: the grant skips creation when a task
/// already exists for the token; the reject's dead-letter entry carries a
/// deterministic id and the token transition is conditional; a missing
/// decision history event is healed exactly once.
///
/// `identity` is the deciding caller on the decide path and `None` on the
/// startup sweep (the sweep resolves the tenant from the instance).
///
/// Returns Err when the effect could not be applied (e.g. the task insert
/// failed): the DECISION stays durable with effect=Pending and the caller
/// surfaces a retryable 500 / a logged sweep failure.
pub async fn drive_approval_effect(
    state: &AppState,
    record: &bpm_engine_core::ApprovalRecord,
    identity: Option<&Identity>,
) -> anyhow::Result<()> {
    // Pre-lock observation (F2's defensive version tripwire): the instance
    // as last seen BEFORE serialization.
    let observed = state
        .storage
        .process_store
        .load(&record.instance_id)
        .await?;
    let _guard = state.instance_lifecycle_lock.lock().await;
    // Heal the audit trail first: the decision's history event is appended
    // by the decide route BETWEEN the durable decide and this effect, so a
    // crash in that window loses it. The check is by (event_type,
    // approval_id) — on the normal path the event exists and this is a
    // no-op; on a re-drive it is appended exactly once with the RECORDED
    // actor/note (the durable decision's).
    ensure_decision_history(state, record).await;
    match record.status {
        bpm_engine_core::ApprovalStatus::Approved => apply_approval_granted(state, record).await?,
        bpm_engine_core::ApprovalStatus::Rejected => {
            apply_approval_rejected(state, record, identity, observed.as_ref()).await?
        }
        bpm_engine_core::ApprovalStatus::Pending => {
            // Undecided: there is no decision to apply (the sweep never
            // returns these; the decide route never passes one).
            return Ok(());
        }
    }
    state
        .storage
        .approval_store
        .mark_effect_applied(&record.id)
        .await?;
    Ok(())
}

/// Ensure the DECISION's history event exists (ADR-008 §7.1/§8: the
/// approval history must explain the decision even across the crash
/// window between the durable decide and the route's append).
///
/// Idempotent: appends only when no event of the decision's type carries
/// this approval id; the appended payload uses the RECORDED decision
/// (actor, note), not the current caller's.
async fn ensure_decision_history(state: &AppState, record: &bpm_engine_core::ApprovalRecord) {
    let history_type = match record.status {
        bpm_engine_core::ApprovalStatus::Approved => "ApprovalGranted",
        bpm_engine_core::ApprovalStatus::Rejected => "ApprovalRejected",
        bpm_engine_core::ApprovalStatus::Pending => return,
    };
    let Ok(events) = HistoryRepo::list_by_instance(
        state.storage.history_repo.as_ref(),
        &record.instance_id,
        None,
        Some(history_type),
    )
    .await
    else {
        return;
    };
    let already = events.iter().any(|e| {
        e.payload
            .get("approval_id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| id == record.id)
    });
    if already {
        return;
    }
    let payload = serde_json::json!({
        "approval_id": record.id,
        "instance_id": record.instance_id,
        "node_id": record.node_id,
        "token_id": record.token_id,
        "task_type": record.task_type,
        "actor": record.decided_by,
        "note": record.note,
    });
    let _ = HistoryRepo::append(
        state.storage.history_repo.as_ref(),
        &record.instance_id,
        history_type,
        &payload,
        &occurred_at_now(),
    )
    .await;
}

/// Startup recovery sweep (E5-2, ADR-008 §7.1): re-drive every DECIDED
/// approval record whose effect is still Pending — decisions that survived
/// a crash between the durable decide and the effect application.
///
/// Invoked from `build_app` after storage wiring, before serving. This is
/// the single-service recovery story's second re-drive path (the first is
/// a retried same-decision decide). Meaningful for the persistent
/// (Postgres) driver: the memory store is process-lifetime dev/test state
/// and starts empty at boot, so the sweep is a no-op there by construction.
///
/// Failures are logged, not fatal: a record whose effect still fails keeps
/// its pending effect — a retried decide re-drives it (the 500 body of a
/// failed decide says exactly that).
pub async fn recover_pending_approval_effects(state: &AppState) {
    let pending = match state
        .storage
        .approval_store
        .list_decided_with_pending_effect()
        .await
    {
        Ok(pending) => pending,
        Err(err) => {
            tracing::error!(
                error = %err,
                "approval effect startup sweep: listing pending effects failed"
            );
            return;
        }
    };
    for record in pending {
        tracing::info!(
            approval_id = %record.id,
            instance_id = %record.instance_id,
            status = ?record.status,
            "approval effect startup sweep: re-driving a pending effect"
        );
        if let Err(err) = drive_approval_effect(state, &record, None).await {
            tracing::error!(
                approval_id = %record.id,
                error = %err,
                "approval effect startup sweep: re-drive failed (the decision stays \
                 durable, the effect stays pending; a retried decide re-drives it)"
            );
        }
    }
}

/// Effect of a granted approval: create the fetchable external task with
/// the ENGINE-written `__approval=true` (task-creation-after-approval,
/// ADR-008 §2-3). Skipped — as complete — when the instance is no longer
/// running or the token is no longer at the node (cancelled/moved on).
///
/// Called UNDER `instance_lifecycle_lock` (by [`drive_approval_effect`]):
/// the in-lock fresh load below is the authoritative guard, serialized
/// against cancel/pause/resume and any concurrent re-drive.
///
/// Idempotent (ADR-008 §7.1): a task row already existing for the token
/// proves a prior (partially applied) grant effect created it — a re-drive
/// must not create a second task. Task-create errors PROPAGATE (the caller
/// answers a retryable 500 naming the approval id).
async fn apply_approval_granted(
    state: &AppState,
    record: &bpm_engine_core::ApprovalRecord,
) -> anyhow::Result<()> {
    // Fresh load: this is a guard read only — the grant path never mutates
    // the instance, so it takes no lifecycle lock (F2 concerns the reject
    // path's instance save).
    let instance = state
        .storage
        .process_store
        .load(&record.instance_id)
        .await?;
    let Some(instance) = instance else {
        warn!(
            instance_id = %record.instance_id,
            approval_id = %record.id,
            "approval grant effect on a missing instance; the decision stands (audit only)"
        );
        return Ok(());
    };
    if instance.state != InstanceState::Running {
        return Ok(());
    }
    let token_active = instance.tokens.iter().any(|t| {
        t.id == record.token_id
            && t.node_id == record.node_id
            && !matches!(t.status, TokenStatus::Completed | TokenStatus::Terminated)
    });
    if !token_active {
        return Ok(());
    }
    // Idempotency: a task for this token (any state) proves the grant
    // effect already created one — e.g. a crash between the task insert and
    // mark_effect_applied. Do not create a second task.
    if state
        .storage
        .external_task_store
        .get_by_token(&record.token_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let Ok(Some(def)) = state.storage.def_store.load(&instance.process_def_id).await else {
        warn!(
            instance_id = %instance.id,
            approval_id = %record.id,
            "approval granted but the process definition is gone; no task created \
             (effect recorded as applied — data loss is not transient)"
        );
        return Ok(());
    };
    let Some(node) = def.nodes.get(record.node_id.as_str()) else {
        return Ok(());
    };
    let NodeType::ExternalTask {
        task_type,
        retries,
        timeout_secs,
    } = &node.node_type
    else {
        return Ok(());
    };
    // Engine truth wins: the reserved approval variable is (over)written by
    // the engine, not the variable channel (ADR-008 §3).
    let mut variables = instance.variables.clone();
    variables.insert(
        bpm_engine_core::APPROVAL_VARIABLE.to_string(),
        "true".to_string(),
    );
    // PROPAGATES (F1/blocker): a swallowed error here would leave the
    // approval granted with no task ever created.
    state
        .storage
        .external_task_store
        .create(
            &record.token_id,
            &record.instance_id,
            task_type,
            *retries,
            *timeout_secs,
            variables,
        )
        .await?;
    Ok(())
}

/// Effect of a rejection: dead-letter the step, terminate the token, emit
/// `TokenFailed` (and `ProcessTerminated` when nothing else is active) —
/// the same family as the permanent-failure path (E3-2).
///
/// F2 (review): called UNDER `instance_lifecycle_lock` (by
/// [`drive_approval_effect`]) with the PRE-lock observation `observed` and
/// a FRESH instance load INSIDE the lock, so a concurrent cancel
/// (Terminated, saved under the same lock) can never be overwritten back
/// to Running by this path's save. When the freshly loaded instance is
/// terminal, the effect is recorded as APPLIED-WITH-NO-INSTANCE-MUTATION:
/// the dead-letter entry (and the `ApprovalRejected` history entry written
/// by the decide route or healed by the re-drive) record the rejection;
/// the terminated instance is never resurrected.
async fn apply_approval_rejected(
    state: &AppState,
    record: &bpm_engine_core::ApprovalRecord,
    identity: Option<&Identity>,
    observed: Option<&ProcessInstance>,
) -> anyhow::Result<()> {
    let occurred_at = occurred_at_now();
    // The pre-lock observation drives the defensive version tripwire below
    // and the tenant resolution (the deciding identity's tenant on the
    // decide path; the instance's tenant on the startup-sweep path).
    let observed_version = observed.map(|i| i.version);
    let tenant_id = identity
        .and_then(tenant_from_identity)
        .or_else(|| observed.and_then(|i| i.tenant_id.clone()));
    // The lifecycle mutex (held by the caller) serializes this effect
    // against pause/resume/cancel and any concurrent re-drive (single
    // service): the check-then-act window and the instance save run under
    // it, so a concurrent cancel's Terminated save can never be overwritten
    // by a stale copy loaded before the lock.
    let instance = state
        .storage
        .process_store
        .load(&record.instance_id)
        .await?;
    let Some(mut instance) = instance else {
        // The instance is gone: record the rejection (the dead-letter entry
        // and the approval history entry explain it) — nothing to mutate.
        warn!(
            instance_id = %record.instance_id,
            approval_id = %record.id,
            "approval reject effect on a missing instance; the rejection is recorded"
        );
        insert_rejection_dead_letter(state, record, tenant_id, HashMap::new(), &occurred_at)
            .await?;
        return Ok(());
    };
    // Under the process mutex a fresh in-lock load is authoritative (every
    // lifecycle-family instance mutation is serialized by this lock — the
    // single-service boundary of ADR-008 §7). The version check is a
    // defensive tripwire against out-of-band writers: the fresh version
    // must never be OLDER than the pre-lock observation.
    if let Some(observed_version) = observed_version {
        if instance.version < observed_version {
            warn!(
                instance_id = %instance.id,
                observed_version = observed_version,
                loaded_version = instance.version,
                "instance version regression detected before the reject effect's save"
            );
            debug_assert!(
                instance.version >= observed_version,
                "instance {} version regressed ({} < {} observed) inside the lifecycle lock",
                instance.id,
                instance.version,
                observed_version
            );
        }
    }
    if instance.state != InstanceState::Running {
        // F2: a terminal instance (e.g. cancel won the race, or an earlier
        // cancel terminated it before the decision) — record the rejection
        // as applied-with-no-instance-mutation: the dead-letter entry
        // records the rejected step, the ApprovalRejected history entry
        // (written by the decide route) explains it, and the terminal
        // instance is NOT resurrected.
        warn!(
            instance_id = %instance.id,
            approval_id = %record.id,
            state = %status_str(instance.state),
            "approval reject effect on a terminal instance; recorded without mutation"
        );
        insert_rejection_dead_letter(
            state,
            record,
            tenant_id,
            instance.variables.clone(),
            &occurred_at,
        )
        .await?;
        return Ok(());
    }

    // Dead letter the step (idempotent: deterministic entry id).
    insert_rejection_dead_letter(
        state,
        record,
        tenant_id.clone(),
        instance.variables.clone(),
        &occurred_at,
    )
    .await?;

    // Terminate the token (operator-forced transition, E5-2 core state
    // machine). The INSTANCE keeps its current state — exactly the E3-2
    // permanent-failure pattern: a dead step leaves the instance running
    // its remaining branches, and the DLQ entry is the operator's lever.
    // Conditional (idempotent): a token already Terminated by a prior
    // partial application of this effect is left alone, and the save and
    // the TokenFailed event below only run when THIS application performed
    // the transition (a re-drive must not duplicate them).
    let mut terminated_now = false;
    if let Some(token) = instance.tokens.iter_mut().find(|t| t.id == record.token_id) {
        if !matches!(
            token.status,
            TokenStatus::Completed | TokenStatus::Terminated
        ) {
            token.status = TokenStatus::Terminated;
            token.version += 1;
            token.updated_at = Some(occurred_at.clone());
            terminated_now = true;
        }
    }
    if terminated_now {
        instance.version += 1;
        state.storage.process_store.save(&instance).await?;
        // TokenFailed is recorded by the history handler — the history
        // explains the dead step (who rejected it, why it is dead-lettered).
        let mut ctx = build_ctx(state, tenant_id);
        state
            .engine
            .run_async(
                EngineEvent::TokenFailed(payloads::TokenFailed {
                    instance_id: record.instance_id.clone(),
                    token_id: record.token_id.clone(),
                    node_id: record.node_id.clone(),
                    reason: rejection_reason(record),
                }),
                &mut ctx,
            )
            .await;
    }
    Ok(())
}

/// The dead-letter message of a rejected approval step (names the actor).
fn rejection_reason(record: &bpm_engine_core::ApprovalRecord) -> String {
    match &record.note {
        Some(note) => format!(
            "approval rejected by {}: {}",
            record.decided_by.as_deref().unwrap_or("?"),
            note
        ),
        None => format!(
            "approval rejected by {}",
            record.decided_by.as_deref().unwrap_or("?")
        ),
    }
}

/// Insert the dead-letter entry of a rejected approval step (the reject
/// effect's durable record).
///
/// The entry id is DETERMINISTIC (`approval:<record id>`) so the insert is
/// idempotent: memory replaces by id, Postgres uses ON CONFLICT (id) DO
/// NOTHING — a re-drive of the effect never duplicates the entry. `task_id`
/// carries the APPROVAL record id — this step never had an external task
/// row (approval happens before creation). A DLQ requeue is the operator's
/// explicit override; the requeued task does NOT carry `__approval`, so the
/// worker-side gate stays fail-closed.
///
/// `variables` is the instance variable snapshot (empty when the instance
/// is gone).
async fn insert_rejection_dead_letter(
    state: &AppState,
    record: &bpm_engine_core::ApprovalRecord,
    tenant_id: Option<String>,
    variables: HashMap<String, String>,
    occurred_at: &str,
) -> anyhow::Result<()> {
    let dl_entry = bpm_engine_storage::DeadLetterEntry {
        id: format!("approval:{}", record.id),
        task_id: record.id.clone(),
        token_id: record.token_id.clone(),
        process_instance_id: record.instance_id.clone(),
        task_type: record.task_type.clone(),
        error_message: rejection_reason(record),
        variables: serde_json::to_string(&variables).unwrap_or_default(),
        tenant_id,
        created_at: occurred_at.to_string(),
    };
    state.storage.dead_letter_store.insert(&dl_entry).await?;
    Ok(())
}

#[derive(Serialize)]
pub struct InstanceLifecycleResponse {
    pub instance_id: String,
    /// `RUNNING` | `COMPLETED` | `TERMINATED`.
    pub status: String,
    /// Whether the instance currently has Suspended (paused) tokens.
    pub paused: bool,
}

fn lifecycle_response(instance: &ProcessInstance) -> InstanceLifecycleResponse {
    InstanceLifecycleResponse {
        instance_id: instance.id.clone(),
        status: status_str(instance.state).to_string(),
        paused: instance
            .tokens
            .iter()
            .any(|t| t.status == TokenStatus::Suspended),
    }
}

/// POST /api/v1/process-instances/:id/pause — suspend every non-terminal
/// token of the instance (operator, E5-2 / ADR-008 §5).
///
/// Idempotent: pausing an already-paused instance replays 200 without a
/// second `InstancePaused` history event; a terminal instance answers 409
/// `instance_not_pausable`. In-flight external-task leases are NOT
/// revoked — complete/fail are refused with 409 `instance_paused` until
/// resume (see `instance_task_op_gate`).
pub async fn pause_instance(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<InstanceLifecycleResponse>, (StatusCode, Json<ErrorResponse>)> {
    // The lifecycle mutex serializes pause/resume/cancel (single service):
    // check-then-act windows cannot interleave, so a concurrent double
    // pause/cancel produces exactly one effect (ADR-008 §7).
    let _guard = state.instance_lifecycle_lock.lock().await;
    let inst = state
        .storage
        .process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?;
    let Some(mut inst) = inst else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!(
                "process instance not found: {}",
                id
            ))),
        ));
    };
    if tenant_denied(&identity, &inst) {
        return Err(tenant_gate_not_found(&id));
    }
    if inst.state != InstanceState::Running {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse::with_code(
                format!(
                    "process instance {} is {} and cannot be paused",
                    id,
                    status_str(inst.state)
                ),
                error_codes::INSTANCE_NOT_PAUSABLE,
            )),
        ));
    }
    if inst
        .tokens
        .iter()
        .any(|t| t.status == TokenStatus::Suspended)
    {
        // Idempotent replay: already paused, no second history event.
        return Ok(Json(lifecycle_response(&inst)));
    }
    let occurred_at = occurred_at_now();
    let suspended = suspend_tokens(&mut inst, &occurred_at);
    if suspended == 0 {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse::with_code(
                format!("process instance {} has no active tokens to pause", id),
                error_codes::INSTANCE_NOT_PAUSABLE,
            )),
        ));
    }
    inst.version += 1;
    state
        .storage
        .process_store
        .save(&inst)
        .await
        .map_err(internal_error_tuple)?;
    let payload = serde_json::json!({
        "instance_id": inst.id,
        "actor": identity.actor(),
        "suspended_tokens": suspended,
    });
    let _ = HistoryRepo::append(
        state.storage.history_repo.as_ref(),
        &id,
        "InstancePaused",
        &payload,
        &occurred_at,
    )
    .await;
    info!(
        instance_id = %id,
        actor = identity.actor(),
        suspended = suspended,
        "process instance paused"
    );
    Ok(Json(lifecycle_response(&inst)))
}

/// Suspend every non-terminal token (operator-forced pause transition);
/// returns how many tokens were suspended.
fn suspend_tokens(inst: &mut ProcessInstance, occurred_at: &str) -> usize {
    let mut n = 0;
    for token in inst.tokens.iter_mut() {
        if matches!(
            token.status,
            TokenStatus::Created
                | TokenStatus::Ready
                | TokenStatus::Waiting
                | TokenStatus::Executing
        ) {
            token.status = TokenStatus::Suspended;
            token.version += 1;
            token.updated_at = Some(occurred_at.to_string());
            n += 1;
        }
    }
    n
}

/// POST /api/v1/process-instances/:id/resume — resume Suspended tokens
/// (operator, E5-2 / ADR-008 §5).
///
/// Resume restores the PRE-PAUSE semantics per node kind: tokens at
/// Waiting-steady-state nodes (timer/message/signal intermediate catch,
/// user task, external task, boundary event, call activity) return to
/// `Waiting` (the sanctioned `Suspended → Waiting` transition — without it
/// a resumed catch token could never fire, since the catch advance handlers
/// match Waiting); every other token flips to `Ready` (the sanctioned
/// `Suspended → Ready` transition).
///
/// Idempotent: resuming an unpaused instance is a 200 no-op; a terminal
/// instance answers 409 `instance_not_pausable`.
pub async fn resume_instance(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<InstanceLifecycleResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _guard = state.instance_lifecycle_lock.lock().await;
    let inst = state
        .storage
        .process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?;
    let Some(mut inst) = inst else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!(
                "process instance not found: {}",
                id
            ))),
        ));
    };
    if tenant_denied(&identity, &inst) {
        return Err(tenant_gate_not_found(&id));
    }
    if inst.state != InstanceState::Running {
        return Err((
            StatusCode::CONFLICT,
            Json(ErrorResponse::with_code(
                format!(
                    "process instance {} is {} and cannot be resumed",
                    id,
                    status_str(inst.state)
                ),
                error_codes::INSTANCE_NOT_PAUSABLE,
            )),
        ));
    }
    let occurred_at = occurred_at_now();
    // Definition for the per-node-kind resume semantics: WAITING-steady-
    // state nodes restore Waiting (their pre-pause status); everything else
    // resumes to Ready. A missing definition falls back to Ready (the
    // pre-E5.3 behavior — see ADR-008 §5).
    let def = state
        .storage
        .def_store
        .load(&inst.process_def_id)
        .await
        .ok()
        .flatten();
    let mut resumed = 0;
    for token in inst.tokens.iter_mut() {
        if token.status == TokenStatus::Suspended {
            // The sanctioned resume transition (version-bumped), per node
            // kind (ADR-008 §5): a token whose node's steady state is
            // Waiting (timer/message/signal intermediate catch, user task,
            // external task, boundary event, call activity) was Waiting
            // before the pause — resume restores EXACTLY that, or its catch
            // could never advance (the catch/handlers match Waiting). All
            // other tokens resume to Ready (the engine claims Ready
            // tokens, and their queued arrival processing continues).
            let restores_waiting = def.as_ref().is_some_and(|d| {
                d.nodes.get(token.node_id.as_str()).is_some_and(|n| {
                    matches!(
                        n.node_type,
                        NodeType::TimerIntermediateCatch { .. }
                            | NodeType::MessageIntermediateCatch { .. }
                            | NodeType::SignalIntermediateCatch { .. }
                            | NodeType::UserTask { .. }
                            | NodeType::ExternalTask { .. }
                            | NodeType::BoundaryTimer { .. }
                            | NodeType::BoundaryError { .. }
                            | NodeType::CallActivity { .. }
                    )
                })
            });
            token.status = if restores_waiting {
                TokenStatus::Waiting
            } else {
                TokenStatus::Ready
            };
            token.version += 1;
            token.updated_at = Some(occurred_at.to_string());
            resumed += 1;
        }
    }
    if resumed == 0 {
        // Idempotent no-op: nothing suspended (never paused, or already
        // resumed) — replay the current state.
        return Ok(Json(lifecycle_response(&inst)));
    }
    inst.version += 1;
    state
        .storage
        .process_store
        .save(&inst)
        .await
        .map_err(internal_error_tuple)?;
    let payload = serde_json::json!({
        "instance_id": inst.id,
        "actor": identity.actor(),
        "resumed_tokens": resumed,
    });
    let _ = HistoryRepo::append(
        state.storage.history_repo.as_ref(),
        &id,
        "InstanceResumed",
        &payload,
        &occurred_at,
    )
    .await;
    info!(instance_id = %id, actor = identity.actor(), resumed = resumed, "process instance resumed");
    Ok(Json(lifecycle_response(&inst)))
}

/// POST /api/v1/process-instances/:id/cancel — terminate the instance and
/// all its tokens (operator, E5-2 / ADR-008 §6), reusing the TerminateEnd
/// semantics: tokens → Terminated, instance → Terminated, a
/// `ProcessTerminated` engine event, and an `InstanceCancelled` history
/// entry with the actor.
///
/// Only-once: cancelling a Terminated instance replays 200 with the
/// terminal state (no second effect); a Completed instance replays 200
/// (no-op). Task operations on a cancelled instance are rejected with 409
/// `instance_cancelled`.
pub async fn cancel_instance(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<InstanceLifecycleResponse>, (StatusCode, Json<ErrorResponse>)> {
    let _guard = state.instance_lifecycle_lock.lock().await;
    let inst = state
        .storage
        .process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?;
    let Some(mut inst) = inst else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(format!(
                "process instance not found: {}",
                id
            ))),
        ));
    };
    if tenant_denied(&identity, &inst) {
        return Err(tenant_gate_not_found(&id));
    }
    // Only-once / no-op replay for terminal states.
    if inst.state == InstanceState::Terminated || inst.state == InstanceState::Completed {
        info!(instance_id = %id, actor = identity.actor(), state = %status_str(inst.state), "cancel replayed (terminal state)");
        return Ok(Json(lifecycle_response(&inst)));
    }
    let occurred_at = occurred_at_now();
    let mut terminated = 0;
    for token in inst.tokens.iter_mut() {
        if !matches!(
            token.status,
            TokenStatus::Completed | TokenStatus::Terminated
        ) {
            token.status = TokenStatus::Terminated;
            token.version += 1;
            token.updated_at = Some(occurred_at.to_string());
            terminated += 1;
        }
    }
    inst.state = InstanceState::Terminated;
    inst.version += 1;
    state
        .storage
        .process_store
        .save(&inst)
        .await
        .map_err(internal_error_tuple)?;
    let payload = serde_json::json!({
        "instance_id": inst.id,
        "actor": identity.actor(),
        "terminated_tokens": terminated,
    });
    let _ = HistoryRepo::append(
        state.storage.history_repo.as_ref(),
        &id,
        "InstanceCancelled",
        &payload,
        &occurred_at,
    )
    .await;
    info!(
        instance_id = %id,
        actor = identity.actor(),
        terminated = terminated,
        "process instance cancelled"
    );
    // Reuse the TerminateEnd engine path: the ProcessTerminated event is
    // recorded by the history handler.
    let tenant_id = tenant_from_identity(&identity);
    let mut ctx = build_ctx(state.as_ref(), tenant_id);
    state
        .engine
        .run_async(
            EngineEvent::ProcessTerminated(payloads::ProcessTerminated {
                instance_id: id.clone(),
            }),
            &mut ctx,
        )
        .await;
    Ok(Json(lifecycle_response(&inst)))
}

// --- Replay API (docs_replay_rest_api.md) ---

#[derive(Serialize)]
pub struct ReplayCreateResponse {
    pub session_id: String,
    pub instance_id: String,
    pub total_events: usize,
}

#[derive(Serialize)]
pub struct ReplayEventView {
    pub event_type: String,
    pub occurred_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
}

#[derive(Serialize)]
pub struct ReplayTokenView {
    pub token_id: String,
    pub node_id: String,
    pub state: String,
}

#[derive(Serialize)]
pub struct ReplaySnapshotView {
    pub completed: bool,
    pub tokens: Vec<ReplayTokenView>,
}

#[derive(Serialize)]
pub struct ReplayStepResponse {
    pub cursor: usize,
    pub event: ReplayEventView,
    pub snapshot: ReplaySnapshotView,
}

#[derive(Deserialize)]
pub struct ReplaySeekRequest {
    pub cursor: usize,
}

#[derive(Serialize)]
pub struct ReplaySeekResponse {
    pub cursor: usize,
    pub snapshot: ReplaySnapshotView,
}

#[derive(Serialize)]
pub struct ReplaySnapshotResponse {
    pub cursor: usize,
    pub total_events: usize,
    pub completed: bool,
    pub tokens: Vec<ReplayTokenView>,
}

fn replay_snapshot_from_instance(inst: Option<&ProcessInstance>) -> ReplaySnapshotView {
    let (completed, tokens) = match inst {
        Some(i) => (
            i.state == InstanceState::Completed,
            i.tokens
                .iter()
                .map(|t| ReplayTokenView {
                    token_id: t.id.clone(),
                    node_id: t.node_id.clone(),
                    state: token_status_str(&t.status).to_string(),
                })
                .collect(),
        ),
        None => (false, vec![]),
    };
    ReplaySnapshotView { completed, tokens }
}

/// POST /api/v1/process-instances/:id/replay — create replay session.
pub async fn create_replay(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<ReplayCreateResponse>), (StatusCode, Json<ErrorResponse>)> {
    // E2-4 review fix: storage ERROR → 500 internal; only a missing
    // instance is 404 not_found. E5-1: an instance of ANOTHER tenant is
    // reported like a missing one (no existence leak).
    let _inst = state
        .storage
        .process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| tenant_gate_not_found(&id))?;
    if tenant_denied(&identity, &_inst) {
        return Err(tenant_gate_not_found(&id));
    }
    let events =
        HistoryRepo::list_by_instance(state.storage.history_repo.as_ref(), &id, None, None)
            .await
            .map_err(internal_error_tuple)?;
    let session_id = uuid::Uuid::new_v4().to_string();
    // E5-1: the session is bound to the verified tenant (None for
    // open/API-key creators).
    let session_tenant = match &identity {
        Identity::Principal { tenant, .. } => Some(tenant.clone()),
        _ => None,
    };
    let session = ReplaySession::new(id.clone(), events, session_tenant);
    let total_events = session.total_events();
    {
        evict_expired(&state.replay_sessions);
        let mut guard = state.replay_sessions.write().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal("internal lock error")),
            )
        })?;
        guard.insert(session_id.clone(), session);
    }
    Ok((
        StatusCode::CREATED,
        Json(ReplayCreateResponse {
            session_id,
            instance_id: id,
            total_events,
        }),
    ))
}

/// Apply one event to a temporary repo and return new snapshot.
async fn replay_apply_one(
    engine: &bpm_engine_runtime::BpmEngine,
    def_store: &Arc<dyn ProcessDefinitionStore>,
    history_redaction_policy: HistoryRedactionPolicy,
    snapshot: Option<&ProcessInstance>,
    ev: &bpm_engine_storage::HistoryEvent,
) -> Option<ProcessInstance> {
    let engine_ev = ReplaySession::parse_event(ev)?;
    let instance_id = instance_id_from_event(&engine_ev)?;
    let replay_repo = Arc::new(MemoryRepo::new());
    if let Some(inst) = snapshot {
        let _ = replay_repo.save(inst).await;
    }
    let mut ctx = build_ctx_with_repo(
        replay_repo.clone(),
        Arc::clone(def_store),
        history_redaction_policy,
        None,
    );
    engine.run_async(engine_ev, &mut ctx).await;
    replay_repo.load(&instance_id).await.ok().flatten()
}

/// E5-1 replay-session gate: under a verified identity, only sessions the
/// SAME tenant created are usable — other tenants' sessions (and
/// admin/open-mode sessions) are reported like missing ones.
fn replay_session_denied(identity: &Identity, session: &ReplaySession) -> bool {
    match identity {
        Identity::Principal { tenant, .. } => session.tenant.as_deref() != Some(tenant.as_str()),
        _ => false,
    }
}

/// 404 `not_found` for an unknown (or tenant-gated) replay session.
fn replay_session_not_found() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse::not_found(
            "replay session not found or expired",
        )),
    )
}

/// POST /api/v1/replay/:session_id/step — step forward one event.
pub async fn replay_step(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(session_id): Path<String>,
) -> Result<Json<ReplayStepResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (ev_clone, snapshot_clone) = {
        let guard = state.replay_sessions.read().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal("internal lock error")),
            )
        })?;
        let session = guard
            .get(&session_id)
            .ok_or_else(replay_session_not_found)?;
        if replay_session_denied(&identity, session) {
            return Err(replay_session_not_found());
        }
        let ev = session
            .current_event()
            .ok_or_else(|| bad_request("no more events to step"))?;
        (ev.clone(), session.snapshot.clone())
    };
    let new_snapshot = replay_apply_one(
        &state.engine,
        &state.storage.def_store,
        state.history_redaction_policy,
        snapshot_clone.as_ref(),
        &ev_clone,
    )
    .await
    .ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::internal("replay apply failed")),
        )
    })?;
    let token_id = ev_clone
        .payload
        .get("token_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let node_id = ev_clone
        .payload
        .get("node_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let event_view = ReplayEventView {
        event_type: ev_clone.event_type.clone(),
        occurred_at: ev_clone.occurred_at.clone(),
        token_id,
        node_id,
    };
    let cursor = {
        let mut guard = state.replay_sessions.write().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal("internal lock error")),
            )
        })?;
        let session = guard.get_mut(&session_id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(
                    "replay session not found or expired",
                )),
            )
        })?;
        session.touch();
        session.snapshot = Some(new_snapshot.clone());
        session.cursor += 1;
        session.cursor
    };
    let snapshot_view = replay_snapshot_from_instance(Some(&new_snapshot));
    Ok(Json(ReplayStepResponse {
        cursor,
        event: event_view,
        snapshot: snapshot_view,
    }))
}

/// POST /api/v1/replay/:session_id/seek — jump to cursor (replay events[0..cursor]).
pub async fn replay_seek(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(session_id): Path<String>,
    ValidatedJson(body): ValidatedJson<ReplaySeekRequest>,
) -> Result<Json<ReplaySeekResponse>, (StatusCode, Json<ErrorResponse>)> {
    let events_to_apply: Vec<bpm_engine_storage::HistoryEvent> = {
        let guard = state.replay_sessions.read().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal("internal lock error")),
            )
        })?;
        let session = guard
            .get(&session_id)
            .ok_or_else(replay_session_not_found)?;
        if replay_session_denied(&identity, session) {
            return Err(replay_session_not_found());
        }
        let cursor = body.cursor.min(session.events.len());
        session.events[..cursor].to_vec()
    };
    let cursor = events_to_apply.len();
    let mut snapshot: Option<ProcessInstance> = None;
    for ev in &events_to_apply {
        if let Some(inst) = replay_apply_one(
            &state.engine,
            &state.storage.def_store,
            state.history_redaction_policy,
            snapshot.as_ref(),
            ev,
        )
        .await
        {
            snapshot = Some(inst);
        }
    }
    {
        if let Ok(mut guard) = state.replay_sessions.write() {
            if let Some(session) = guard.get_mut(&session_id) {
                session.touch();
                session.cursor = cursor;
                session.snapshot = snapshot.clone();
            }
        }
    }
    let snapshot_view = replay_snapshot_from_instance(snapshot.as_ref());
    Ok(Json(ReplaySeekResponse {
        cursor,
        snapshot: snapshot_view,
    }))
}

/// GET /api/v1/replay/:session_id/snapshot — read-only snapshot.
pub async fn get_replay_snapshot(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(session_id): Path<String>,
) -> Result<Json<ReplaySnapshotResponse>, (StatusCode, Json<ErrorResponse>)> {
    let guard = state.replay_sessions.read().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::internal("internal lock error")),
        )
    })?;
    let session = guard
        .get(&session_id)
        .ok_or_else(replay_session_not_found)?;
    if replay_session_denied(&identity, session) {
        return Err(replay_session_not_found());
    }
    let (completed, tokens) = match &session.snapshot {
        Some(i) => (
            i.state == InstanceState::Completed,
            i.tokens
                .iter()
                .map(|t| ReplayTokenView {
                    token_id: t.id.clone(),
                    node_id: t.node_id.clone(),
                    state: token_status_str(&t.status).to_string(),
                })
                .collect(),
        ),
        None => (false, vec![]),
    };
    Ok(Json(ReplaySnapshotResponse {
        cursor: session.cursor,
        total_events: session.total_events(),
        completed,
        tokens,
    }))
}

/// DELETE /api/v1/replay/:session_id — destroy session.
pub async fn delete_replay_session(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut guard = state.replay_sessions.write().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::internal("internal lock error")),
        )
    })?;
    // E5-1: a principal may only drop sessions its own tenant created.
    if let Some(session) = guard.get(&session_id) {
        if replay_session_denied(&identity, session) {
            return Err(replay_session_not_found());
        }
    }
    guard.remove(&session_id);
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/process-definitions/deploy — deploy a process definition from BPMN 2.0 XML.
/// On compile failure returns 400 with list of CompilerErrors (03.md).
///
/// E5-3: the deployer is logged with the verified actor. Definition
/// lifecycle actions have NO instance to attach history rows to (history
/// is instance-scoped, and the Postgres schema enforces that with a
/// foreign key), so their traceability is the server log — see
/// `docs/audit-and-retention.md` for the honest scope.
pub async fn deploy_bpmn(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    body: String,
) -> Result<(StatusCode, Json<DeployResponse>), (StatusCode, Json<DeployErrorResponse>)> {
    let def = match bpm_engine_bpmn::parse_and_compile(&body) {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(match e {
                    bpm_engine_bpmn::CompileError::Parse(parse_err) => DeployErrorResponse::Parse {
                        error: parse_err.to_string(),
                    },
                    bpm_engine_bpmn::CompileError::Compile(ce) => {
                        DeployErrorResponse::Compile { errors: ce.0 }
                    }
                }),
            ))
        }
    };
    let process_definition_id = def.id.to_string();
    if let Err(e) = state.storage.deployer.deploy(&def, &body).await {
        tracing::error!(error = %e, "process definition deploy failed");
        // E2-4 review fix: a persistence failure is a DISTINCT 500
        // response with code `deploy_failed` — no longer the Parse (400)
        // shape, which mislabeled server-side failures as client errors.
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DeployErrorResponse::Persist {
                error: "failed to persist process definition".to_string(),
                code: error_codes::DEPLOY_FAILED.to_string(),
            }),
        ));
    }
    info!(
        definition_id = %process_definition_id,
        actor = identity.actor(),
        "process definition deployed"
    );
    Ok((
        StatusCode::CREATED,
        Json(DeployResponse {
            process_definition_id,
        }),
    ))
}

// --- Static validation & dry-run (E2-2, issue #12) --------------------------

/// Source format discriminator for validate / dry-run requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceFormat {
    /// BPMN 2.0 XML.
    Bpmn,
    /// JSON DSL (E2-1).
    Dsl,
}

impl SourceFormat {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "bpmn" => Some(Self::Bpmn),
            "dsl" => Some(Self::Dsl),
            _ => None,
        }
    }
}

/// Request envelope for validate / dry-run: a definition source document
/// plus its format discriminator. `variables` is used by dry-run only.
#[derive(Deserialize)]
pub struct ValidateSourceRequest {
    /// Source format: `"bpmn"` (BPMN 2.0 XML) or `"dsl"` (JSON DSL, E2-1).
    pub format: String,
    /// Full source document as a string (XML or JSON, per `format`).
    pub source: String,
    /// Hypothetical start variables for the dry-run walk (validate ignores
    /// this field).
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

/// Compile-phase failures, returned as DATA with HTTP 200 so agents can read
/// and fix them. Shapes reuse the public DTOs of each compiler verbatim:
/// BPMN parse errors serialize as `{ "error": "..." }` (same shape as the
/// deploy endpoint), BPMN semantic errors and DSL errors as arrays.
#[derive(Serialize)]
#[serde(untagged)]
pub enum CompileErrorsView {
    /// BPMN XML parse failure (single message object).
    Parse {
        /// Parse error message.
        error: String,
    },
    /// BPMN semantic compile errors.
    Bpmn(Vec<bpm_engine_bpmn::CompilerError>),
    /// DSL structural/semantic compile errors (stable snake_case codes and
    /// JSON-pointer paths, see `docs/dsl-guide.md`).
    Dsl(Vec<bpm_engine_dsl::DslError>),
}

/// Response of `POST /api/v1/process-definitions/validate`: either the
/// compile-phase errors (`valid: false`, `compile_errors`) or the static
/// findings (`findings`; `valid` = no Critical finding).
#[derive(Serialize)]
pub struct ValidateResponse {
    /// False when compilation failed or a Critical finding exists.
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compile_errors: Option<CompileErrorsView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<bpm_engine_bpmn::ValidationFinding>>,
}

/// Response of `POST /api/v1/process-definitions/dry-run`: validate plus the
/// side-effect-free walk report.
#[derive(Serialize)]
pub struct DryRunResponse {
    /// False when compilation failed or a Critical finding exists.
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compile_errors: Option<CompileErrorsView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<Vec<bpm_engine_bpmn::ValidationFinding>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bpm_engine_bpmn::DryRunReport>,
}

/// Build a 400 response with a stable machine-readable code for a malformed
/// request envelope (bad format value, empty source, unparseable body).
fn bad_request_envelope(code: &str, message: String) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse::with_code(message, code)),
    )
}

/// Deserialize and envelope-check a validate / dry-run request. 4xx is
/// reserved for malformed envelopes; compile failures are 200 data.
fn parse_validate_request(
    body: &str,
) -> Result<(SourceFormat, ValidateSourceRequest), (StatusCode, Json<ErrorResponse>)> {
    let req: ValidateSourceRequest = serde_json::from_str(body).map_err(|e| {
        bad_request_envelope(
            "invalid_request_body",
            format!("invalid request body: {}", e),
        )
    })?;
    let format = SourceFormat::parse(&req.format).ok_or_else(|| {
        bad_request_envelope(
            "invalid_format",
            format!("format must be \"bpmn\" or \"dsl\", got {:?}", req.format),
        )
    })?;
    if req.source.trim().is_empty() {
        return Err(bad_request_envelope(
            "empty_source",
            "source must be a non-empty document string".to_string(),
        ));
    }
    Ok((format, req))
}

/// Compile a source document for validation / dry-run. Never persists
/// anything; compile failures are returned as data.
fn compile_for_validation(
    format: SourceFormat,
    source: &str,
) -> Result<ProcessDefinition, CompileErrorsView> {
    match format {
        SourceFormat::Bpmn => match bpm_engine_bpmn::parse_and_compile(source) {
            Ok(def) => Ok(def),
            Err(bpm_engine_bpmn::CompileError::Parse(e)) => Err(CompileErrorsView::Parse {
                error: e.to_string(),
            }),
            Err(bpm_engine_bpmn::CompileError::Compile(ce)) => Err(CompileErrorsView::Bpmn(ce.0)),
        },
        SourceFormat::Dsl => match bpm_engine_dsl::compile(source) {
            Ok(def) => Ok(def),
            Err(report) => Err(CompileErrorsView::Dsl(report.errors)),
        },
    }
}

/// POST /api/v1/process-definitions/validate — compile and statically
/// validate a definition WITHOUT deploying it (the E2-2 pre-deploy gate).
///
/// Takes no state on purpose: the handler is structurally side-effect-free.
/// Compile errors come back with HTTP 200 (`valid: false` +
/// `compile_errors`); a successful compile reports the findings of
/// `bpm_engine_bpmn::validate_definition`, and `valid` is true when no
/// finding is Critical (warnings/info included in the response).
pub async fn validate_process_definition(
    body: String,
) -> Result<Json<ValidateResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (format, req) = parse_validate_request(&body)?;
    match compile_for_validation(format, &req.source) {
        Err(compile_errors) => Ok(Json(ValidateResponse {
            valid: false,
            compile_errors: Some(compile_errors),
            findings: None,
        })),
        Ok(def) => {
            let findings = bpm_engine_bpmn::validate_definition(&def);
            let valid = !findings
                .iter()
                .any(|f| f.severity == bpm_engine_bpmn::Severity::Critical);
            info!(
                definition_id = %def.id,
                valid,
                findings = findings.len(),
                "process definition validated (pre-deploy, no persistence)"
            );
            Ok(Json(ValidateResponse {
                valid,
                compile_errors: None,
                findings: Some(findings),
            }))
        }
    }
}

/// POST /api/v1/process-definitions/dry-run — compile, validate and
/// side-effect-free simulate a definition with hypothetical start variables.
///
/// MUST NOT deploy and MUST NOT create any instance, task, timer or history
/// record (acceptance: dry-run 不产生实例). Same response contract as
/// validate plus a `dry_run` walk report.
pub async fn dry_run_process_definition(
    body: String,
) -> Result<Json<DryRunResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (format, req) = parse_validate_request(&body)?;
    match compile_for_validation(format, &req.source) {
        Err(compile_errors) => Ok(Json(DryRunResponse {
            valid: false,
            compile_errors: Some(compile_errors),
            findings: None,
            dry_run: None,
        })),
        Ok(def) => {
            let findings = bpm_engine_bpmn::validate_definition(&def);
            let valid = !findings
                .iter()
                .any(|f| f.severity == bpm_engine_bpmn::Severity::Critical);
            // Pure function over the compiled definition: no stores touched.
            let report =
                bpm_engine_bpmn::dry_run(&def, &req.variables, bpm_engine_bpmn::DEFAULT_MAX_STEPS);
            info!(
                definition_id = %def.id,
                valid,
                steps = report.steps.len(),
                "process definition dry-run (no persistence)"
            );
            Ok(Json(DryRunResponse {
                valid,
                compile_errors: None,
                findings: Some(findings),
                dry_run: Some(report),
            }))
        }
    }
}

// --- Process Definition Version Management ---

#[derive(Serialize)]
pub struct DefinitionVersionView {
    pub id: String,
    pub key: String,
    pub version: u32,
    pub status: String,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct VersionListResponse {
    pub key: String,
    pub versions: Vec<DefinitionVersionView>,
}

#[derive(Serialize)]
pub struct ActivateResponse {
    pub id: String,
    pub status: String,
}

#[derive(Serialize)]
pub struct DefinitionListResponse {
    pub definitions: Vec<DefinitionVersionView>,
}

/// GET /api/v1/process-definitions — list every deployed process definition version.
pub async fn list_process_definitions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<DefinitionListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let records = state
        .storage
        .def_store
        .list_all()
        .await
        .map_err(internal_error_tuple)?;
    Ok(Json(DefinitionListResponse {
        definitions: records
            .into_iter()
            .map(|record| DefinitionVersionView {
                id: record.id,
                key: record.key,
                version: record.version,
                status: record.status.to_string(),
                created_at: record.created_at,
            })
            .collect(),
    }))
}

/// GET /api/v1/process-definitions/versions/:key — list all versions for a key.
pub async fn list_definition_versions(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<VersionListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let versions = state
        .storage
        .def_store
        .list_versions(&key)
        .await
        .map_err(internal_error_tuple)?;
    Ok(Json(VersionListResponse {
        key,
        versions: versions
            .into_iter()
            .map(|v| DefinitionVersionView {
                id: v.id,
                key: v.key,
                version: v.version,
                status: v.status.to_string(),
                created_at: v.created_at,
            })
            .collect(),
    }))
}

/// GET /api/v1/process-definitions/active/:key — get the active version for a key.
pub async fn get_active_definition(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<DefinitionVersionView>, (StatusCode, Json<ErrorResponse>)> {
    let record = state
        .storage
        .def_store
        .get_active(&key)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "no active version found for key: {}",
                    key
                ))),
            )
        })?;
    Ok(Json(DefinitionVersionView {
        id: record.id,
        key: record.key,
        version: record.version,
        status: record.status.to_string(),
        created_at: record.created_at,
    }))
}

/// PUT /api/v1/process-definitions/:id/activate — set a version as active.
///
/// E5-3: the operator is logged with the verified actor (definition
/// lifecycle actions carry no instance-scoped history rows — see
/// `docs/audit-and-retention.md`).
pub async fn activate_definition(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<ActivateResponse>, (StatusCode, Json<ErrorResponse>)> {
    state
        .storage
        .def_store
        .activate(&id)
        .await
        .map_err(internal_error_tuple)?;
    info!(definition_id = %id, actor = identity.actor(), "process definition activated");
    Ok(Json(ActivateResponse {
        id,
        status: "active".to_string(),
    }))
}

/// PUT /api/v1/process-definitions/:id/deprecate — mark a version as deprecated.
///
/// E5-3: the operator is logged with the verified actor.
pub async fn deprecate_definition(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<ActivateResponse>, (StatusCode, Json<ErrorResponse>)> {
    state
        .storage
        .def_store
        .deprecate(&id)
        .await
        .map_err(internal_error_tuple)?;
    info!(definition_id = %id, actor = identity.actor(), "process definition deprecated");
    Ok(Json(ActivateResponse {
        id,
        status: "deprecated".to_string(),
    }))
}

// --- Health check endpoints ---

/// GET /health - liveness probe. Always returns 200 if the process is up.
pub async fn health() -> Json<super::state::HealthResponse> {
    Json(super::state::HealthResponse {
        status: "ok".to_string(),
    })
}

/// GET /capabilities — machine-readable agent tool catalog (E4-1,
/// issue #19).
///
/// Serves the COMMITTED catalog artifact
/// (`crates/server/rest/capabilities/agent-tools.json`, embedded with
/// `include_str!` at compile time): one tool per operation of
/// `docs/openapi.yaml` with parameter schemas, read-only/mutating
/// classification, side-effect notes, and error-code examples — see
/// `docs/agent-tools.md`.
///
/// Served UNAUTHENTICATED, like `/health` and `/ready`, and by the same
/// top-level registration: the catalog is DISCOVERY metadata for agents
/// BEFORE they hold an API key, and it carries no secrets — only route
/// shapes and stable error codes derived from the public spec. The
/// `/api/v1` routes (the actual engine operations) keep the
/// api-key + rate-limit middleware; this route stays outside it. The
/// consistency suite's lockstep test fails when the artifact drifts
/// from the spec, so what this serves is always the current contract.
///
/// The body is the exact committed JSON bytes (no runtime YAML
/// parsing, no new runtime dependencies); the content type is
/// `application/json`.
pub async fn capabilities() -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        crate::capabilities::AGENT_TOOLS_JSON,
    )
}

/// GET /ready - readiness probe. Checks engine availability and the storage backend.
pub async fn readiness(
    State(state): State<Arc<AppState>>,
) -> Json<super::state::ReadinessResponse> {
    let mut checks = std::collections::HashMap::new();

    // Check engine is accessible (always ok in current impl)
    checks.insert("engine".to_string(), "ok".to_string());

    // Check the storage backend is accessible by attempting a lightweight operation
    let store_ok = state.storage.process_store.list_running(None).await.is_ok();
    checks.insert(
        "database".to_string(),
        if store_ok { "ok" } else { "unavailable" }.to_string(),
    );

    // Merge extra health checks (e.g. postgres pool status)
    if let Some(ref extra) = state.extra_health_checks {
        checks.extend(extra());
    }

    // E5-3: retention readiness. "ok" = retention is ON and the last sweep
    // COMPLETED (last_sweep is set only on a fully successful run — the
    // startup sweep runs before serving, so by the time /ready answers a
    // None means the sweep errored: a persistently failing retention sweep
    // reports "degraded" instead of green, which is the correct compliance
    // signal). Reported only when retention is ON — the default (0 days) is
    // "keep everything", which needs no readiness signal.
    if state.retention.retention_days > 0 {
        let sweep_ok = state
            .retention
            .last_sweep
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false);
        checks.insert(
            "audit_retention".to_string(),
            if sweep_ok { "ok" } else { "degraded" }.to_string(),
        );
    }

    // Informational metrics (e.g. db_pool_active) are numeric; they report
    // state, not pass/fail, and do not affect the overall status.
    let all_ok = checks
        .values()
        .all(|v| v == "ok" || v.parse::<u64>().is_ok());
    let status = if all_ok { "ok" } else { "degraded" }.to_string();

    Json(super::state::ReadinessResponse { status, checks })
}

// ---------------------------------------------------------------------------
// Dead Letter Queue endpoints
// ---------------------------------------------------------------------------

/// E5-1 dead-letter gate: under a verified identity, only entries of the
/// claim tenant are visible — other tenants' (and untagged/admin) entries
/// are reported like missing ones (the same 404 body, no existence leak).
fn dead_letter_denied(identity: &Identity, entry: &bpm_engine_storage::DeadLetterEntry) -> bool {
    match identity {
        Identity::Principal { tenant, .. } => entry.tenant_id.as_deref() != Some(tenant.as_str()),
        _ => false,
    }
}

/// 404 `not_found` for an unknown (or tenant-gated) dead-letter entry.
fn dead_letter_not_found() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse::not_found("dead letter not found")),
    )
}

/// GET /api/v1/dead-letters — list dead letter entries.
///
/// Optional route-level pagination (E2-4): `limit`/`offset` query
/// parameters (validated like the instances endpoint: `limit` 1..=500,
/// `offset` ≥ 0, else 400 `invalid_request`) slice the fetched entries IN
/// MEMORY. Absent parameters keep the previous behavior. The store is not
/// extended: the fetch window stays bounded by the store's listing cap,
/// which the memory and Postgres adapters enforce.
async fn list_dead_letters(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let page = if params.contains_key("limit") || params.contains_key("offset") {
        Some(parse_page_params(&params)?)
    } else {
        None
    };
    let tenant_id = tenant_from_identity(&identity);
    let mut entries = state
        .storage
        .dead_letter_store
        .list(tenant_id.as_deref(), 100)
        .await
        .map_err(internal_error_tuple)?;
    if let Some(page) = page {
        let offset = (page.offset as usize).min(entries.len());
        entries.drain(..offset);
        entries.truncate(page.limit as usize);
    }
    Ok(Json(serde_json::json!({ "entries": entries })))
}

/// GET /api/v1/dead-letters/:id — get a single dead letter entry.
async fn get_dead_letter(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let entry = state
        .storage
        .dead_letter_store
        .get(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(dead_letter_not_found)?;
    if dead_letter_denied(&identity, &entry) {
        return Err(dead_letter_not_found());
    }
    Ok(Json(serde_json::json!({ "entry": entry })))
}

/// POST /api/v1/dead-letters/:id/requeue — requeue a dead letter entry back as an external task.
async fn requeue_dead_letter(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    // E5-1 gate before the mutation (requeue removes the entry).
    let entry = state
        .storage
        .dead_letter_store
        .get(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(dead_letter_not_found)?;
    if dead_letter_denied(&identity, &entry) {
        return Err(dead_letter_not_found());
    }
    let task_id = state
        .storage
        .dead_letter_store
        .requeue(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(dead_letter_not_found)?;
    // E5-3 boundary event: WHO requeued the dead-letter entry (the DLQ
    // requeue is an operator action; its history lands on the instance the
    // entry belongs to).
    {
        let payload = serde_json::json!({
            "instance_id": entry.process_instance_id,
            "dead_letter_id": id,
            "task_id": task_id,
            "actor": identity.actor(),
        });
        let _ = HistoryRepo::append(
            state.storage.history_repo.as_ref(),
            &entry.process_instance_id,
            "DeadLetterRequeued",
            &payload,
            &occurred_at_now(),
        )
        .await;
    }
    info!(dead_letter_id = %id, actor = identity.actor(), "dead letter requeued");
    Ok(Json(
        serde_json::json!({ "status": "requeued", "task_id": task_id }),
    ))
}

/// DELETE /api/v1/dead-letters/:id — delete a dead letter entry.
async fn delete_dead_letter(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    // E5-1 gate before the mutation (delete is irreversible).
    let entry = state
        .storage
        .dead_letter_store
        .get(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(dead_letter_not_found)?;
    if dead_letter_denied(&identity, &entry) {
        return Err(dead_letter_not_found());
    }
    state
        .storage
        .dead_letter_store
        .delete(&id)
        .await
        .map_err(internal_error_tuple)?;
    // E5-3 boundary event: WHO deleted the dead-letter entry (irreversible
    // operator action).
    {
        let payload = serde_json::json!({
            "instance_id": entry.process_instance_id,
            "dead_letter_id": id,
            "actor": identity.actor(),
        });
        let _ = HistoryRepo::append(
            state.storage.history_repo.as_ref(),
            &entry.process_instance_id,
            "DeadLetterDeleted",
            &payload,
            &occurred_at_now(),
        )
        .await;
    }
    info!(dead_letter_id = %id, actor = identity.actor(), "dead letter deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Run invariant checks and return the results.
///
/// Use for health checks, debugging, or post-crash recovery verification.
async fn invariant_check(
    State(state): State<Arc<AppState>>,
) -> Result<Json<bpm_engine_storage::InvariantCheckResult>, (StatusCode, Json<ErrorResponse>)> {
    state
        .storage
        .invariant_checker
        .check_all()
        .await
        .map(Json)
        .map_err(internal_error_tuple)
}

// --- Route registration registry (E2-4 review fix) ---------------------------
//
// The OpenAPI route-consistency contract must compare the spec against
// the routes the router ACTUALLY registers — not a hand-maintained table
// that can drift from the real `.route(...)` calls. Every registration
// therefore goes through the [`root_route!`]/[`api_route!`] macros
// below, which record `(HTTP method, full wire path)` into a thread-local
// registry as a side effect of building the router.
// [`registered_routes`] derives the table by constructing the router once
// (cheap: no state value, no I/O, handlers never invoked).

thread_local! {
    /// Routes registered by the most recent router build, in
    /// registration order: `(HTTP method, full wire path)`.
    static REGISTERED_ROUTES: std::cell::RefCell<Vec<(&'static str, &'static str)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record a route registration (called by the route macros).
fn track_route(method: &'static str, path: &'static str) {
    REGISTERED_ROUTES.with(|cell| cell.borrow_mut().push((method, path)));
}

/// Start a fresh registration; the registry always describes exactly one
/// full router build.
fn reset_route_registry() {
    REGISTERED_ROUTES.with(|cell| cell.borrow_mut().clear());
}

/// Build a single-method [`MethodRouter`] — the one place HTTP method
/// names become axum method routers. An unknown method panics loudly; it
/// can only come from a bug in the route macros below.
fn method_router<S, H, T>(method: &'static str, handler: H) -> MethodRouter<S>
where
    H: axum::handler::Handler<T, S>,
    T: 'static,
    S: Clone + Send + Sync + 'static,
{
    match method {
        "GET" => axum::routing::get(handler),
        "POST" => axum::routing::post(handler),
        "PUT" => axum::routing::put(handler),
        "DELETE" => axum::routing::delete(handler),
        other => panic!("unsupported HTTP method in route registration: {other}"),
    }
}

/// Register a route on the top-level router and record
/// `(METHOD, path)` in the route registry. `$path` is the full wire path.
macro_rules! root_route {
    ($router:expr, $method:ident, $path:literal, $handler:expr) => {{
        track_route(stringify!($method), $path);
        $router.route($path, method_router(stringify!($method), $handler))
    }};
}

/// Register a route inside the `/api/v1` nest and record its FULL wire
/// path (`/api/v1` + `$path`) in the route registry. The path literal is
/// used exactly once, so the registry cannot drift from the actual
/// registration.
macro_rules! api_route {
    // One method on the path.
    ($router:expr, $method:ident, $path:literal, $handler:expr) => {{
        track_route(stringify!($method), concat!("/api/v1", $path));
        $router.route($path, method_router(stringify!($method), $handler))
    }};
    // Two methods on the same path (e.g. GET + POST /process-instances).
    ($router:expr, $method1:ident, $method2:ident, $path:literal, $handler1:expr, $handler2:expr) => {{
        track_route(stringify!($method1), concat!("/api/v1", $path));
        track_route(stringify!($method2), concat!("/api/v1", $path));
        $router.route(
            $path,
            method_router(stringify!($method1), $handler1)
                .merge(method_router(stringify!($method2), $handler2)),
        )
    }};
}

/// Every `(HTTP method, full wire path)` the current build of [`router`]
/// registers — DERIVED at registration time (the route macros record into
/// the registry while the router is built), not hand-maintained.
///
/// This is the single source of truth the OpenAPI consistency suite
/// (`crates/server/rest/tests/openapi_contract.rs`) compares against the
/// paths/operations of `docs/openapi.yaml` in BOTH directions:
///
/// - a route cannot be registered without spec coverage, and
/// - the spec cannot document a route that does not exist.
///
/// Construction needs no state value and performs no I/O: the handlers
/// are generic over the state type and are never invoked, so the router
/// is simply built and dropped. `GET /metrics` is recorded only under the
/// `observability` build feature and is therefore not part of the default
/// wire contract (documented in `docs/openapi.yaml`).
pub fn registered_routes() -> Vec<(&'static str, &'static str)> {
    let rate_limiter = std::sync::Arc::new(tokio::sync::Mutex::new(
        middleware::RateLimiterState::new(0, 60),
    ));
    let _probe: Router<Arc<AppState>> = base_router().nest("/api/v1", api_v1_router(rate_limiter));
    REGISTERED_ROUTES.with(|cell| cell.borrow().clone())
}

/// Top-level routes (outside the `/api/v1` nest). Also resets the route
/// registry: each build starts from a clean slate, so the registry always
/// mirrors the most recent [`router`] (or [`registered_routes`]) build.
fn base_router() -> Router<Arc<AppState>> {
    reset_route_registry();
    let router = Router::new();
    let router = root_route!(router, GET, "/health", health);
    let router = root_route!(router, GET, "/ready", readiness);
    // Agent tool discovery (E4-1): unauthenticated like /health and
    // /ready — see the `capabilities` handler docs for the rationale.
    let router = root_route!(router, GET, "/capabilities", capabilities);

    // Add /metrics endpoint when observability feature is enabled
    #[cfg(feature = "observability")]
    let router = root_route!(router, GET, "/metrics", super::metrics::metrics_handler);

    router
}

/// The versioned `/api/v1` routes, with authentication (JWT/API key/open,
/// E5-1) and rate limiting applied exactly as before (same layer order —
/// the layers wrap the nested router, so they cover the API routes only,
/// while the request-id middleware on the outer router covers everything).
fn api_v1_router(
    rate_limiter: std::sync::Arc<tokio::sync::Mutex<middleware::RateLimiterState>>,
) -> Router<Arc<AppState>> {
    let router = Router::new();
    let router = api_route!(
        router,
        GET,
        POST,
        "/process-instances",
        list_process_instances,
        start_instance
    );
    let router = api_route!(router, GET, "/process-instances/:id", get_instance);
    let router = api_route!(
        router,
        GET,
        "/process-instances/:id/trace",
        get_instance_trace
    );
    let router = api_route!(
        router,
        GET,
        "/process-instances/:id/history",
        get_instance_history
    );
    let router = api_route!(
        router,
        GET,
        "/process-instances/:id/steps",
        get_instance_steps
    );
    // Audit view (E5-3, issue #25): actor-attributed timeline + decisions
    // + artifact references. A read — all authenticated roles.
    let router = api_route!(
        router,
        GET,
        "/process-instances/:id/audit",
        get_instance_audit
    );
    let router = api_route!(router, POST, "/process-instances/:id/replay", create_replay);
    let router = api_route!(router, POST, "/replay/:session_id/step", replay_step);
    let router = api_route!(router, POST, "/replay/:session_id/seek", replay_seek);
    let router = api_route!(
        router,
        GET,
        "/replay/:session_id/snapshot",
        get_replay_snapshot
    );
    let router = api_route!(router, DELETE, "/replay/:session_id", delete_replay_session);
    let router = api_route!(
        router,
        GET,
        "/process-definitions/:id",
        get_process_definition
    );
    let router = api_route!(
        router,
        GET,
        "/process-definitions",
        list_process_definitions
    );
    let router = api_route!(
        router,
        GET,
        "/process-definitions/versions/:key",
        list_definition_versions
    );
    let router = api_route!(
        router,
        GET,
        "/process-definitions/active/:key",
        get_active_definition
    );
    let router = api_route!(
        router,
        PUT,
        "/process-definitions/:id/activate",
        activate_definition
    );
    let router = api_route!(
        router,
        PUT,
        "/process-definitions/:id/deprecate",
        deprecate_definition
    );
    let router = api_route!(router, GET, "/tasks", list_tasks);
    let router = api_route!(router, GET, "/tasks/:task_id/form", get_task_form);
    let router = api_route!(
        router,
        POST,
        "/tasks/:task_id/complete",
        complete_task_by_id
    );
    let router = api_route!(
        router,
        POST,
        "/external-tasks/fetch-and-lock",
        external_task_fetch_and_lock
    );
    let router = api_route!(
        router,
        POST,
        "/external-tasks/:task_id/complete",
        external_task_complete
    );
    let router = api_route!(
        router,
        POST,
        "/external-tasks/:task_id/fail",
        external_task_fail
    );
    let router = api_route!(
        router,
        POST,
        "/external-tasks/:task_id/extend-lock",
        external_task_extend_lock
    );
    // Approval & instance lifecycle (E5-2, ADR-008).
    let router = api_route!(
        router,
        GET,
        "/process-instances/:id/approvals",
        list_instance_approvals
    );
    let router = api_route!(
        router,
        POST,
        "/approvals/:approval_id/decide",
        approval_decide
    );
    let router = api_route!(router, POST, "/process-instances/:id/pause", pause_instance);
    let router = api_route!(
        router,
        POST,
        "/process-instances/:id/resume",
        resume_instance
    );
    let router = api_route!(
        router,
        POST,
        "/process-instances/:id/cancel",
        cancel_instance
    );
    let router = api_route!(router, POST, "/process-definitions/deploy", deploy_bpmn);
    let router = api_route!(
        router,
        POST,
        "/process-definitions/validate",
        validate_process_definition
    );
    let router = api_route!(
        router,
        POST,
        "/process-definitions/dry-run",
        dry_run_process_definition
    );
    let router = api_route!(router, GET, "/dead-letters", list_dead_letters);
    let router = api_route!(router, GET, "/dead-letters/:id", get_dead_letter);
    let router = api_route!(
        router,
        POST,
        "/dead-letters/:id/requeue",
        requeue_dead_letter
    );
    let router = api_route!(router, DELETE, "/dead-letters/:id", delete_dead_letter);
    let router = api_route!(router, GET, "/invariants/check", invariant_check);
    router
        .layer(axum_mw::from_fn(middleware::authenticate))
        .layer(axum_mw::from_fn_with_state(
            rate_limiter,
            middleware::rate_limit,
        ))
}

pub fn router(state: Arc<AppState>, rate_limit_rpm: u64) -> Router {
    let rate_limiter = std::sync::Arc::new(tokio::sync::Mutex::new(
        middleware::RateLimiterState::new(rate_limit_rpm, 60),
    ));

    base_router()
        .nest("/api/v1", api_v1_router(rate_limiter))
        .with_state(state)
        .layer(axum_mw::from_fn(middleware::request_id_middleware))
}

#[cfg(test)]
mod tests {
    use super::error_codes;

    /// E2-4 review fix: the source-literal scan of the OpenAPI contract
    /// suite cannot see codes referenced through the `error_codes` consts
    /// (handlers mostly use `error_codes::NOT_FOUND` etc.). This test
    /// names EVERY public const explicitly, so a const whose value is NOT
    /// registered in `ERROR_CODES` — and therefore missing from the
    /// `ErrorResponse.code` enum of `docs/openapi.yaml` — fails here.
    /// When adding a const, extend this list; the
    /// `error_codes_identifiers_used_in_routes_are_declared_consts` scan
    /// of the contract suite keeps the two views in sync.
    #[test]
    fn every_error_code_const_is_registered() {
        for code in [
            error_codes::NOT_FOUND,
            error_codes::UNAUTHORIZED,
            error_codes::INVALID_TOKEN,
            error_codes::FORBIDDEN,
            error_codes::RATE_LIMITED,
            error_codes::INVALID_REQUEST,
            error_codes::INVALID_JSON,
            error_codes::INTERNAL,
            error_codes::DEPLOY_FAILED,
            error_codes::TASK_NOT_FOUND,
            error_codes::TASK_NOT_LOCKED,
            error_codes::LOCK_EXPIRED,
            error_codes::INVARIANT_VIOLATION,
            error_codes::LOCK_CONFLICT,
            error_codes::APPROVAL_NOT_FOUND,
            error_codes::APPROVAL_ALREADY_DECIDED,
            error_codes::INSTANCE_PAUSED,
            error_codes::INSTANCE_CANCELLED,
            error_codes::INSTANCE_NOT_PAUSABLE,
        ] {
            assert!(
                error_codes::ERROR_CODES.contains(&code),
                "error-code const {code:?} must be registered in ERROR_CODES \
                 (and mirrored in the docs/openapi.yaml ErrorResponse.code enum)"
            );
        }
    }

    /// The route registry is derived at registration time: constructing
    /// the router populates it, every entry is unique, and the known
    /// anchors are present (the OpenAPI contract suite does the full
    /// spec ↔ registration comparison).
    #[test]
    fn route_registry_is_derived_and_duplicate_free() {
        let routes = super::registered_routes();
        assert!(
            routes.contains(&("GET", "/health")),
            "the registry must derive from the actual registrations"
        );
        assert!(
            routes.contains(&("POST", "/api/v1/process-instances")),
            "dual-method routes must record every method"
        );
        assert!(
            routes.contains(&("GET", "/api/v1/process-instances")),
            "dual-method routes must record every method"
        );
        let mut sorted = routes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            routes.len(),
            "duplicate route registrations: {routes:?}"
        );
        assert!(
            routes.iter().all(|(_, p)| p.starts_with('/')),
            "registry paths must be absolute wire paths: {routes:?}"
        );
    }

    // --- Audit helpers (E5-3) ---------------------------------------------------

    #[test]
    fn audit_definition_id_version_parsing_matches_the_stores() {
        use super::parse_definition_id_version;
        assert_eq!(
            parse_definition_id_version("order-flow:3"),
            ("order-flow".to_string(), 3)
        );
        // Colonless id → version 1 (the stores' rule).
        assert_eq!(
            parse_definition_id_version("audit-approval-flow"),
            ("audit-approval-flow".to_string(), 1)
        );
        // A NON-numeric version segment is not a version: the whole id is
        // the key (the stores' fallback rule).
        assert_eq!(
            parse_definition_id_version("weird:key:notnum"),
            ("weird:key:notnum".to_string(), 1)
        );
    }

    #[test]
    fn audit_event_actor_reads_only_nonempty_actors() {
        use super::audit_event_actor;
        assert_eq!(
            audit_event_actor(&serde_json::json!({ "actor": "alice" })),
            Some("alice".to_string())
        );
        assert_eq!(
            audit_event_actor(&serde_json::json!({ "actor": null })),
            None,
            "a null actor (e.g. an undecided record) stays absent"
        );
        assert_eq!(
            audit_event_actor(&serde_json::json!({ "actor": "" })),
            None,
            "an empty actor is not an attribution"
        );
        assert_eq!(
            audit_event_actor(&serde_json::json!({ "token_id": "t1" })),
            None,
            "engine-internal events carry no actor"
        );
    }

    #[test]
    fn audit_artifact_extraction_parses_envelopes_and_dedups() {
        use super::collect_artifact_refs;
        use std::collections::HashSet;
        let sha = "a".repeat(64);
        let mut variables = serde_json::Map::new();
        variables.insert(
            "report".to_string(),
            serde_json::Value::String(format!(
                r#"__var:1:{{"t":"artifact","uri":"https://x.example.com/r1.md","sha256":"{sha}","size":4096}}"#
            )),
        );
        // The SAME reference seen again (a later event) — deduped.
        variables.insert(
            "report_copy".to_string(),
            serde_json::Value::String(format!(
                r#"__var:1:{{"t":"artifact","uri":"https://x.example.com/r1.md","sha256":"{sha}","size":4096}}"#
            )),
        );
        // A json envelope is NOT an artifact reference.
        variables.insert(
            "data".to_string(),
            serde_json::Value::String(r#"__var:1:{"t":"json","val":1}"#.to_string()),
        );
        // Redacted / encrypted / plain values contribute nothing.
        variables.insert(
            "redacted".to_string(),
            serde_json::Value::String("__truncated:5000:0123456789ab:".to_string()),
        );
        variables.insert(
            "secret".to_string(),
            serde_json::Value::String("__encrypted:QUFB:QkJC".to_string()),
        );
        variables.insert(
            "plain".to_string(),
            serde_json::Value::String("hello".to_string()),
        );

        let mut out = Vec::new();
        let mut seen = HashSet::new();
        collect_artifact_refs(&variables, "history", &mut out, &mut seen);
        // Both variables reference the same URI but under DIFFERENT
        // names: both are listed (the dedup key is (uri, variable)).
        assert_eq!(out.len(), 2, "{out:?}");
        for a in &out {
            assert_eq!(a.uri, "https://x.example.com/r1.md");
            assert_eq!(a.sha256.as_deref(), Some(sha.as_str()));
            assert_eq!(a.size, Some(4096));
            assert!(a.media_type.is_none());
            assert_eq!(a.source, "history");
        }
        // Re-collecting the same map adds nothing (idempotent dedup).
        collect_artifact_refs(&variables, "instance", &mut out, &mut seen);
        assert_eq!(out.len(), 2, "{out:?}");
    }
}
