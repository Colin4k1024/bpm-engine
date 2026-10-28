//! Shared app state for REST API.

use crate::replay::ReplaySessions;
use crate::storage::Storage;
use bpm_engine_core::HistoryRedactionPolicy;
use bpm_engine_runtime::BpmEngine;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Extra health check callback for readiness probe.
///
/// Implementations return (name, status) pairs to be merged into
/// the `/ready` response. This allows external adapters (e.g. postgres)
/// to inject pool health checks without creating a direct dependency.
pub type ExtraHealthChecks =
    Arc<dyn Fn() -> std::collections::HashMap<String, String> + Send + Sync>;

/// Variable size limits enforced at the REST write boundaries (E2-3).
///
/// Derived from `engine.variable_max_bytes` and
/// `engine.history_variable_max_bytes` configuration.
#[derive(Debug, Clone, Copy)]
pub struct VariableContractLimits {
    /// Maximum byte length of a stored variable value (final stored string,
    /// envelope included). Enforced at start-instance, user-task complete,
    /// and external-task complete.
    pub variable_max_bytes: usize,
    /// Maximum byte length of a variable value kept verbatim in event
    /// history (drives the redaction policy wired into EngineContext).
    pub history_variable_max_bytes: usize,
}

impl From<&crate::bpm_config::EngineConfig> for VariableContractLimits {
    fn from(engine: &crate::bpm_config::EngineConfig) -> Self {
        Self {
            variable_max_bytes: engine.variable_max_bytes,
            history_variable_max_bytes: engine.history_variable_max_bytes,
        }
    }
}

/// Shared app state. Pass Arc<AppState> to router.
pub struct AppState {
    pub engine: BpmEngine,
    /// Storage bundle: trait objects for every store, memory or Postgres
    /// depending on `database.driver` configuration.
    pub storage: Storage,
    /// Variable contract limits enforced at the REST write boundaries (E2-3).
    pub variable_limits: VariableContractLimits,
    /// Redaction policy applied to variable values written into event
    /// history (built from `engine.history_variable_max_bytes`).
    pub history_redaction_policy: HistoryRedactionPolicy,
    /// Task types whose external tasks are approval-required (E5-2,
    /// `engine.approval_required_task_types`): the engine arrival gate
    /// creates a persisted approval record and defers task creation
    /// until an operator approves (ADR-008).
    pub approval_required_task_types: Arc<std::collections::HashSet<String>>,
    /// Serializes the instance-level lifecycle operations pause/resume/
    /// cancel (E5-2): their check-then-act windows run under this
    /// process-wide async mutex, so concurrent lifecycle calls on the same
    /// instance cannot interleave — only-once within the single service.
    /// (Multi-replica deployment needs DB-level conditional updates;
    /// out of scope for v1, see ADR-008 §7.)
    pub instance_lifecycle_lock: Arc<tokio::sync::Mutex<()>>,
    /// Ephemeral replay sessions (session_id -> ReplaySession). Not persisted.
    pub replay_sessions: Arc<ReplaySessions>,
    /// Cancellation token for the timer scheduler and timer event drain task.
    /// Used during graceful shutdown.
    pub timer_cancel: CancellationToken,
    /// Optional extra health checks (e.g. database pool status).
    pub extra_health_checks: Option<ExtraHealthChecks>,
    /// Prometheus metrics render function (only available with `observability` feature).
    #[cfg(feature = "observability")]
    pub metrics_render: crate::metrics::MetricsRenderer,
}

/// Health response for liveness probe.
#[derive(serde::Serialize)]
pub struct HealthResponse {
    pub status: String,
}

/// Readiness response with per-component checks.
#[derive(serde::Serialize)]
pub struct ReadinessResponse {
    pub status: String,
    pub checks: std::collections::HashMap<String, String>,
}
