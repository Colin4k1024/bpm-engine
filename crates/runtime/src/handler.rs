//! EventHandler trait and EngineContext — the dependency injection boundary for the engine.

use async_trait::async_trait;
use bpm_engine_core::{EngineEvent, HistoryRedactionPolicy};
use bpm_engine_storage::{
    ApprovalStore, CompensationRecordRepo, ExternalTaskStore, HistoryRepo, OutboxRepo,
    ParallelJoinRepo, ProcessDefinitionStore, ProcessInstanceStore, TimerStore, TokenStore,
};
use std::collections::HashSet;
use std::sync::Arc;

/// Trait implemented by each engine handler (token arrival, process start, etc.).
///
/// Handlers are deterministic and side-effect free beyond storage writes through `ctx`.
/// They receive an event and return zero or more follow-up events for the pump to process.
#[async_trait]
pub trait EventHandler: Send + Sync {
    /// Handle an event and return zero or more follow-up events for the pump to process.
    async fn handle(&self, event: &EngineEvent, ctx: &mut EngineContext) -> Vec<EngineEvent>;
}

/// Execution context providing access to all storage backends.
///
/// Constructed via [`EngineContext::builder`]. Three stores are required (process instances,
/// tokens, process definitions); all others are optional and enable specific BPMN features
/// (timers, external tasks, compensation, history, parallel joins, outbox, approvals).
pub struct EngineContext {
    /// Process instance persistence.
    pub process_store: Arc<dyn ProcessInstanceStore>,
    /// Token persistence with optimistic concurrency.
    pub token_store: Arc<dyn TokenStore>,
    /// Process definition storage (BPMN XML + compiled graph).
    pub process_def_store: Arc<dyn ProcessDefinitionStore>,
    /// Parallel join coordination (optional — enables fork/join).
    pub parallel_join_repo: Option<Arc<dyn ParallelJoinRepo>>,
    /// Persistent timer storage (optional — enables timer events).
    pub timer_store: Option<Arc<dyn TimerStore>>,
    /// Saga compensation record storage (optional — enables compensation).
    pub compensation_repo: Option<Arc<dyn CompensationRecordRepo>>,
    /// Transactional outbox (optional — enables reliable event publishing).
    pub outbox_repo: Option<Arc<dyn OutboxRepo>>,
    /// External task storage (optional — enables worker protocol).
    pub external_task_store: Option<Arc<dyn ExternalTaskStore>>,
    /// Execution history recording (optional — enables audit/replay).
    pub history_repo: Option<Arc<dyn HistoryRepo>>,
    /// Engine-owned approval records (optional — enables the E5-2 approval
    /// gate on approval-required external task nodes).
    pub approval_store: Option<Arc<dyn ApprovalStore>>,
    /// Task types whose external tasks require approval before creation
    /// (E5-2, `engine.approval_required_task_types`). The gate only runs
    /// when [`EngineContext::approval_store`] is wired; an empty set (the
    /// default) keeps the pre-E5-2 behavior byte-identical.
    pub approval_required_task_types: Arc<HashSet<String>>,
    /// Redaction policy applied to variable values written into event
    /// history (E2-3). Defaults to the 1024-byte cap — redaction is ON by
    /// default, so oversized values never land in history.
    pub history_redaction_policy: HistoryRedactionPolicy,
    /// Tenant isolation key for multi-tenant deployments.
    pub tenant_id: Option<String>,
}

impl EngineContext {
    /// Create a builder with the three required stores.
    pub fn builder(
        process_store: Arc<dyn ProcessInstanceStore>,
        token_store: Arc<dyn TokenStore>,
        process_def_store: Arc<dyn ProcessDefinitionStore>,
    ) -> EngineContextBuilder {
        EngineContextBuilder {
            process_store,
            token_store,
            process_def_store,
            parallel_join_repo: None,
            timer_store: None,
            compensation_repo: None,
            outbox_repo: None,
            external_task_store: None,
            history_repo: None,
            approval_store: None,
            approval_required_task_types: Arc::new(HashSet::new()),
            history_redaction_policy: HistoryRedactionPolicy::default(),
            tenant_id: None,
        }
    }
}

/// Builder for [`EngineContext`] — use method chaining to wire optional stores.
pub struct EngineContextBuilder {
    process_store: Arc<dyn ProcessInstanceStore>,
    token_store: Arc<dyn TokenStore>,
    process_def_store: Arc<dyn ProcessDefinitionStore>,
    parallel_join_repo: Option<Arc<dyn ParallelJoinRepo>>,
    timer_store: Option<Arc<dyn TimerStore>>,
    compensation_repo: Option<Arc<dyn CompensationRecordRepo>>,
    outbox_repo: Option<Arc<dyn OutboxRepo>>,
    external_task_store: Option<Arc<dyn ExternalTaskStore>>,
    history_repo: Option<Arc<dyn HistoryRepo>>,
    approval_store: Option<Arc<dyn ApprovalStore>>,
    approval_required_task_types: Arc<HashSet<String>>,
    history_redaction_policy: HistoryRedactionPolicy,
    tenant_id: Option<String>,
}

impl EngineContextBuilder {
    /// Wire the parallel join repository (enables fork/join).
    pub fn parallel_join_repo(mut self, repo: Arc<dyn ParallelJoinRepo>) -> Self {
        self.parallel_join_repo = Some(repo);
        self
    }

    /// Wire the timer store (enables timer events).
    pub fn timer_store(mut self, store: Arc<dyn TimerStore>) -> Self {
        self.timer_store = Some(store);
        self
    }

    /// Wire the compensation repository (enables saga compensation).
    pub fn compensation_repo(mut self, repo: Arc<dyn CompensationRecordRepo>) -> Self {
        self.compensation_repo = Some(repo);
        self
    }

    /// Wire the outbox repository (enables reliable event publishing).
    pub fn outbox_repo(mut self, repo: Arc<dyn OutboxRepo>) -> Self {
        self.outbox_repo = Some(repo);
        self
    }

    /// Wire the external task store (enables worker protocol).
    pub fn external_task_store(mut self, store: Arc<dyn ExternalTaskStore>) -> Self {
        self.external_task_store = Some(store);
        self
    }

    /// Wire the history repository (enables audit/replay).
    pub fn history_repo(mut self, repo: Arc<dyn HistoryRepo>) -> Self {
        self.history_repo = Some(repo);
        self
    }

    /// Wire the approval store (enables the E5-2 approval gate).
    pub fn approval_store(mut self, store: Arc<dyn ApprovalStore>) -> Self {
        self.approval_store = Some(store);
        self
    }

    /// Set the approval-required task types (E5-2): external tasks of these
    /// types are created only after an operator approves the persisted
    /// approval record. Defaults to the empty set (no gate).
    pub fn approval_required_task_types(mut self, types: Arc<HashSet<String>>) -> Self {
        self.approval_required_task_types = types;
        self
    }

    /// Set the redaction policy for variable values written into event
    /// history. Unset, the policy defaults to the 1024-byte cap
    /// (`HistoryRedactionPolicy::default()`), i.e. redaction is ON.
    pub fn history_redaction_policy(mut self, policy: HistoryRedactionPolicy) -> Self {
        self.history_redaction_policy = policy;
        self
    }

    /// Set the tenant isolation key.
    pub fn tenant_id(mut self, id: String) -> Self {
        self.tenant_id = Some(id);
        self
    }

    /// Build the [`EngineContext`]. Consumes the builder.
    pub fn build(self) -> EngineContext {
        EngineContext {
            process_store: self.process_store,
            token_store: self.token_store,
            process_def_store: self.process_def_store,
            parallel_join_repo: self.parallel_join_repo,
            timer_store: self.timer_store,
            compensation_repo: self.compensation_repo,
            outbox_repo: self.outbox_repo,
            external_task_store: self.external_task_store,
            history_repo: self.history_repo,
            approval_store: self.approval_store,
            approval_required_task_types: self.approval_required_task_types,
            history_redaction_policy: self.history_redaction_policy,
            tenant_id: self.tenant_id,
        }
    }
}
