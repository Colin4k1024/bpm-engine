//! Shared app state for REST API.

use crate::replay::ReplaySessions;
use bpm_engine_adapter_memory::{MemoryInvariantChecker, MemoryRepo, ProcessDefStore};
use bpm_engine_runtime::BpmEngine;
use bpm_engine_storage::DeadLetterStore;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Extra health check callback for readiness probe.
///
/// Implementations return (name, status) pairs to be merged into
/// the `/ready` response. This allows external adapters (e.g. postgres)
/// to inject pool health checks without creating a direct dependency.
pub type ExtraHealthChecks =
    Arc<dyn Fn() -> std::collections::HashMap<String, String> + Send + Sync>;

/// Shared app state. Pass Arc<AppState> to router.
pub struct AppState {
    pub engine: BpmEngine,
    pub repo: Arc<MemoryRepo>,
    pub def_store: Arc<ProcessDefStore>,
    pub dead_letter_store: Arc<dyn DeadLetterStore>,
    /// Invariant checker for storage consistency verification.
    pub invariant_checker: MemoryInvariantChecker,
    /// Ephemeral replay sessions (session_id -> ReplaySession). Not persisted.
    pub replay_sessions: Arc<ReplaySessions>,
    /// Cancellation token for the timer scheduler. Used during graceful shutdown.
    #[allow(dead_code)]
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
