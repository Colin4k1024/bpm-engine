//! Invariant checker: verifies storage layer consistency.
//!
//! Use after crash recovery, during debugging, or as a health check.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of an invariant check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantCheckResult {
    /// Whether all invariants passed.
    pub passed: bool,
    /// List of violations found.
    pub violations: Vec<InvariantViolationReport>,
    /// Summary statistics.
    pub stats: CheckStats,
}

/// A single invariant violation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvariantViolationReport {
    /// Which invariant was violated.
    pub invariant: String,
    /// Description of the violation.
    pub description: String,
    /// Affected entity ID (instance, token, task, etc.).
    pub entity_id: String,
    /// Severity level.
    pub severity: Severity,
}

/// Severity of an invariant violation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Severity {
    /// Data corruption that must be fixed.
    Critical,
    /// Inconsistency that may cause issues.
    Warning,
    /// Informational finding.
    Info,
}

/// Statistics from an invariant check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckStats {
    /// Number of process instances checked.
    pub instances_checked: usize,
    /// Number of tokens checked.
    pub tokens_checked: usize,
    /// Number of external tasks checked.
    pub external_tasks_checked: usize,
    /// Number of timers checked.
    pub timers_checked: usize,
    /// Duration of the check in milliseconds.
    pub duration_ms: u64,
}

/// Trait for invariant checking against a storage backend.
#[async_trait]
pub trait InvariantChecker: Send + Sync {
    /// Run all invariant checks and return the result.
    async fn check_all(&self) -> anyhow::Result<InvariantCheckResult>;

    /// Check token invariants only.
    async fn check_tokens(&self) -> anyhow::Result<Vec<InvariantViolationReport>>;

    /// Check external task invariants only.
    async fn check_external_tasks(&self) -> anyhow::Result<Vec<InvariantViolationReport>>;

    /// Check process instance invariants only.
    async fn check_instances(&self) -> anyhow::Result<Vec<InvariantViolationReport>>;

    /// Check timer invariants only.
    async fn check_timers(&self) -> anyhow::Result<Vec<InvariantViolationReport>>;
}
