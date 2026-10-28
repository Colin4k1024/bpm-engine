//! Invariant checker: verifies storage layer consistency.
//!
//! Use after crash recovery, during debugging, or as a health check.

use async_trait::async_trait;
use bpm_engine_core::{InstanceState, TokenStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::ProcessInstanceStore;

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

/// Generic invariant checker that works over any [`ProcessInstanceStore`].
///
/// Ports the memory adapter checker logic: it only relies on `list_running`
/// and `load`, so the same checks run against both the memory and the
/// PostgreSQL backends.
pub struct StoreInvariantChecker {
    process_store: Arc<dyn ProcessInstanceStore>,
}

impl StoreInvariantChecker {
    /// Create a checker over the given process instance store.
    pub fn new(process_store: Arc<dyn ProcessInstanceStore>) -> Self {
        Self { process_store }
    }
}

#[async_trait]
impl InvariantChecker for StoreInvariantChecker {
    async fn check_all(&self) -> anyhow::Result<InvariantCheckResult> {
        let start = std::time::Instant::now();

        let mut violations = Vec::new();
        let mut stats = CheckStats {
            instances_checked: 0,
            tokens_checked: 0,
            external_tasks_checked: 0,
            timers_checked: 0,
            duration_ms: 0,
        };

        // Check tokens
        let token_violations = self.check_tokens().await?;
        violations.extend(token_violations);

        // Check external tasks
        let task_violations = self.check_external_tasks().await?;
        violations.extend(task_violations);

        // Check instances
        let instance_violations = self.check_instances().await?;
        violations.extend(instance_violations);

        // Check timers
        let timer_violations = self.check_timers().await?;
        violations.extend(timer_violations);

        stats.duration_ms = start.elapsed().as_millis() as u64;

        Ok(InvariantCheckResult {
            passed: violations.is_empty(),
            violations,
            stats,
        })
    }

    async fn check_tokens(&self) -> anyhow::Result<Vec<InvariantViolationReport>> {
        let mut violations = Vec::new();

        // Get all running instances
        let instance_ids = self.process_store.list_running(None).await?;

        for instance_id in &instance_ids {
            if let Some(instance) = self.process_store.load(instance_id).await? {
                // Check for duplicate token IDs within the instance
                let mut seen_ids = std::collections::HashSet::new();
                for token in &instance.tokens {
                    if !seen_ids.insert(&token.id) {
                        violations.push(InvariantViolationReport {
                            invariant: "token_id_unique".to_string(),
                            description: format!(
                                "Duplicate token ID {} in instance {}",
                                token.id, instance_id
                            ),
                            entity_id: token.id.clone(),
                            severity: Severity::Critical,
                        });
                    }
                }

                for token in &instance.tokens {
                    // Check: Token version should be positive for non-Ready tokens
                    if token.version == 0 && token.status != TokenStatus::Ready {
                        violations.push(InvariantViolationReport {
                            invariant: "token_version_positive".to_string(),
                            description: format!(
                                "Token {} has version 0 but status {:?}",
                                token.id, token.status
                            ),
                            entity_id: token.id.clone(),
                            severity: Severity::Warning,
                        });
                    }

                    // Check: Executing tokens should have attempt > 0
                    if token.status == TokenStatus::Executing && token.attempt == 0 {
                        violations.push(InvariantViolationReport {
                            invariant: "token_attempt_positive".to_string(),
                            description: format!(
                                "Token {} is Executing but has attempt 0",
                                token.id
                            ),
                            entity_id: token.id.clone(),
                            severity: Severity::Warning,
                        });
                    }
                }
            }
        }

        Ok(violations)
    }

    async fn check_external_tasks(&self) -> anyhow::Result<Vec<InvariantViolationReport>> {
        // External task checks would require the external task store and the
        // process definition store; for now, return empty.
        Ok(Vec::new())
    }

    async fn check_instances(&self) -> anyhow::Result<Vec<InvariantViolationReport>> {
        let mut violations = Vec::new();

        let instance_ids = self.process_store.list_running(None).await?;

        for instance_id in &instance_ids {
            if let Some(instance) = self.process_store.load(instance_id).await? {
                // Check: Running instances should have at least one token
                if instance.state == InstanceState::Running && instance.tokens.is_empty() {
                    violations.push(InvariantViolationReport {
                        invariant: "running_instance_has_tokens".to_string(),
                        description: format!(
                            "Instance {} is Running but has no tokens",
                            instance_id
                        ),
                        entity_id: instance_id.clone(),
                        severity: Severity::Critical,
                    });
                }

                // Check: Completed instances should have no active tokens
                if instance.state == InstanceState::Completed {
                    let has_active = instance.tokens.iter().any(|t| {
                        t.status == TokenStatus::Ready || t.status == TokenStatus::Executing
                    });
                    if has_active {
                        violations.push(InvariantViolationReport {
                            invariant: "completed_instance_no_active_tokens".to_string(),
                            description: format!(
                                "Instance {} is Completed but has active tokens",
                                instance_id
                            ),
                            entity_id: instance_id.clone(),
                            severity: Severity::Critical,
                        });
                    }
                }

                // Check: Terminated instances should have no active tokens
                if instance.state == InstanceState::Terminated {
                    let has_active = instance.tokens.iter().any(|t| {
                        t.status == TokenStatus::Ready || t.status == TokenStatus::Executing
                    });
                    if has_active {
                        violations.push(InvariantViolationReport {
                            invariant: "terminated_instance_no_active_tokens".to_string(),
                            description: format!(
                                "Instance {} is Terminated but has active tokens",
                                instance_id
                            ),
                            entity_id: instance_id.clone(),
                            severity: Severity::Critical,
                        });
                    }
                }

                // Check: Version should be positive
                if instance.version == 0 {
                    violations.push(InvariantViolationReport {
                        invariant: "instance_version_positive".to_string(),
                        description: format!("Instance {} has version 0", instance_id),
                        entity_id: instance_id.clone(),
                        severity: Severity::Info,
                    });
                }
            }
        }

        Ok(violations)
    }

    async fn check_timers(&self) -> anyhow::Result<Vec<InvariantViolationReport>> {
        // Timer checks would require the timer store; for now, return empty.
        Ok(Vec::new())
    }
}
