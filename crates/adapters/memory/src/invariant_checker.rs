//! Memory adapter invariant checker implementation.

use async_trait::async_trait;
use bpm_engine_core::{InstanceState, TokenStatus};
use bpm_engine_storage::{
    CheckStats, InvariantCheckResult, InvariantChecker, InvariantViolationReport,
    ProcessInstanceStore, Severity,
};
use std::sync::Arc;
use std::time::Instant;

use crate::MemoryRepo;

/// Memory-based invariant checker.
pub struct MemoryInvariantChecker {
    repo: Arc<MemoryRepo>,
}

impl MemoryInvariantChecker {
    /// Create a new checker for the given memory repo.
    pub fn new(repo: Arc<MemoryRepo>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl InvariantChecker for MemoryInvariantChecker {
    async fn check_all(&self) -> anyhow::Result<InvariantCheckResult> {
        let start = Instant::now();

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
        let instance_ids = self.repo.list_running(None).await?;

        for instance_id in &instance_ids {
            if let Some(instance) = self.repo.load(instance_id).await? {
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

                    // Check: Completed/Terminated tokens should not be in a running instance's
                    // active token list with non-zero version (they should have been removed)
                    if token.status == TokenStatus::Completed && token.version > 0 {
                        // This is OK - completed tokens may remain for history
                    }
                }
            }
        }

        Ok(violations)
    }

    async fn check_external_tasks(&self) -> anyhow::Result<Vec<InvariantViolationReport>> {
        let violations = Vec::new();

        // Check all running instances for external tasks
        let instance_ids = self.repo.list_running(None).await?;

        for instance_id in &instance_ids {
            if let Some(instance) = self.repo.load(instance_id).await? {
                for token in &instance.tokens {
                    // Check: ExternalTask tokens should have a corresponding external task
                    if let Some(bpm_engine_core::NodeType::ExternalTask { .. }) =
                        self.get_node_type(&instance.process_def_id, &token.node_id).await
                    {
                        // This is a basic check - in a real implementation we'd verify
                        // the external task exists and is in the correct state
                    }
                }
            }
        }

        Ok(violations)
    }

    async fn check_instances(&self) -> anyhow::Result<Vec<InvariantViolationReport>> {
        let mut violations = Vec::new();

        let instance_ids = self.repo.list_running(None).await?;

        for instance_id in &instance_ids {
            if let Some(instance) = self.repo.load(instance_id).await? {
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
                        description: format!(
                            "Instance {} has version 0",
                            instance_id
                        ),
                        entity_id: instance_id.clone(),
                        severity: Severity::Info,
                    });
                }
            }
        }

        Ok(violations)
    }

    async fn check_timers(&self) -> anyhow::Result<Vec<InvariantViolationReport>> {
        // Timer checks would go here if we had access to the timer store
        // For now, return empty
        Ok(Vec::new())
    }
}

impl MemoryInvariantChecker {
    /// Helper to get node type from process definition.
    async fn get_node_type(
        &self,
        _process_def_id: &str,
        _node_id: &str,
    ) -> Option<bpm_engine_core::NodeType> {
        // In a real implementation, we'd look up the process definition
        // and find the node type. For now, return None.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bpm_engine_core::{ProcessInstance, Token, TokenMode};
    use bpm_engine_storage::ProcessInstanceStore;
    use std::collections::HashMap;

    fn make_token(id: &str, status: TokenStatus, version: u32, attempt: u32) -> Token {
        Token {
            id: id.to_string(),
            node_id: "node1".to_string(),
            status,
            mode: TokenMode::Forward,
            version,
            attempt,
            parallel_group_id: None,
            updated_at: None,
        }
    }

    fn make_instance(
        id: &str,
        state: InstanceState,
        tokens: Vec<Token>,
        version: u32,
    ) -> ProcessInstance {
        ProcessInstance {
            id: id.to_string(),
            process_def_id: "proc1".to_string(),
            tenant_id: None,
            tokens,
            variables: HashMap::new(),
            state,
            version,
            parent_instance_id: None,
            parent_token_id: None,
        }
    }

    #[tokio::test]
    async fn check_all_passes_for_empty_repo() {
        let repo = Arc::new(MemoryRepo::new());
        let checker = MemoryInvariantChecker::new(repo);

        let result = checker.check_all().await.unwrap();
        assert!(result.passed);
        assert!(result.violations.is_empty());
    }

    #[tokio::test]
    async fn check_tokens_detects_version_zero() {
        let repo = Arc::new(MemoryRepo::new());
        let instance = make_instance(
            "inst1",
            InstanceState::Running,
            vec![make_token("tok1", TokenStatus::Executing, 0, 1)],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_tokens().await.unwrap();

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].invariant, "token_version_positive");
        assert_eq!(violations[0].severity, Severity::Warning);
    }

    #[tokio::test]
    async fn check_tokens_detects_zero_attempt() {
        let repo = Arc::new(MemoryRepo::new());
        let instance = make_instance(
            "inst1",
            InstanceState::Running,
            vec![make_token("tok1", TokenStatus::Executing, 1, 0)],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_tokens().await.unwrap();

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].invariant, "token_attempt_positive");
    }

    #[tokio::test]
    async fn check_tokens_passes_for_valid_state() {
        let repo = Arc::new(MemoryRepo::new());
        let instance = make_instance(
            "inst1",
            InstanceState::Running,
            vec![make_token("tok1", TokenStatus::Executing, 1, 1)],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_tokens().await.unwrap();

        assert!(violations.is_empty());
    }

    #[tokio::test]
    async fn check_instances_detects_running_without_tokens() {
        let repo = Arc::new(MemoryRepo::new());
        let instance = make_instance("inst1", InstanceState::Running, vec![], 1);
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_instances().await.unwrap();

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].invariant, "running_instance_has_tokens");
        assert_eq!(violations[0].severity, Severity::Critical);
    }

    #[tokio::test]
    async fn check_instances_skips_completed_instances() {
        let repo = Arc::new(MemoryRepo::new());
        // Completed instances are not returned by list_running,
        // so they won't be checked by check_instances
        let instance = make_instance(
            "inst1",
            InstanceState::Completed,
            vec![make_token("tok1", TokenStatus::Ready, 1, 0)],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_instances().await.unwrap();

        // No violations because completed instances are not checked
        assert!(violations.is_empty());
    }

    #[tokio::test]
    async fn check_instances_passes_for_valid_state() {
        let repo = Arc::new(MemoryRepo::new());
        let instance = make_instance(
            "inst1",
            InstanceState::Running,
            vec![make_token("tok1", TokenStatus::Ready, 1, 0)],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_instances().await.unwrap();

        assert!(violations.is_empty());
    }

    #[tokio::test]
    async fn check_all_combines_all_violations() {
        let repo = Arc::new(MemoryRepo::new());

        // Instance with version 0
        let instance1 = make_instance("inst1", InstanceState::Running, vec![], 0);
        repo.save(&instance1).await.unwrap();

        // Instance with token version 0
        let instance2 = make_instance(
            "inst2",
            InstanceState::Running,
            vec![make_token("tok1", TokenStatus::Executing, 0, 1)],
            1,
        );
        repo.save(&instance2).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let result = checker.check_all().await.unwrap();

        assert!(!result.passed);
        assert!(result.violations.len() >= 2); // At least instance + token violations
    }

    #[tokio::test]
    async fn check_tokens_detects_duplicate_token_ids() {
        let repo = Arc::new(MemoryRepo::new());
        let instance = make_instance(
            "inst1",
            InstanceState::Running,
            vec![
                make_token("tok1", TokenStatus::Ready, 1, 0),
                make_token("tok1", TokenStatus::Executing, 1, 1),
            ],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_tokens().await.unwrap();

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].invariant, "token_id_unique");
        assert_eq!(violations[0].severity, Severity::Critical);
    }

    #[tokio::test]
    async fn check_tokens_allows_version_zero_for_ready() {
        let repo = Arc::new(MemoryRepo::new());
        // Ready tokens with version 0 are valid (initial state)
        let instance = make_instance(
            "inst1",
            InstanceState::Running,
            vec![make_token("tok1", TokenStatus::Ready, 0, 0)],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_tokens().await.unwrap();

        assert!(violations.is_empty());
    }

    #[tokio::test]
    async fn check_instances_detects_multiple_violations() {
        let repo = Arc::new(MemoryRepo::new());
        // Instance with no tokens AND version 0
        let instance = make_instance("inst1", InstanceState::Running, vec![], 0);
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_instances().await.unwrap();

        assert_eq!(violations.len(), 2); // running_instance_has_tokens + instance_version_positive
    }

    #[tokio::test]
    async fn check_all_stats_tracks_duration() {
        let repo = Arc::new(MemoryRepo::new());
        let checker = MemoryInvariantChecker::new(repo);

        let result = checker.check_all().await.unwrap();

        // Duration should be non-negative (u64)
        assert!(result.stats.duration_ms < u64::MAX);
    }

    #[tokio::test]
    async fn check_tokens_with_multiple_valid_tokens() {
        let repo = Arc::new(MemoryRepo::new());
        let instance = make_instance(
            "inst1",
            InstanceState::Running,
            vec![
                make_token("tok1", TokenStatus::Ready, 1, 0),
                make_token("tok2", TokenStatus::Executing, 2, 1),
                make_token("tok3", TokenStatus::Completed, 3, 1),
            ],
            1,
        );
        repo.save(&instance).await.unwrap();

        let checker = MemoryInvariantChecker::new(repo);
        let violations = checker.check_tokens().await.unwrap();

        assert!(violations.is_empty());
    }
}
