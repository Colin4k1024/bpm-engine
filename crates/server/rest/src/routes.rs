//! REST API v1 routes: process-instances, tasks, external-tasks.

use axum::{
    async_trait,
    extract::{rejection::JsonRejection, FromRequest, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware as axum_mw,
    response::{IntoResponse, Response},
    routing::MethodRouter,
    Json, Router,
};
use bpm_engine_core::{
    parse_envelope, payloads, redact_history_variable_value, validate_artifact_uri,
    AesGcmEncryptor, AiTaskEnvelope, EngineEvent, ExternalTaskState, HistoryRedactionPolicy,
    InstanceState, NodeType, ProcessDefinition, ProcessInstance, RetryCategory, TokenStatus,
    VariableEncryptor, VariableEnvelope, AI_TASK_ENVELOPE_KEY, ENCRYPTED_PREFIX,
};
use bpm_engine_runtime::EngineContext;
use bpm_engine_storage::{
    CompensationRecordRepo, ExternalTaskStore, HistoryRepo, ParallelJoinRepo,
    ProcessDefinitionStore, ProcessInstanceStore, TimerStore, TokenStore,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use super::middleware;
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
    ];

    /// 404 — unknown resource id.
    pub const NOT_FOUND: &str = "not_found";
    /// 401 — missing or invalid API key.
    pub const UNAUTHORIZED: &str = "unauthorized";
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
/// row. The bound applies ONLY at this save boundary: `X-Tenant-Id` keeps
/// its unbounded, pass-through semantics on every other endpoint (see
/// [`tenant_from_headers`]).
const MAX_IDEMPOTENCY_TENANT_BYTES: usize = 128;

/// Validate the `X-Tenant-Id` of an idempotent start-instance request:
/// when an `Idempotency-Key` is supplied, the co-stored tenant id must be
/// at most [`MAX_IDEMPOTENCY_TENANT_BYTES`] printable ASCII bytes, else
/// 400 `invalid_request` ("tenant id too long"). An absent tenant id is
/// fine (idempotency keys work without a tenant scope).
fn tenant_id_for_idempotency(
    tenant: Option<String>,
) -> Result<Option<String>, (StatusCode, Json<ErrorResponse>)> {
    let Some(id) = tenant else {
        return Ok(None);
    };
    if id.len() > MAX_IDEMPOTENCY_TENANT_BYTES || !id.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(bad_request(format!(
            "tenant id too long: X-Tenant-Id must be at most {} printable ASCII bytes \
             when an Idempotency-Key is supplied",
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
    /// Optional retry category (E3-1): `"transient"` or `"permanent"`.
    /// Recorded for observability (error message prefix, envelope,
    /// history, DLQ) — retry BEHAVIOR is unchanged in v1 (E3-2 scope).
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

fn tenant_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
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
    .history_repo(repo as Arc<dyn HistoryRepo>)
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
    ValidatedJson(body): ValidatedJson<StartInstanceRequest>,
) -> Result<(StatusCode, Json<StartInstanceResponse>), (StatusCode, Json<ErrorResponse>)> {
    let tenant_id = tenant_from_headers(&headers);
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
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<InstanceListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let page = parse_page_params(&params)?;
    let tenant_id = tenant_from_headers(&headers);
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
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<InstanceStateResponse>, (StatusCode, Json<ErrorResponse>)> {
    let tenant_id = tenant_from_headers(&headers);
    let _ctx = build_ctx(state.as_ref(), tenant_id);
    let process_store = Arc::clone(&state.storage.process_store);
    // E2-4 review fix: a storage ERROR is 500 `internal`, only a missing
    // instance is 404 `not_found` (previously both collapsed into 404).
    let inst = process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?;
    match inst {
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

/// Map event_type to human-friendly category: instance | token | external.
fn event_type_to_category(event_type: &str) -> &'static str {
    match event_type {
        "ProcessStarted" | "ProcessCompleted" => "instance",
        "TokenArrived" | "TokenCompleted" | "TokenFailed" | "UserTaskCreated"
        | "UserTaskCompleted" | "TimerScheduled" | "TimerFired" | "SagaStarted"
        | "SagaCompleted" => "token",
        "ExternalTaskLocked" | "ExternalTaskCompleted" | "ExternalTaskFailed" => "external",
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
    Path(id): Path<String>,
) -> Result<Json<TraceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let process_store = Arc::clone(&state.storage.process_store);
    // E2-4 review fix: storage ERROR → 500 internal; missing instance →
    // 404 not_found.
    let inst = process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "process instance not found: {}",
                    id
                ))),
            )
        })?;
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
    Path(id): Path<String>,
) -> Result<Json<crate::step_trace::StepsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let process_store = Arc::clone(&state.storage.process_store);
    let inst = process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "process instance not found: {}",
                    id
                ))),
            )
        })?;
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
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<TaskListItem>>, (StatusCode, Json<ErrorResponse>)> {
    // Route-level pagination: absent parameters ⇒ no windowing (backward
    // compatible). When present, validation matches the instances list.
    let page = if params.contains_key("limit") || params.contains_key("offset") {
        Some(parse_page_params(&params)?)
    } else {
        None
    };
    let tenant_id = tenant_from_headers(&headers);
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
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "process instance not found: {}",
                    instance_id
                ))),
            )
        })?;
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
    headers: HeaderMap,
    Path(task_id): Path<String>,
    ValidatedJson(body): ValidatedJson<CompleteTaskBodyRequest>,
) -> Result<Json<CompleteTaskResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (instance_id, node_id) = parse_task_id(&task_id)
        .ok_or_else(|| bad_request(format!("invalid task_id: {}", task_id)))?;

    // E2-3: enforce the variable value contract at this write boundary
    // (valid envelopes, valid artifact URIs, size cap) before the event
    // enters the engine. E3-1: `__task_envelope` is engine-owned —
    // user tasks have no envelope field, so the key is simply reserved.
    enforce_reserved_variable_keys(&body.variables)?;
    enforce_variable_contract(&body.variables, &state.variable_limits)?;

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

    let tenant_id = tenant_from_headers(&headers);
    let mut ctx = build_ctx(state.as_ref(), tenant_id);
    let ev = EngineEvent::UserTaskCompleted(payloads::UserTaskCompleted {
        task_id: task_id.clone(),
        instance_id: instance_id.clone(),
        node_id: node_id.clone(),
        variables: body.variables,
    });
    state.engine.run_async(ev, &mut ctx).await;
    info!(
        task_id = %task_id,
        instance_id = %instance_id,
        "user task completed"
    );
    Ok(Json(CompleteTaskResponse {
        status: "COMPLETED".to_string(),
    }))
}

/// POST /api/v1/external-tasks/fetch-and-lock
pub async fn external_task_fetch_and_lock(
    State(state): State<Arc<AppState>>,
    ValidatedJson(body): ValidatedJson<FetchAndLockRequest>,
) -> Result<Json<Vec<ExternalTaskResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let external_task_store = Arc::clone(&state.storage.external_task_store);
    let _ = external_task_store
        .reclaim_expired_locks()
        .await
        .map_err(internal_error_tuple)?;
    let lock_duration = Duration::from_millis(body.lock_duration_ms);
    let max_tasks = body.max_tasks as usize;
    let tasks = external_task_store
        .fetch_and_lock(&body.worker_id, &body.task_types, max_tasks, lock_duration)
        .await
        .map_err(internal_error_tuple)?;
    let occurred_at = occurred_at_now();
    for t in &tasks {
        let payload = serde_json::json!({
            "task_id": t.task_id,
            "token_id": t.token_id,
            "process_instance_id": t.process_instance_id,
            "worker_id": body.worker_id,
            "retries": t.retries,
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
    headers: HeaderMap,
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
    let tenant_id = tenant_from_headers(&headers);
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
        node_id,
        variables,
    });
    state.engine.run_async(ev, &mut ctx).await;
    Ok(Json(CompleteTaskResponse {
        status: "COMPLETED".to_string(),
    }))
}

/// POST /api/v1/external-tasks/:task_id/extend-lock — extend lock duration on a locked task.
pub async fn external_task_extend_lock(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
    ValidatedJson(body): ValidatedJson<ExtendLockRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let external_task_store = Arc::clone(&state.storage.external_task_store);
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
    Path(task_id): Path<String>,
    ValidatedJson(body): ValidatedJson<ExternalTaskFailRequest>,
) -> Result<Json<CompleteTaskResponse>, Response> {
    // E3-1: optional retry category, validated up front. v1 semantics: the
    // category is RECORDED for observability (error message prefix,
    // envelope, history, DLQ) — retry behavior is unchanged; acting on it
    // is E3-2's structured-failure scope.
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
    // v1 fail-path decision (documented in docs/ai-task-envelope.md): the
    // store's fail() has no variables channel, and the store trait is not
    // extended for E3-1 — so the category is persisted as a structured
    // JSON prefix on the error message (`{"retry_category":...}: <error>`,
    // surviving task.error_message, ExternalTaskFailed history and the
    // DLQ), and the envelope is recorded in the ExternalTaskFailed history
    // payload only. The envelope is deliberately NOT embedded in the error
    // message: schemas can be multi-KB and that path bypasses history
    // redaction.
    let error_message = match retry_category {
        Some(category) => format!(
            "{}: {}",
            serde_json::json!({ "retry_category": category }),
            body.error
        ),
        None => body.error.clone(),
    };
    let external_task_store = Arc::clone(&state.storage.external_task_store);
    let retry_after = body.retry_after_ms.map(Duration::from_millis);
    external_task_store
        .fail(
            &task_id,
            &body.worker_id,
            error_message.clone(),
            retry_after,
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
        // Insert into dead letter queue (input variables only — the
        // fail-path envelope is in the ExternalTaskFailed history event).
        let dl_entry = bpm_engine_storage::DeadLetterEntry {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: task.task_id.clone(),
            token_id: task.token_id.clone(),
            process_instance_id: task.process_instance_id.clone(),
            task_type: task.task_type.clone(),
            error_message: error_message.clone(),
            variables: serde_json::to_string(&task.variables).unwrap_or_default(),
            tenant_id: None,
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
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<ReplayCreateResponse>), (StatusCode, Json<ErrorResponse>)> {
    // E2-4 review fix: storage ERROR → 500 internal; only a missing
    // instance is 404 not_found.
    let _inst = state
        .storage
        .process_store
        .load(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(format!(
                    "process instance not found: {}",
                    id
                ))),
            )
        })?;
    let events =
        HistoryRepo::list_by_instance(state.storage.history_repo.as_ref(), &id, None, None)
            .await
            .map_err(internal_error_tuple)?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let session = ReplaySession::new(id.clone(), events);
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

/// POST /api/v1/replay/:session_id/step — step forward one event.
pub async fn replay_step(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<ReplayStepResponse>, (StatusCode, Json<ErrorResponse>)> {
    let (ev_clone, snapshot_clone) = {
        let guard = state.replay_sessions.read().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::internal("internal lock error")),
            )
        })?;
        let session = guard.get(&session_id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(
                    "replay session not found or expired",
                )),
            )
        })?;
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
        let session = guard.get(&session_id).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found(
                    "replay session not found or expired",
                )),
            )
        })?;
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
    Path(session_id): Path<String>,
) -> Result<Json<ReplaySnapshotResponse>, (StatusCode, Json<ErrorResponse>)> {
    let guard = state.replay_sessions.read().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::internal("internal lock error")),
        )
    })?;
    let session = guard.get(&session_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::not_found(
                "replay session not found or expired",
            )),
        )
    })?;
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
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let mut guard = state.replay_sessions.write().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::internal("internal lock error")),
        )
    })?;
    guard.remove(&session_id);
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/v1/process-definitions/deploy — deploy a process definition from BPMN 2.0 XML.
/// On compile failure returns 400 with list of CompilerErrors (03.md).
pub async fn deploy_bpmn(
    State(state): State<Arc<AppState>>,
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
pub async fn activate_definition(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ActivateResponse>, (StatusCode, Json<ErrorResponse>)> {
    state
        .storage
        .def_store
        .activate(&id)
        .await
        .map_err(internal_error_tuple)?;
    info!(definition_id = %id, "process definition activated");
    Ok(Json(ActivateResponse {
        id,
        status: "active".to_string(),
    }))
}

/// PUT /api/v1/process-definitions/:id/deprecate — mark a version as deprecated.
pub async fn deprecate_definition(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ActivateResponse>, (StatusCode, Json<ErrorResponse>)> {
    state
        .storage
        .def_store
        .deprecate(&id)
        .await
        .map_err(internal_error_tuple)?;
    info!(definition_id = %id, "process definition deprecated");
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
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let page = if params.contains_key("limit") || params.contains_key("offset") {
        Some(parse_page_params(&params)?)
    } else {
        None
    };
    let tenant_id = tenant_from_headers(&headers);
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
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let entry = state
        .storage
        .dead_letter_store
        .get(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found("dead letter not found")),
            )
        })?;
    Ok(Json(serde_json::json!({ "entry": entry })))
}

/// POST /api/v1/dead-letters/:id/requeue — requeue a dead letter entry back as an external task.
async fn requeue_dead_letter(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let task_id = state
        .storage
        .dead_letter_store
        .requeue(&id)
        .await
        .map_err(internal_error_tuple)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse::not_found("dead letter not found")),
            )
        })?;
    Ok(Json(
        serde_json::json!({ "status": "requeued", "task_id": task_id }),
    ))
}

/// DELETE /api/v1/dead-letters/:id — delete a dead letter entry.
async fn delete_dead_letter(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    state
        .storage
        .dead_letter_store
        .delete(&id)
        .await
        .map_err(internal_error_tuple)?;
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

    // Add /metrics endpoint when observability feature is enabled
    #[cfg(feature = "observability")]
    let router = root_route!(router, GET, "/metrics", super::metrics::metrics_handler);

    router
}

/// The versioned `/api/v1` routes, with auth and rate limiting applied
/// exactly as before (same layers, same order — the layers wrap the
/// nested router, so they cover the API routes only, while the
/// request-id middleware on the outer router covers everything).
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
        .layer(axum_mw::from_fn(middleware::api_key_auth))
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
}
